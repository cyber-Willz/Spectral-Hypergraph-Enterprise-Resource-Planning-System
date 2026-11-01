//! CIR-GNN engine: training on the weekly tensor, history replay (recurrent memory), multi-horizon
//! prediction, free-running simulation for the digital twin, and persistence of the model artifact.
use chrono::NaiveDate;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    analytics::{self, mean_sd, phi},
    cirgnn::{evaluate, fit, ChannelEval, CirGnn, Dataset, FitConfig, FitReport, Session},
    data::{Context, CHANNELS, NCH},
    graph::{FlowMix, Kind, Node},
};
use crate::supply_chain::error::{AppError, AppResult};

pub const HORIZONS: [usize; 3] = [1, 2, 4];
pub const LATE_DAYS: f64 = 2.0;

#[derive(Clone, Serialize, Deserialize)]
pub struct Trained {
    pub gnn: CirGnn,
    pub nodes: Vec<Node>,
    pub mean: Vec<f64>,
    pub sd: Vec<f64>,
    pub horizons: Vec<usize>,
    pub channel_eval: Vec<ChannelEval>,
    pub fingerprint: String,
    pub as_of: NaiveDate,
    pub weeks: usize,
    pub holdout_weeks: usize,
}

impl Trained {
    /// A channel's forecasts are trusted only if the model beats both "same as last week" and "node mean"
    /// on weeks it never trained on.
    pub fn channel_ok(&self, ch: usize) -> bool {
        let e = &self.channel_eval[ch];
        self.holdout_weeks > 0 && e.n > 0 && e.skill_vs_persistence > 0.0 && e.skill_vs_mean > 0.0
    }
}

pub fn to_raw(ch: usize, z: f64, mean: f64, sd: f64) -> f64 {
    let v = z * sd + mean;
    match ch {
        0 => v.exp_m1().max(0.0),
        1 => v.clamp(-30.0, 60.0),
        2 => v.clamp(0.0, 100.0),
        _ => v.max(0.0),
    }
}
pub fn to_z(ch: usize, raw: f64, mean: f64, sd: f64) -> f64 {
    let v = if ch == 0 { raw.max(0.0).ln_1p() } else { raw };
    (v - mean) / sd
}

fn norm_stats(ctx: &Context, upto: usize) -> (Vec<f64>, Vec<f64>) {
    let r = ctx.graph.r();
    let (mut mean, mut sd) = (vec![0.0; r * NCH], vec![1.0; r * NCH]);
    for i in 0..r {
        for c in 0..NCH {
            let xs: Vec<f64> = (0..upto).filter(|t| ctx.obs[(t * r + i) * NCH + c] > 0.0).map(|t| ctx.values[(t * r + i) * NCH + c]).collect();
            if xs.len() >= 3 {
                let (m, s) = mean_sd(&xs);
                mean[i * NCH + c] = m;
                sd[i * NCH + c] = if s > 1e-6 { s } else { 1.0 };
            }
        }
    }
    (mean, sd)
}

pub fn dataset(ctx: &Context, mean: &[f64], sd: &[f64]) -> Dataset {
    let r = ctx.graph.r();
    let mut z = vec![0.0; ctx.weeks * r * NCH];
    for t in 0..ctx.weeks {
        for i in 0..r {
            for c in 0..NCH {
                let k = (t * r + i) * NCH + c;
                if ctx.obs[k] > 0.0 {
                    z[k] = ((ctx.values[k] - mean[i * NCH + c]) / sd[i * NCH + c]).clamp(-8.0, 8.0);
                }
            }
        }
    }
    Dataset { t: ctx.weeks, r, nch: NCH, horizons: HORIZONS.to_vec(), z, obs: ctx.obs.clone() }
}

pub fn train(ctx: &Context, cfg: &FitConfig, warm: Option<&Trained>) -> AppResult<(Trained, FitReport)> {
    if ctx.graph.r() < 3 {
        return Err(AppError::conflict("the supply network has fewer than 3 nodes: load master data and orders first"));
    }
    let hold = cfg.holdout.min(ctx.weeks / 3);
    let (mean, sd) = norm_stats(ctx, ctx.weeks - hold);
    let ds = dataset(ctx, &mean, &sd);
    let conn = ctx.graph.connectome(&FlowMix::default()).map_err(AppError::bad)?;
    let kinds: Vec<usize> = ctx.graph.nodes.iter().map(|n| n.kind.idx()).collect();
    let mut gnn = CirGnn::from_connectome(&conn, kinds, Kind::ALL.len(), ds.fin(), ds.ko(), cfg.seed).map_err(AppError::bad)?;
    gnn.skip_nch = NCH;
    let fp = ctx.graph.fingerprint();
    if let Some(w) = warm {
        if w.fingerprint == fp && w.gnn.params.len() == gnn.params.len() && w.gnn.edges == gnn.edges {
            gnn.params = w.gnn.params.clone();
        }
    }
    let rep = fit(&mut gnn, &ds, cfg).map_err(AppError::conflict)?;
    let t = Trained {
        gnn,
        nodes: ctx.graph.nodes.clone(),
        mean,
        sd,
        horizons: HORIZONS.to_vec(),
        channel_eval: rep.per_channel.clone(),
        fingerprint: fp,
        as_of: ctx.as_of,
        weeks: ctx.weeks,
        holdout_weeks: rep.holdout_weeks,
    };
    Ok((t, rep))
}

/// Replay the full history through the network (rebuilds the recurrent memory of every node).
pub fn replay(t: &Trained, ctx: &Context) -> AppResult<(Session, Vec<f64>)> {
    let same = t.nodes.len() == ctx.graph.nodes.len() && t.nodes.iter().zip(&ctx.graph.nodes).all(|(a, b)| a.id == b.id);
    if !same {
        return Err(AppError::conflict("the supply network changed since this model was trained; run POST /cognitive/train or /cognitive/learning/step"));
    }
    let ds = dataset(ctx, &t.mean, &t.sd);
    let w = t.gnn.effective_weights();
    let mut s = Session::new(t.gnn.n());
    let mut inp = Vec::new();
    let mut y = vec![];
    for k in 0..ds.t {
        inp.clear();
        ds.input_at(k, &mut inp);
        y = s.step(&t.gnn, &w, &inp);
    }
    Ok((s, y))
}

#[derive(Serialize, Clone, Debug)]
pub struct Fc {
    pub node: usize,
    pub kind: Kind,
    pub code: String,
    pub channel: &'static str,
    pub horizon_weeks: usize,
    pub week_start: NaiveDate,
    pub value: f64,
    pub low: f64,
    pub high: f64,
    /// P(delay > 2 days) for the delay channel
    pub prob_late: Option<f64>,
    pub trusted: bool,
}

pub fn forecasts(t: &Trained, ctx: &Context, y: &[f64]) -> Vec<Fc> {
    let r = t.gnn.r;
    let ko = t.gnn.ko;
    let mut out = Vec::new();
    for i in 0..r {
        for (hi, &h) in t.horizons.iter().enumerate() {
            for c in 0..NCH {
                let z = y[i * ko + hi * NCH + c];
                let (m, s) = (t.mean[i * NCH + c], t.sd[i * NCH + c]);
                let rs = t.channel_eval[c].resid_sd.get(hi).copied().unwrap_or(0.0).max(0.05);
                let v = to_raw(c, z, m, s);
                let (lo, hi_v) = (to_raw(c, z - 1.2816 * rs, m, s), to_raw(c, z + 1.2816 * rs, m, s));
                let prob = if c == 1 {
                    let sd_raw = (rs * s).max(0.25);
                    Some(1.0 - phi((LATE_DAYS - v) / sd_raw))
                } else {
                    None
                };
                out.push(Fc {
                    node: i,
                    kind: t.nodes[i].kind,
                    code: t.nodes[i].code.clone(),
                    channel: CHANNELS[c],
                    horizon_weeks: h,
                    week_start: ctx.week_start(ctx.weeks - 1 + h),
                    value: v,
                    low: lo,
                    high: hi_v,
                    prob_late: prob,
                    trusted: t.channel_ok(c),
                });
            }
        }
    }
    out
}

// ---- digital-twin free run ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Set,
    Add,
    Mul,
}
#[derive(Clone, Debug)]
pub struct Shock {
    pub node: usize,
    pub channel: usize,
    pub op: Op,
    pub value: f64,
    pub from: usize,
    pub dur: usize,
}

/// Raw-scale trajectory [step][node][channel] over `steps` future weeks, optionally under shocks.
pub fn free_run(t: &Trained, sess: &Session, y0: &[f64], steps: usize, shocks: &[Shock]) -> Vec<Vec<f64>> {
    let (r, ko) = (t.gnn.r, t.gnn.ko);
    let w = t.gnn.effective_weights();
    let mut s = sess.clone();
    let mut y = y0.to_vec();
    let mut traj = Vec::new();
    for step in 0..steps {
        let mut inp = vec![0.0; r * NCH * 2];
        let mut raw_row = vec![0.0; r * NCH];
        for i in 0..r {
            for c in 0..NCH {
                let (m, sd) = (t.mean[i * NCH + c], t.sd[i * NCH + c]);
                let mut z = y[i * ko + c];
                for sh in shocks.iter().filter(|x| x.node == i && x.channel == c && step >= x.from && step < x.from + x.dur) {
                    let raw = to_raw(c, z, m, sd);
                    let nr = match sh.op {
                        Op::Set => sh.value,
                        Op::Add => raw + sh.value,
                        Op::Mul => raw * sh.value,
                    };
                    z = to_z(c, nr, m, sd).clamp(-8.0, 8.0);
                }
                inp[i * NCH * 2 + c] = z;
                inp[i * NCH * 2 + NCH + c] = 1.0;
                raw_row[i * NCH + c] = to_raw(c, z, m, sd);
            }
        }
        traj.push(raw_row);
        y = s.step(&t.gnn, &w, &inp);
    }
    traj
}

// ---- persistence --------------------------------------------------------------------------------------

#[derive(FromQueryResult)]
struct ModelRow {
    id: Uuid,
    artifact: Value,
}

pub async fn save_model(db: &DatabaseConnection, t: &Trained, rep: &FitReport) -> AppResult<Uuid> {
    let id = Uuid::new_v4();
    db.execute(Statement::from_string(DbBackend::Postgres, "UPDATE cognitive_models SET active = FALSE WHERE active".to_string())).await?;
    let metrics = json!({
        "fit": rep,
        "channels": CHANNELS.iter().enumerate().map(|(c, n)| json!({
            "channel": n, "trusted": t.channel_ok(c), "holdout_mse": t.channel_eval[c].mse,
            "persistence_mse": t.channel_eval[c].persistence_mse, "mean_mse": t.channel_eval[c].mean_mse,
            "skill_vs_persistence": t.channel_eval[c].skill_vs_persistence, "skill_vs_mean": t.channel_eval[c].skill_vs_mean,
            "holdout_points": t.channel_eval[c].n })).collect::<Vec<_>>(),
        "init_spectral_radius": t.gnn.init_radius,
        "dale_law_ok": t.gnn.check_dale(),
        "parameters": t.gnn.params.len(),
    });
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO cognitive_models (id, as_of, weeks, n_nodes, fingerprint, active, metrics, artifact) VALUES ($1,$2,$3,$4,$5,TRUE,$6,$7)",
        [id.into(), t.as_of.into(), (t.weeks as i32).into(), (t.nodes.len() as i32).into(), t.fingerprint.clone().into(), metrics.into(), serde_json::to_value(t).map_err(|e| AppError::bad(e.to_string()))?.into()],
    ))
    .await?;
    Ok(id)
}

pub async fn load_active(db: &DatabaseConnection) -> AppResult<Option<(Uuid, Trained)>> {
    let row = ModelRow::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT id, artifact FROM cognitive_models WHERE active ORDER BY created_at DESC LIMIT 1".to_string(),
    ))
    .one(db)
    .await?;
    match row {
        None => Ok(None),
        Some(r) => Ok(Some((r.id, serde_json::from_value(r.artifact).map_err(|e| AppError::conflict(format!("stored model unreadable: {e}")))?))),
    }
}

pub async fn require_model(db: &DatabaseConnection) -> AppResult<(Uuid, Trained)> {
    load_active(db).await?.ok_or_else(|| AppError::conflict("no trained CIR-GNN yet: call POST /cognitive/train first"))
}

/// Store forecasts for later scoring (idempotent per model/node/channel/horizon/week).
pub async fn store_predictions(db: &DatabaseConnection, model_id: Uuid, fcs: &[Fc], nodes: &[Node]) -> AppResult<usize> {
    let mut n = 0;
    for f in fcs {
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO cognitive_predictions (id, model_id, node_id, kind, channel, horizon, target_week, predicted, prob) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
             ON CONFLICT (model_id, node_id, channel, horizon, target_week) DO UPDATE SET predicted = EXCLUDED.predicted, prob = EXCLUDED.prob WHERE cognitive_predictions.actual IS NULL",
            [Uuid::new_v4().into(), model_id.into(), nodes[f.node].id.into(), f.kind.label().to_lowercase().into(), f.channel.into(), (f.horizon_weeks as i32).into(), f.week_start.into(), f.value.into(), f.prob_late.into()],
        ))
        .await?;
        n += 1;
    }
    Ok(n)
}

/// Persist the recurrent memory (E/I rates) and encoder scores of every node.
pub async fn store_node_state(db: &DatabaseConnection, model_id: Uuid, t: &Trained, ctx: &Context, sess: &Session) -> AppResult<()> {
    let n = t.gnn.n();
    let rates = sess.rates(n);
    let conn = ctx.graph.connectome(&FlowMix::default()).map_err(AppError::bad)?;
    let crit = analytics::criticality(&conn);
    for (i, node) in t.nodes.iter().enumerate() {
        let st = json!({ "excitatory_rate": rates[i], "inhibitory_rate": rates[t.gnn.r + i], "criticality": crit[i].criticality,
                         "pagerank": crit[i].pagerank, "betweenness": crit[i].betweenness, "weeks_replayed": sess.t });
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO cognitive_node_state (model_id, node_id, kind, code, state) VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (model_id, node_id) DO UPDATE SET state = EXCLUDED.state, updated_at = now()",
            [model_id.into(), node.id.into(), node.kind.label().to_lowercase().into(), node.code.clone().into(), st.into()],
        ))
        .await?;
    }
    Ok(())
}

pub fn eval_on(t: &Trained, ctx: &Context, lo: usize) -> Vec<ChannelEval> {
    let ds = dataset(ctx, &t.mean, &t.sd);
    evaluate(&t.gnn, &ds, lo)
}
