//! Cognitive autonomous supply-chain layer: CIR-GNN training and prediction, graph encoder, recurrent
//! memory, digital twin, decision optimisation, guard-railed execution and the continuous-learning loop.
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::supply_chain::{
    cognitive::{
        analytics,
        cirgnn::FitConfig,
        data::{load_context, CHANNELS, NCH},
        engine, insights,
        graph::{FlowMix, Kind},
        ops::{self, PlanReq, TwinReq},
    },
    error::{AppError, AppResult},
    resilience::{analyse, hhi},
    routes::resilience::{disruption, load_network, DisruptionReq},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/cognitive/status", get(status))
        .route("/cognitive/train", post(train))
        .route("/cognitive/graph", get(graph))
        .route("/cognitive/encoder", get(encoder))
        .route("/cognitive/memory", get(memory))
        .route("/cognitive/forecast", get(forecast))
        .route("/cognitive/resilience", get(resilience))
        .route("/cognitive/optimize", post(optimize))
        .route("/cognitive/twin/simulate", post(twin))
        .route("/cognitive/actions", get(list_actions))
        .route("/cognitive/actions/{id}/approve", post(approve))
        .route("/cognitive/actions/{id}/reject", post(reject))
        .route("/cognitive/actions/{id}/execute", post(execute))
        .route("/cognitive/policies", get(list_policies))
        .route("/cognitive/policies/{kind}", put(set_policy))
        .route("/cognitive/signals", post(add_signal).get(list_signals))
        .route("/cognitive/learning/reconcile", post(reconcile))
        .route("/cognitive/learning/step", post(learning_step))
        .route("/cognitive/cycle", post(cycle))
}

#[derive(Deserialize, Default)]
pub struct CtxQuery {
    as_of: Option<NaiveDate>,
    weeks: Option<usize>,
    lookback_days: Option<i32>,
}

macro_rules! ctx_impl {
    ($($t:ty),*) => {$(impl $t { fn ctx(&self) -> CtxQuery { CtxQuery { as_of: self.as_of, weeks: self.weeks, lookback_days: self.lookback_days } } })*};
}
ctx_impl!(TopQuery, MemQuery, FcQuery);

async fn ctx_of(s: &AppState, q: &CtxQuery) -> AppResult<crate::supply_chain::cognitive::data::Context> {
    load_context(&s.db, q.as_of.unwrap_or_else(|| Utc::now().date_naive()), q.weeks.unwrap_or(104), q.lookback_days.unwrap_or(365)).await
}

#[derive(FromQueryResult)]
struct MetaRow {
    id: Uuid,
    created_at: chrono::DateTime<Utc>,
    as_of: NaiveDate,
    weeks: i32,
    n_nodes: i32,
    fingerprint: String,
    metrics: Value,
}

async fn latest_meta(s: &AppState) -> AppResult<Option<MetaRow>> {
    Ok(MetaRow::find_by_statement(Statement::from_string(DbBackend::Postgres, "SELECT id, created_at, as_of, weeks, n_nodes, fingerprint, metrics FROM cognitive_models WHERE active ORDER BY created_at DESC LIMIT 1".to_string())).one(&s.db).await?)
}

async fn status(State(s): State<AppState>) -> AppResult<Json<Value>> {
    let m = latest_meta(&s).await?;
    let pol = ops_policies(&s).await?;
    Ok(Json(json!({
        "layers": ["ingestion", "supply_chain_graph", "hypergraph_engine", "connectome_graph_encoder", "recurrent_memory", "temporal_learning",
                   "predictive_intelligence", "decision_optimization", "autonomous_execution", "digital_twin", "resilience_analytics", "continuous_learning"],
        "model": m.map(|m| json!({"id": m.id, "trained_at": m.created_at, "as_of": m.as_of, "weeks": m.weeks, "nodes": m.n_nodes, "fingerprint": m.fingerprint, "metrics": m.metrics})),
        "policies": pol,
    })))
}

#[derive(Deserialize, Default)]
pub struct TrainReq {
    as_of: Option<NaiveDate>,
    weeks: Option<usize>,
    steps: Option<usize>,
    holdout_weeks: Option<usize>,
    seed: Option<u64>,
    /// continue from the active model's weights when the network is unchanged
    warm_start: Option<bool>,
}

async fn train(State(s): State<AppState>, Json(r): Json<TrainReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let ctx = Arc::new(load_context(&s.db, r.as_of.unwrap_or_else(|| Utc::now().date_naive()), r.weeks.unwrap_or(104), 365).await?);
    let cfg = FitConfig { steps: r.steps.unwrap_or(300).clamp(10, 5000), holdout: r.holdout_weeks.unwrap_or(12), seed: r.seed.unwrap_or(1), threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2), ..Default::default() };
    let warm = if r.warm_start.unwrap_or(false) { engine::load_active(&s.db).await?.map(|x| x.1) } else { None };
    let (t, rep) = ops::train_async(ctx.clone(), cfg, warm).await?;
    let id = engine::save_model(&s.db, &t, &rep).await?;
    let (sess, y) = engine::replay(&t, &ctx)?;
    engine::store_node_state(&s.db, id, &t, &ctx, &sess).await?;
    let stored = engine::store_predictions(&s.db, id, &engine::forecasts(&t, &ctx, &y), &t.nodes).await?;
    Ok((StatusCode::CREATED, Json(json!({
        "model_id": id, "nodes": t.nodes.len(), "edges": t.gnn.edges.len(), "parameters": t.gnn.params.len(),
        "wiring": {"initial_spectral_radius": t.gnn.init_radius, "dale_law_ok": t.gnn.check_dale()},
        "fit": rep,
        "channels": CHANNELS.iter().enumerate().map(|(c, n)| json!({"channel": n, "trusted": t.channel_ok(c), "skill_vs_persistence": t.channel_eval[c].skill_vs_persistence, "skill_vs_node_mean": t.channel_eval[c].skill_vs_mean, "holdout_points": t.channel_eval[c].n})).collect::<Vec<_>>(),
        "predictions_stored": stored,
        "reading": "a channel is 'trusted' only if, on weeks the model never trained on, it beats both 'same as last week' and the node's mean; untrusted channels fall back to statistical estimates in /cognitive/optimize",
    }))))
}

async fn graph(State(s): State<AppState>, Query(q): Query<CtxQuery>) -> AppResult<Json<Value>> {
    let ctx = ctx_of(&s, &q).await?;
    let g = &ctx.graph;
    let counts: Vec<Value> = Kind::ALL.iter().map(|k| json!({"kind": k, "nodes": g.nodes.iter().filter(|n| n.kind == *k).count()})).collect();
    let mut hk = std::collections::BTreeMap::new();
    for h in &g.hypers {
        *hk.entry(h.kind).or_insert(0usize) += 1;
    }
    let conn = g.connectome(&FlowMix::default()).map_err(AppError::bad)?;
    Ok(Json(json!({
        "nodes": g.r(), "nodes_in_database": ctx.total_nodes, "truncated": ctx.total_nodes > g.r(), "by_kind": counts,
        "edges": conn.num_edges(), "density": conn.density(), "connected": conn.is_connected(), "hyperedges": hk,
        "fingerprint": g.fingerprint(), "weeks": ctx.weeks, "window_start": ctx.base,
        "wiring": "flow strength -> connection weight, lead time (weeks) -> conduction delay; each node has an excitatory and an inhibitory unit",
    })))
}

#[derive(Deserialize, Default)]
pub struct TopQuery {
    as_of: Option<NaiveDate>,
    weeks: Option<usize>,
    lookback_days: Option<i32>,
    top: Option<usize>,
}

async fn encoder(State(s): State<AppState>, Query(q): Query<TopQuery>) -> AppResult<Json<Value>> {
    let ctx = ctx_of(&s, &q.ctx()).await?;
    Ok(Json(insights::encoder_report(&ctx, q.top.unwrap_or(10).clamp(1, 50)).map_err(AppError::conflict)?))
}

#[derive(Deserialize, Default)]
pub struct MemQuery {
    as_of: Option<NaiveDate>,
    weeks: Option<usize>,
    lookback_days: Option<i32>,
    node: Option<String>,
    limit: Option<usize>,
}

async fn memory(State(s): State<AppState>, Query(q): Query<MemQuery>) -> AppResult<Json<Value>> {
    let ctx = ctx_of(&s, &q.ctx()).await?;
    let f = insights::findings(&ctx);
    let lim = q.limit.unwrap_or(30).clamp(1, 500);
    let (profile, stored) = match &q.node {
        Some(code) => {
            let i = ctx.graph.nodes.iter().position(|n| n.code.eq_ignore_ascii_case(code.trim())).ok_or_else(|| AppError::not_found("node"))?;
            let st = Statement::from_sql_and_values(DbBackend::Postgres, "SELECT state AS v FROM cognitive_node_state s JOIN cognitive_models m ON m.id = s.model_id WHERE m.active AND s.node_id = $1 LIMIT 1", [ctx.graph.nodes[i].id.into()]);
            #[derive(FromQueryResult)]
            struct V {
                v: Value,
            }
            (Some(insights::node_profile(&ctx, i)), V::find_by_statement(st).one(&s.db).await?.map(|x| x.v))
        }
        None => (None, None),
    };
    let filtered: Vec<_> = f.iter().filter(|x| q.node.as_ref().map(|n| x.node.eq_ignore_ascii_case(n)).unwrap_or(true)).take(lim).collect();
    Ok(Json(json!({
        "findings": filtered, "supplier_evolution": if q.node.is_none() { insights::supplier_evolution(&ctx) } else { vec![] },
        "node_profile": profile, "recurrent_state": stored,
        "note": "findings come from the stored weekly history of every node; recurrent_state is the CIR-GNN's replayed memory of the node (E/I rates)",
    })))
}

#[derive(Deserialize, Default)]
pub struct FcQuery {
    as_of: Option<NaiveDate>,
    weeks: Option<usize>,
    lookback_days: Option<i32>,
    kind: Option<String>,
    node: Option<String>,
    channel: Option<String>,
    limit: Option<usize>,
    /// store the predictions for the learning loop (default true)
    record: Option<bool>,
}

async fn forecast(State(s): State<AppState>, Query(q): Query<FcQuery>) -> AppResult<Json<Value>> {
    let ctx = ctx_of(&s, &q.ctx()).await?;
    let (id, t) = engine::require_model(&s.db).await?;
    let (_, y) = engine::replay(&t, &ctx)?;
    let all = engine::forecasts(&t, &ctx, &y);
    if q.record.unwrap_or(true) {
        engine::store_predictions(&s.db, id, &all, &t.nodes).await?;
    }
    let rows: Vec<_> = all
        .iter()
        .filter(|f| q.kind.as_ref().map(|k| format!("{:?}", f.kind).eq_ignore_ascii_case(k)).unwrap_or(true))
        .filter(|f| q.node.as_ref().map(|n| f.code.eq_ignore_ascii_case(n)).unwrap_or(true))
        .filter(|f| q.channel.as_ref().map(|c| f.channel == c).unwrap_or(true))
        .take(q.limit.unwrap_or(100).clamp(1, 5000))
        .collect();
    Ok(Json(json!({"model_id": id, "trusted_channels": (0..NCH).filter(|c| t.channel_ok(*c)).map(|c| CHANNELS[c]).collect::<Vec<_>>(), "forecasts": rows})))
}

async fn resilience(State(s): State<AppState>, Query(q): Query<TopQuery>) -> AppResult<Json<Value>> {
    let ctx = ctx_of(&s, &q.ctx()).await?;
    let as_of = q.as_of.unwrap_or_else(|| Utc::now().date_naive());
    let enc = insights::encoder_report(&ctx, 5).map_err(AppError::conflict)?;
    let sp = analyse(&ctx.graph.hypergraph());
    // concentration
    let net = load_network(&s.db, as_of, q.lookback_days.unwrap_or(365)).await?;
    let (mut sole, mut hh) = (0usize, Vec::new());
    for p in &net.products {
        if p.facts.qualified.len() == 1 {
            sole += 1;
        }
        let shares: Vec<f64> = p.facts.shares.iter().map(|x| x.1).collect();
        if !shares.is_empty() {
            hh.push(hhi(&shares));
        }
    }
    let mean_hhi = if hh.is_empty() { None } else { Some(hh.iter().sum::<f64>() / hh.len() as f64) };
    let hhi_high = hh.iter().filter(|h| **h > 0.5).count();
    // recovery time and financial impact of losing each of the most critical suppliers
    let conn = ctx.graph.connectome(&FlowMix::default()).map_err(AppError::bad)?;
    let crit = analytics::criticality(&conn);
    let mut sup: Vec<usize> = (0..ctx.graph.r()).filter(|i| ctx.graph.nodes[*i].kind == Kind::Supplier).collect();
    sup.sort_by(|a, b| crit[*b].criticality.partial_cmp(&crit[*a].criticality).unwrap_or(std::cmp::Ordering::Equal));
    let mut impacts = Vec::new();
    for i in sup.into_iter().take(3) {
        let code = ctx.graph.nodes[i].code.clone();
        let dr = DisruptionReq { supplier_codes: vec![code.clone()], as_of: Some(as_of), lookback_days: None, horizon_days: Some(90.0), max_uplift: None, standby_share: None, qualification_days: None, top: Some(1) };
        if let Ok(j) = disruption(State(s.clone()), Json(dr)).await {
            impacts.push(json!({"supplier": code, "criticality": crit[i].criticality, "shortfall_value": j.0["summary"]["shortfall_value"], "longest_recovery_days": j.0["summary"]["longest_recovery_days"], "products_without_any_source": j.0["summary"]["products_now_without_any_source"]}));
        }
    }
    Ok(Json(json!({
        "hypergraph_spectrum": sp.map(|x| json!({"nodes": x.suppliers, "components": x.components, "lambda2": x.lambda2, "lambda2_main_component": x.lambda2_main_component,
            "cheeger_bounds": [x.cheeger_lower_bound, x.cheeger_upper_bound], "fragmentation_risk": x.fragmentation_risk, "fiedler_cluster_sizes": x.component_sizes})),
        "node_criticality": enc["critical_nodes"], "bottlenecks": enc["bottlenecks"], "topology": enc["topology"],
        "concentration": {"products_with_sourcing": net.products.len(), "sole_source_products": sole, "mean_hhi": mean_hhi, "products_hhi_above_0_5": hhi_high},
        "loss_of_critical_supplier": impacts,
    })))
}

async fn optimize(State(s): State<AppState>, Json(r): Json<PlanReq>) -> AppResult<Json<Value>> {
    Ok(Json(ops::plan(&s, r).await?))
}

async fn twin(State(s): State<AppState>, Json(r): Json<TwinReq>) -> AppResult<Json<Value>> {
    Ok(Json(ops::twin(&s, r).await?))
}

#[derive(Deserialize)]
pub struct ActQuery {
    status: Option<String>,
    limit: Option<i64>,
}

async fn list_actions(State(s): State<AppState>, Query(q): Query<ActQuery>) -> AppResult<Json<Value>> {
    #[derive(FromQueryResult, serde::Serialize)]
    struct A {
        id: Uuid,
        kind: String,
        status: String,
        payload: Value,
        rationale: Value,
        result: Option<Value>,
        created_at: chrono::DateTime<Utc>,
    }
    let rows = A::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, kind, status, payload, rationale, result, created_at FROM cognitive_actions WHERE ($1::text IS NULL OR status = $1) ORDER BY created_at DESC LIMIT $2",
        [q.status.into(), q.limit.unwrap_or(100).clamp(1, 500).into()],
    ))
    .all(&s.db)
    .await?;
    Ok(Json(json!(rows)))
}

async fn approve(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    Ok(Json(ops::decide_action(&s.db, id, true).await?))
}
async fn reject(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    Ok(Json(ops::decide_action(&s.db, id, false).await?))
}
async fn execute(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    Ok(Json(ops::execute_action(&s, id, false).await?))
}

async fn ops_policies(s: &AppState) -> AppResult<Value> {
    #[derive(FromQueryResult, serde::Serialize)]
    struct P {
        action_kind: String,
        autonomy: String,
        params: Value,
    }
    Ok(json!(P::find_by_statement(Statement::from_string(DbBackend::Postgres, "SELECT action_kind, autonomy, params FROM cognitive_policies ORDER BY action_kind".to_string())).all(&s.db).await?))
}
async fn list_policies(State(s): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(ops_policies(&s).await?))
}

#[derive(Deserialize)]
pub struct PolicyIn {
    autonomy: Option<String>,
    /// merged into the existing guardrail parameters
    params: Option<Value>,
}

async fn set_policy(State(s): State<AppState>, Path(kind): Path<String>, Json(i): Json<PolicyIn>) -> AppResult<Json<Value>> {
    let (cur_a, mut cur_p) = ops::policy(&s.db, &kind).await?;
    let a = i.autonomy.unwrap_or(cur_a);
    if !matches!(a.as_str(), "advisory" | "supervised" | "autonomous") {
        return Err(AppError::bad("autonomy must be advisory, supervised or autonomous"));
    }
    if let Some(Value::Object(m)) = i.params {
        for (k, v) in m {
            cur_p[k] = v;
        }
    }
    s.db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE cognitive_policies SET autonomy = $2, params = $3, updated_at = now() WHERE action_kind = $1", [kind.clone().into(), a.clone().into(), cur_p.clone().into()])).await?;
    Ok(Json(json!({"action_kind": kind, "autonomy": a, "params": cur_p})))
}

#[derive(Deserialize)]
pub struct SignalIn {
    /// weather | market | iot | port_congestion | disruption (0-1 risk on a node) | ...
    signal_type: String,
    node_code: Option<String>,
    value: f64,
    unit: Option<String>,
    note: Option<String>,
}

async fn add_signal(State(s): State<AppState>, Json(i): Json<SignalIn>) -> AppResult<(StatusCode, Json<Value>)> {
    if i.signal_type.trim().is_empty() || !i.value.is_finite() {
        return Err(AppError::bad("signal_type and a finite value are required"));
    }
    let node = match &i.node_code {
        Some(c) => {
            #[derive(FromQueryResult)]
            struct N {
                id: Uuid,
            }
            let n = N::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM (SELECT id, code FROM suppliers UNION ALL SELECT id, code FROM carriers UNION ALL SELECT id, code FROM warehouses \
                 UNION ALL SELECT id, sku FROM products UNION ALL SELECT id, code FROM scm_customers) x WHERE lower(code) = lower($1) LIMIT 1",
                [c.trim().into()],
            ))
            .one(&s.db)
            .await?
            .ok_or_else(|| AppError::not_found("node"))?;
            Some(n.id)
        }
        None => None,
    };
    let id = Uuid::new_v4();
    s.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO cognitive_signals (id, signal_type, node_id, value, unit, note) VALUES ($1,$2,$3,$4,$5,$6)",
        [id.into(), i.signal_type.trim().to_lowercase().into(), node.into(), i.value.into(), i.unit.into(), i.note.into()],
    ))
    .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn list_signals(State(s): State<AppState>) -> AppResult<Json<Value>> {
    #[derive(FromQueryResult, serde::Serialize)]
    struct S {
        id: Uuid,
        signal_type: String,
        node_id: Option<Uuid>,
        value: f64,
        unit: Option<String>,
        note: Option<String>,
        observed_at: chrono::DateTime<Utc>,
    }
    Ok(Json(json!(S::find_by_statement(Statement::from_string(DbBackend::Postgres, "SELECT id, signal_type, node_id, value, unit, note, observed_at FROM cognitive_signals ORDER BY observed_at DESC LIMIT 200".to_string())).all(&s.db).await?)))
}

async fn reconcile(State(s): State<AppState>, Query(q): Query<CtxQuery>) -> AppResult<Json<Value>> {
    let ctx = ctx_of(&s, &q).await?;
    Ok(Json(ops::reconcile(&s.db, &ctx).await?))
}

#[derive(Deserialize, Default)]
pub struct StepReq {
    force: Option<bool>,
    as_of: Option<NaiveDate>,
}
async fn learning_step(State(s): State<AppState>, Json(r): Json<StepReq>) -> AppResult<Json<Value>> {
    Ok(Json(ops::learning_step(&s, r.force.unwrap_or(false), r.as_of).await?))
}

/// One pass of the whole loop: learn from outcomes, resilience check, optimise, propose / execute per policy.
async fn cycle(State(s): State<AppState>, Json(r): Json<PlanReq>) -> AppResult<Json<Value>> {
    let learn = ops::learning_step(&s, false, r.as_of).await?;
    let plan = ops::plan(&s, r).await?;
    Ok(Json(json!({"learning": learn, "plan": plan})))
}
