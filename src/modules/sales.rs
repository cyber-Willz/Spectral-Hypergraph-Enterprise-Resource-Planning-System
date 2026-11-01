//! Sales analytics: performance with significance testing, cycle time, pipeline health,
//! a validity-gated win-probability model and a backtested revenue forecast.
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Datelike, Duration, NaiveDate};
use sea_orm::{DatabaseConnection, Value};
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, HashMap};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/crm/sales/performance", get(performance))
        .route("/api/crm/sales/cycle-time", get(cycle_time))
        .route("/api/crm/sales/pipeline-health", get(pipeline_health))
        .route("/api/crm/predict/train", post(train))
        .route("/api/crm/predict/model", get(model_info))
        .route("/api/crm/predict/score-open", post(score_open))
        .route("/api/crm/forecast/revenue", get(forecast))
        .route("/api/crm/forecast/weighted-pipeline", get(weighted_pipeline))
}

const JOINS: &str = "FROM crm_opportunities o LEFT JOIN crm_sales_agents a ON a.id=o.sales_agent_id LEFT JOIN crm_products pr ON pr.id=o.product_id LEFT JOIN crm_accounts acc ON acc.id=o.account_id";
const CLOSED: &str = "o.stage IN ('closed_won','closed_lost')";
const OPEN: &str = "o.stage NOT IN ('closed_won','closed_lost')";

fn dim_sql(d: &str) -> Option<&'static str> {
    Some(match d {
        "agent" => "a.name", "manager" => "a.manager_name", "regional_office" => "a.regional_office", "product" => "pr.name", "series" => "pr.series",
        "sector" => "acc.industry", "account" => "acc.name", "country" => "acc.country", "stage" => "o.stage",
        "month" => "to_char(o.close_date,'YYYY-MM')", _ => return None,
    })
}

/// The dataset to analyse: explicit `dataset` param, else the one with the most closed deals.
async fn dataset(db: &DatabaseConnection, p: &HashMap<String, String>) -> AppResult<Option<String>> {
    if let Some(d) = p.get("dataset") { return Ok(if d == "all" { None } else { Some(d.clone()) }); }
    let r = row(db, "SELECT source_dataset AS d FROM crm_opportunities WHERE stage IN ('closed_won','closed_lost') AND source_dataset IS NOT NULL GROUP BY 1 ORDER BY count(*) DESC LIMIT 1", vec![]).await?;
    Ok(r["d"].as_str().map(String::from))
}

fn erfc(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    let y = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429)))) * (-x * x).exp();
    if x >= 0.0 { y } else { 2.0 - y }
}
fn two_sided_p(z: f64) -> f64 { erfc(z.abs() / std::f64::consts::SQRT_2) }
fn wilson(w: f64, n: f64) -> (f64, f64) {
    if n == 0.0 { return (0.0, 1.0); }
    let (p, z) = (w / n, 1.96);
    let d = 1.0 + z * z / n;
    let c = (p + z * z / (2.0 * n)) / d;
    let h = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / d;
    (c - h, c + h)
}
fn r1(x: f64) -> f64 { (x * 1000.0).round() / 1000.0 }

// ------------------------------------------------------------ performance

async fn performance(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let by = p.get("by").map(String::as_str).unwrap_or("agent");
    let dim = dim_sql(by).ok_or_else(|| AppError::BadRequest("by must be one of: agent, manager, regional_office, product, series, sector, account, country, stage, month".into()))?;
    let ds = dataset(&st.db, &p).await?;
    let min_deals: i64 = p.get("min_deals").and_then(|v| v.parse().ok()).unwrap_or(1);
    let vals: Vec<Value> = vec![ds.clone().into(), p.get("from").cloned().into(), p.get("to").cloned().into()];
    let scope = "($1::text IS NULL OR o.source_dataset=$1) AND (o.close_date IS NULL OR (($2::date IS NULL OR o.close_date >= $2::date) AND ($3::date IS NULL OR o.close_date <= $3::date)))";
    let mut data = rows(&st.db, &format!("SELECT COALESCE({dim},'(none)') AS key,
        count(*) FILTER (WHERE {CLOSED}) AS closed, count(*) FILTER (WHERE o.stage='closed_won') AS won, count(*) FILTER (WHERE o.stage='closed_lost') AS lost,
        COALESCE(sum(o.close_value) FILTER (WHERE o.stage='closed_won'),0)::float8 AS revenue_won,
        round(avg(o.close_value) FILTER (WHERE o.stage='closed_won'),2)::float8 AS avg_deal_value,
        round(avg(o.close_date-o.engage_date) FILTER (WHERE o.stage='closed_won' AND o.engage_date IS NOT NULL),1)::float8 AS avg_cycle_days,
        round(100*sum(o.close_value) FILTER (WHERE o.stage='closed_won' AND pr.list_price>0)/NULLIF(sum(pr.list_price) FILTER (WHERE o.stage='closed_won' AND pr.list_price>0),0),1)::float8 AS price_realization_pct,
        count(*) FILTER (WHERE {OPEN}) AS open_deals, COALESCE(sum(o.amount) FILTER (WHERE {OPEN}),0)::float8 AS open_pipeline
        {JOINS} WHERE {scope} GROUP BY 1 HAVING count(*) FILTER (WHERE {CLOSED}) + count(*) FILTER (WHERE {OPEN}) >= {min_deals} ORDER BY revenue_won DESC, key"), vals).await?;
    let (tw, tc): (f64, f64) = data.iter().fold((0.0, 0.0), |a, d| (a.0 + d["won"].as_f64().unwrap_or(0.0), a.1 + d["closed"].as_f64().unwrap_or(0.0)));
    let p0 = if tc > 0.0 { tw / tc } else { 0.0 };
    let testable = data.iter().filter(|d| d["closed"].as_f64().unwrap_or(0.0) >= 30.0).count().max(1) as f64;
    for d in data.iter_mut() {
        let (w, c) = (d["won"].as_f64().unwrap_or(0.0), d["closed"].as_f64().unwrap_or(0.0));
        let (lo, hi) = wilson(w, c);
        let rate = if c > 0.0 { w / c } else { 0.0 };
        let se = (p0 * (1.0 - p0) / c.max(1.0)).sqrt();
        let p_adj = if c >= 30.0 && se > 0.0 { (two_sided_p((rate - p0) / se) * testable).min(1.0) } else { 1.0 };
        let o = d.as_object_mut().unwrap();
        o.insert("win_rate_pct".into(), json!(if c > 0.0 { r1(rate * 100.0) } else { 0.0 }));
        o.insert("win_rate_ci95".into(), json!([r1(lo * 100.0), r1(hi * 100.0)]));
        o.insert("p_value_vs_overall_adj".into(), json!(r1(p_adj)));
        o.insert("significantly_different".into(), json!(p_adj < 0.05));
    }
    let sig = data.iter().filter(|d| d["significantly_different"] == true).count();
    Ok(Json(json!({ "by": by, "dataset": ds, "overall_win_rate_pct": r1(p0 * 100.0), "closed_deals": tc,
        "groups_significantly_different": sig,
        "note": "win-rate differences are flagged only when significant after Bonferroni correction across groups with >= 30 closed deals; ignore rank differences between unflagged groups",
        "data": data })))
}

async fn cycle_time(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let by = p.get("by").map(String::as_str).unwrap_or("product");
    let dim = dim_sql(by).ok_or_else(|| AppError::BadRequest("invalid 'by' dimension".into()))?;
    let ds = dataset(&st.db, &p).await?;
    let q = |pct: &str| format!("percentile_cont({pct}) WITHIN GROUP (ORDER BY (o.close_date-o.engage_date))::float8");
    let d = rows(&st.db, &format!("SELECT COALESCE({dim},'(none)') AS key, o.stage, count(*) AS deals, {} AS p10, {} AS p25, {} AS median, {} AS p75, {} AS p90
        {JOINS} WHERE o.stage IN ('closed_won','closed_lost') AND o.engage_date IS NOT NULL AND o.close_date IS NOT NULL AND ($1::text IS NULL OR o.source_dataset=$1)
        GROUP BY 1,2 ORDER BY 1,2", q("0.1"), q("0.25"), q("0.5"), q("0.75"), q("0.9")), vec![ds.clone().into()]).await?;
    Ok(Json(json!({ "by": by, "dataset": ds, "unit": "days from engage_date to close_date", "data": d })))
}

async fn pipeline_health(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let ds = dataset(&st.db, &p).await?;
    let as_of = match p.get("as_of") { Some(a) => a.clone(), None => row(&st.db, "SELECT COALESCE(max(close_date), max(engage_date), current_date)::text AS d FROM crm_opportunities WHERE ($1::text IS NULL OR source_dataset=$1)", vec![ds.clone().into()]).await?["d"].as_str().unwrap().to_string() };
    let cyc = row(&st.db, "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY (close_date-engage_date))::float8 AS median, percentile_cont(0.9) WITHIN GROUP (ORDER BY (close_date-engage_date))::float8 AS p90
        FROM crm_opportunities WHERE stage='closed_won' AND engage_date IS NOT NULL AND close_date IS NOT NULL AND ($1::text IS NULL OR source_dataset=$1)", vec![ds.clone().into()]).await?;
    let p90 = cyc["p90"].as_f64().unwrap_or(90.0);
    let by_stage = rows(&st.db, "SELECT stage, count(*) AS deals, COALESCE(sum(amount),0)::float8 AS amount, COALESCE(sum(amount*COALESCE(ml_win_probability, probability/100.0)),0)::float8 AS weighted_amount,
        count(*) FILTER (WHERE account_id IS NULL) AS without_account, count(*) FILTER (WHERE engage_date IS NULL) AS without_engage_date
        FROM crm_opportunities WHERE stage NOT IN ('closed_won','closed_lost') AND ($1::text IS NULL OR source_dataset=$1) GROUP BY stage ORDER BY 2 DESC", vec![ds.clone().into()]).await?;
    let stale = rows(&st.db, "SELECT o.id::text AS id, o.name, o.stage, a.name AS agent, o.amount::float8 AS amount, o.engage_date::text AS engage_date, ($2::date - o.engage_date) AS age_days
        FROM crm_opportunities o LEFT JOIN crm_sales_agents a ON a.id=o.sales_agent_id
        WHERE o.stage NOT IN ('closed_won','closed_lost') AND o.engage_date IS NOT NULL AND ($2::date - o.engage_date) > $3 AND ($1::text IS NULL OR o.source_dataset=$1)
        ORDER BY age_days DESC LIMIT 25", vec![ds.clone().into(), as_of.clone().into(), (p90.ceil() as i32).into()]).await?;
    let n_stale = row(&st.db, "SELECT count(*) AS n, COALESCE(sum(amount),0)::float8 AS amount FROM crm_opportunities WHERE stage NOT IN ('closed_won','closed_lost') AND engage_date IS NOT NULL AND ($2::date - engage_date) > $3 AND ($1::text IS NULL OR source_dataset=$1)",
        vec![ds.clone().into(), as_of.clone().into(), (p90.ceil() as i32).into()]).await?;
    Ok(Json(json!({ "dataset": ds, "as_of": as_of, "historical_cycle_days": { "median": cyc["median"], "p90": cyc["p90"] }, "stale_threshold_days": p90.ceil(),
        "open_by_stage": by_stage, "stale_deals": { "count": n_stale["n"], "amount": n_stale["amount"], "top": stale },
        "note": "as_of defaults to the latest activity date in the data (not today); a deal is stale when open longer than the 90th percentile of won-deal cycle times" })))
}

// -------------------------------------------------------- win-probability

struct Row { id: String, won: bool, close: Option<NaiveDate>, cats: Vec<String>, nums: Vec<Option<f64>> }
const CAT_NAMES: &[&str] = &["product", "series", "sector", "country", "agent", "manager", "office", "engage_month"];
const NUM_NAMES: &[&str] = &["ln_list_price", "ln_account_revenue", "ln_employees", "account_age_years"];

async fn feature_rows(db: &DatabaseConnection, ds: &Option<String>, closed: bool) -> AppResult<Vec<Row>> {
    let cond = if closed { CLOSED } else { OPEN };
    let r = rows(db, &format!("SELECT o.id::text AS id, o.stage, o.close_date::text AS close_date,
        COALESCE(pr.name,'unknown') AS product, COALESCE(pr.series,'unknown') AS series, COALESCE(acc.industry,'unknown') AS sector, COALESCE(acc.country,'unknown') AS country,
        COALESCE(a.name,'unknown') AS agent, COALESCE(a.manager_name,'unknown') AS manager, COALESCE(a.regional_office,'unknown') AS office,
        COALESCE(extract(month from o.engage_date)::int::text,'unknown') AS engage_month,
        ln(NULLIF(pr.list_price,0))::float8 AS ln_price, ln(NULLIF(acc.annual_revenue,0))::float8 AS ln_rev, ln(NULLIF(acc.employees,0))::float8 AS ln_emp,
        (extract(year from COALESCE(o.engage_date, current_date)) - acc.year_established)::float8 AS age
        {JOINS} WHERE {cond} AND ($1::text IS NULL OR o.source_dataset=$1) ORDER BY o.close_date NULLS LAST, o.id"), vec![ds.clone().into()]).await?;
    Ok(r.iter().map(|x| Row { id: x["id"].as_str().unwrap().into(), won: x["stage"] == "closed_won", close: x["close_date"].as_str().and_then(|d| d.parse().ok()),
        cats: CAT_NAMES.iter().map(|c| x[*c].as_str().unwrap_or("unknown").to_string()).collect(),
        nums: vec![x["ln_price"].as_f64(), x["ln_rev"].as_f64(), x["ln_emp"].as_f64(), x["age"].as_f64()] }).collect())
}

struct Encoder { levels: Vec<Vec<String>>, mean: Vec<f64>, std: Vec<f64> }
impl Encoder {
    fn fit(train: &[&Row]) -> Self {
        let mut levels = vec![];
        for (i, _) in CAT_NAMES.iter().enumerate() {
            let mut c: BTreeMap<&str, usize> = BTreeMap::new();
            for r in train { *c.entry(r.cats[i].as_str()).or_default() += 1; }
            levels.push(c.into_iter().filter(|(l, n)| *n >= 20 && *l != "unknown").map(|(l, _)| l.to_string()).collect());
        }
        let (mut mean, mut std) = (vec![], vec![]);
        for j in 0..NUM_NAMES.len() {
            let v: Vec<f64> = train.iter().filter_map(|r| r.nums[j]).collect();
            let m = if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
            let s = if v.len() > 1 { (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() as f64 - 1.0)).sqrt() } else { 1.0 };
            mean.push(m); std.push(if s > 1e-9 { s } else { 1.0 });
        }
        Encoder { levels, mean, std }
    }
    fn names(&self) -> Vec<String> {
        let mut v = vec![];
        for (i, c) in CAT_NAMES.iter().enumerate() { for l in &self.levels[i] { v.push(format!("{c}={l}")); } }
        v.extend(NUM_NAMES.iter().map(|s| s.to_string()));
        v
    }
    fn encode(&self, r: &Row) -> Vec<f64> {
        let mut x = vec![];
        for (i, _) in CAT_NAMES.iter().enumerate() { for l in &self.levels[i] { x.push(if &r.cats[i] == l { 1.0 } else { 0.0 }); } }
        for j in 0..NUM_NAMES.len() { x.push(r.nums[j].map(|v| (v - self.mean[j]) / self.std[j]).unwrap_or(0.0)); }
        x
    }
}

fn sigmoid(z: f64) -> f64 { 1.0 / (1.0 + (-z).exp()) }

/// L2-regularised logistic regression, full-batch Adam.
fn fit_logistic(x: &[Vec<f64>], y: &[f64], l2: f64) -> (Vec<f64>, f64) {
    let (n, d) = (x.len(), x[0].len());
    let (mut w, mut b) = (vec![0.0; d], (y.iter().sum::<f64>() / n as f64).max(1e-6).min(1.0 - 1e-6));
    b = (b / (1.0 - b)).ln();
    let (mut mw, mut vw, mut mb, mut vb) = (vec![0.0; d], vec![0.0; d], 0.0, 0.0);
    let (lr, b1, b2, eps) = (0.05, 0.9, 0.999, 1e-8);
    for t in 1..=400 {
        let mut gw = vec![0.0; d]; let mut gb = 0.0;
        for i in 0..n {
            let z = b + x[i].iter().zip(&w).map(|(a, c)| a * c).sum::<f64>();
            let e = sigmoid(z) - y[i];
            gb += e; for j in 0..d { gw[j] += e * x[i][j]; }
        }
        for j in 0..d {
            let g = gw[j] / n as f64 + l2 * w[j];
            mw[j] = b1 * mw[j] + (1.0 - b1) * g; vw[j] = b2 * vw[j] + (1.0 - b2) * g * g;
            w[j] -= lr * (mw[j] / (1.0 - b1.powi(t))) / ((vw[j] / (1.0 - b2.powi(t))).sqrt() + eps);
        }
        let g = gb / n as f64;
        mb = b1 * mb + (1.0 - b1) * g; vb = b2 * vb + (1.0 - b2) * g * g;
        b -= lr * (mb / (1.0 - b1.powi(t))) / ((vb / (1.0 - b2.powi(t))).sqrt() + eps);
    }
    (w, b)
}

fn auc(scores: &[f64], y: &[f64]) -> f64 {
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    idx.sort_by(|&a, &b| scores[a].partial_cmp(&scores[b]).unwrap());
    let (mut rank_sum, mut i) = (0.0, 0);
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && scores[idx[j + 1]] == scores[idx[i]] { j += 1; }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for k in i..=j { if y[idx[k]] > 0.5 { rank_sum += avg; } }
        i = j + 1;
    }
    let n1 = y.iter().filter(|v| **v > 0.5).count() as f64; let n0 = y.len() as f64 - n1;
    if n1 == 0.0 || n0 == 0.0 { return 0.5; }
    (rank_sum - n1 * (n1 + 1.0) / 2.0) / (n1 * n0)
}

fn auc_se(a: f64, n1: f64, n0: f64) -> f64 {
    let (q1, q2) = (a / (2.0 - a), 2.0 * a * a / (1.0 + a));
    ((a * (1.0 - a) + (n1 - 1.0) * (q1 - a * a) + (n0 - 1.0) * (q2 - a * a)) / (n1 * n0)).max(0.0).sqrt()
}

async fn train(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let ds = dataset(&st.db, &p).await?;
    let all = feature_rows(&st.db, &ds, true).await?;
    if all.len() < 200 { return Err(AppError::Unprocessable(format!("need at least 200 closed deals to train; found {}", all.len()))); }
    let test_frac: f64 = p.get("test_fraction").and_then(|v| v.parse().ok()).unwrap_or(0.2f64).clamp(0.1, 0.5);
    let cutoff = all[((all.len() as f64) * (1.0 - test_frac)) as usize].close.ok_or_else(|| AppError::Unprocessable("closed deals need close dates".into()))?;
    let (train_rows, test_rows): (Vec<&Row>, Vec<&Row>) = all.iter().partition(|r| r.close.map(|c| c < cutoff).unwrap_or(true));
    if train_rows.is_empty() || test_rows.is_empty() { return Err(AppError::Unprocessable("time split produced an empty train or test set".into())); }
    let enc = Encoder::fit(&train_rows);
    let xtr: Vec<Vec<f64>> = train_rows.iter().map(|r| enc.encode(r)).collect();
    let ytr: Vec<f64> = train_rows.iter().map(|r| f64::from(r.won)).collect();
    let l2: f64 = p.get("l2").and_then(|v| v.parse().ok()).unwrap_or(0.05);
    let (w, b) = fit_logistic(&xtr, &ytr, l2);
    let base = ytr.iter().sum::<f64>() / ytr.len() as f64;
    let yte: Vec<f64> = test_rows.iter().map(|r| f64::from(r.won)).collect();
    let pte: Vec<f64> = test_rows.iter().map(|r| sigmoid(b + enc.encode(r).iter().zip(&w).map(|(a, c)| a * c).sum::<f64>())).collect();
    let n = yte.len() as f64;
    let brier = pte.iter().zip(&yte).map(|(p, y)| (p - y).powi(2)).sum::<f64>() / n;
    let brier_base = yte.iter().map(|y| (base - y).powi(2)).sum::<f64>() / n;
    let ll = |p: f64, y: f64| -(y * p.max(1e-9).ln() + (1.0 - y) * (1.0 - p).max(1e-9).ln());
    let logloss = pte.iter().zip(&yte).map(|(p, y)| ll(*p, *y)).sum::<f64>() / n;
    let logloss_base = yte.iter().map(|y| ll(base, *y)).sum::<f64>() / n;
    let acc = pte.iter().zip(&yte).filter(|(p, y)| (**p >= 0.5) == (**y > 0.5)).count() as f64 / n;
    let acc_base = yte.iter().filter(|y| (base >= 0.5) == (**y > 0.5)).count() as f64 / n;
    let a = auc(&pte, &yte);
    let n1 = yte.iter().sum::<f64>(); let se = auc_se(a, n1, n - n1);
    let (lo, hi) = (a - 1.96 * se, a + 1.96 * se);
    let skill = 1.0 - brier / brier_base;
    let usable = lo > 0.5 && skill > 0.0;
    let mut order: Vec<usize> = (0..pte.len()).collect(); order.sort_by(|&i, &j| pte[i].partial_cmp(&pte[j]).unwrap());
    let calib: Vec<J> = order.chunks(order.len().div_ceil(5).max(1)).map(|c| json!({ "n": c.len(), "mean_predicted": r1(c.iter().map(|&i| pte[i]).sum::<f64>() / c.len() as f64), "actual_win_rate": r1(c.iter().map(|&i| yte[i]).sum::<f64>() / c.len() as f64) })).collect();
    let names = enc.names();
    let mut top: Vec<(String, f64)> = names.iter().cloned().zip(w.iter().cloned()).collect();
    top.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap());
    let metrics = json!({ "auc": r1(a), "auc_ci95": [r1(lo), r1(hi)], "brier": r1(brier), "brier_baseline": r1(brier_base), "brier_skill": r1(skill),
        "log_loss": r1(logloss), "log_loss_baseline": r1(logloss_base), "accuracy": r1(acc), "accuracy_baseline": r1(acc_base),
        "train_win_rate": r1(base), "test_win_rate": r1(yte.iter().sum::<f64>() / n), "calibration_quintiles": calib,
        "usable_rule": "usable only if the lower 95% bound of holdout AUC > 0.5 AND Brier skill vs base rate > 0" });
    let coeffs = json!({ "intercept": b, "weights": names.iter().cloned().zip(w.iter().cloned()).collect::<BTreeMap<_, _>>(),
        "levels": CAT_NAMES.iter().zip(&enc.levels).map(|(c, l)| (c.to_string(), l.clone())).collect::<BTreeMap<_, _>>(), "num_mean": enc.mean, "num_std": enc.std, "base_rate": base, "l2": l2 });
    sea_orm::ConnectionTrait::execute_unprepared(&st.db, "UPDATE crm_models SET is_active=false WHERE name='deal_win_probability'").await?;
    let m = row(&st.db, "INSERT INTO crm_models (name, algorithm, is_active, usable, n_train, n_test, cutoff_date, metrics, coefficients) VALUES ('deal_win_probability','logistic_regression_l2',true,$1,$2,$3,$4::date,$5::text::jsonb,$6::text::jsonb) RETURNING id::text AS id",
        vec![usable.into(), (train_rows.len() as i32).into(), (test_rows.len() as i32).into(), cutoff.to_string().into(), metrics.to_string().into(), coeffs.to_string().into()]).await?;
    Ok(Json(json!({ "model_id": m["id"], "dataset": ds, "split": { "method": "time-based (train on earlier closes, test on later)", "cutoff_date": cutoff, "n_train": train_rows.len(), "n_test": test_rows.len() },
        "metrics": metrics, "usable": usable, "largest_coefficients": top.iter().take(8).map(|(n, w)| json!({ "feature": n, "weight": r1(*w) })).collect::<Vec<_>>(),
        "verdict": if usable { "model beats the base rate on held-out deals and will be used to score open deals" }
                   else { "NO PREDICTIVE SIGNAL: on held-out deals the model is not better than predicting the overall win rate. Open deals will be scored with the base win rate instead of model output." } })))
}

async fn model_info(State(st): State<AppState>) -> AppResult<Json<J>> {
    let m = row(&st.db, "SELECT id::text AS id, name, algorithm, trained_at, usable, n_train, n_test, cutoff_date::text AS cutoff_date, metrics FROM crm_models WHERE name='deal_win_probability' AND is_active ORDER BY trained_at DESC LIMIT 1", vec![]).await?;
    if m.is_null() { return Err(AppError::NotFound(None)); }
    Ok(Json(m))
}

async fn score_open(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let m = row(&st.db, "SELECT id::text AS id, usable, coefficients FROM crm_models WHERE name='deal_win_probability' AND is_active ORDER BY trained_at DESC LIMIT 1", vec![]).await?;
    if m.is_null() { return Err(AppError::Conflict("no trained model; call POST /api/crm/predict/train first".into())); }
    let ds = dataset(&st.db, &p).await?;
    let usable = m["usable"] == true;
    let c = &m["coefficients"];
    let base = c["base_rate"].as_f64().unwrap_or(0.5);
    let open = feature_rows(&st.db, &ds, false).await?;
    let probs: Vec<(String, f64)> = if usable {
        let levels: Vec<Vec<String>> = CAT_NAMES.iter().map(|n| c["levels"][*n].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()).collect();
        let f = |k: &str| c[k].as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect::<Vec<_>>()).unwrap_or_default();
        let enc = Encoder { levels, mean: f("num_mean"), std: f("num_std") };
        let b = c["intercept"].as_f64().unwrap_or(0.0);
        open.iter().map(|r| { let x = enc.encode(r); let z = b + enc.names().iter().zip(&x).map(|(nm, v)| c["weights"][nm].as_f64().unwrap_or(0.0) * v).sum::<f64>(); (r.id.clone(), sigmoid(z)) }).collect()
    } else { open.iter().map(|r| (r.id.clone(), base)).collect() };
    for part in probs.chunks(1000) {
        let mut vals: Vec<Value> = vec![]; let mut g = vec![];
        for (id, pr) in part { vals.push(id.clone().into()); vals.push(format!("{pr:.6}").into()); g.push(format!("(${}::uuid,${}::float8)", vals.len() - 1, vals.len())); }
        row(&st.db, &format!("UPDATE crm_opportunities o SET ml_win_probability=v.p FROM (VALUES {}) AS v(id,p) WHERE o.id=v.id RETURNING 1 AS x", g.join(",")), vals).await?;
    }
    let s = row(&st.db, "SELECT count(*) AS deals, round(avg(ml_win_probability)::numeric,3)::float8 AS mean_probability, COALESCE(sum(amount),0)::float8 AS open_amount,
        COALESCE(sum(amount*ml_win_probability),0)::float8 AS expected_revenue FROM crm_opportunities WHERE stage NOT IN ('closed_won','closed_lost') AND ($1::text IS NULL OR source_dataset=$1)", vec![ds.clone().into()]).await?;
    Ok(Json(json!({ "dataset": ds, "method": if usable { "logistic_regression" } else { "base_rate (model not better than baseline)" }, "scored": probs.len(), "summary": s })))
}

// ---------------------------------------------------------------- forecast

fn holt(y: &[f64], a: f64, b: f64) -> (f64, f64, f64) {
    let (mut l, mut t) = (y[0], if y.len() > 1 { y[1] - y[0] } else { 0.0 });
    let mut sse = 0.0;
    for &v in &y[1..] { let f = l + t; sse += (v - f).powi(2); let nl = a * v + (1.0 - a) * (l + t); t = b * (nl - l) + (1.0 - b) * t; l = nl; }
    (l, t, sse)
}
fn fit_holt(y: &[f64]) -> (f64, f64) {
    let mut best = (f64::MAX, 0.5, 0.1);
    for ai in 1..=9 { for bi in 1..=9 { let (a, b) = (ai as f64 / 10.0, bi as f64 / 10.0); let (_, _, s) = holt(y, a, b); if s < best.0 { best = (s, a, b); } } }
    (best.1, best.2)
}
/// one-step-ahead prediction for each method given a history slice
fn predict(method: &str, h: &[f64], steps: usize) -> f64 {
    match method {
        "naive_last" => *h.last().unwrap(),
        "mean_last_3" => { let k = h.len().min(3); h[h.len() - k..].iter().sum::<f64>() / k as f64 }
        _ => { let (a, b) = fit_holt(h); let (l, t, _) = holt(h, a, b); (l + t * steps as f64).max(0.0) }
    }
}

async fn forecast(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let ds = dataset(&st.db, &p).await?;
    let horizon: usize = p.get("horizon").and_then(|v| v.parse().ok()).unwrap_or(3).clamp(1, 12);
    let hist = rows(&st.db, "SELECT to_char(date_trunc('month', close_date),'YYYY-MM') AS month, sum(close_value)::float8 AS revenue, count(*) AS deals FROM crm_opportunities
        WHERE stage='closed_won' AND close_date IS NOT NULL AND ($1::text IS NULL OR source_dataset=$1) GROUP BY 1 ORDER BY 1", vec![ds.clone().into()]).await?;
    if hist.len() < 6 { return Err(AppError::Unprocessable(format!("need at least 6 months of closed-won history; found {}", hist.len()))); }
    let y: Vec<f64> = hist.iter().map(|h| h["revenue"].as_f64().unwrap_or(0.0)).collect();
    let methods = ["naive_last", "mean_last_3", "holt_linear"];
    let start = (y.len() / 2).max(4).min(y.len() - 1);
    let mut bt = vec![];
    let mut best = ("naive_last", f64::MAX, 0.0);
    for m in methods {
        let (mut ape, mut se, mut k) = (0.0, 0.0, 0.0);
        for t in start..y.len() { let f = predict(m, &y[..t], 1); se += (y[t] - f).powi(2); if y[t] > 0.0 { ape += (y[t] - f).abs() / y[t]; } k += 1.0; }
        let (mape, rmse) = (ape / k * 100.0, (se / k).sqrt());
        if mape < best.1 { best = (m, mape, rmse); }
        bt.push(json!({ "method": m, "mape_pct": (mape * 10.0).round() / 10.0, "rmse": rmse.round(), "origins_tested": k }));
    }
    let last_month: NaiveDate = format!("{}-01", hist.last().unwrap()["month"].as_str().unwrap()).parse().unwrap();
    let shift = |d: NaiveDate, m: u32| { let t = d.year() * 12 + d.month0() as i32 + m as i32; NaiveDate::from_ymd_opt(t / 12, (t % 12) as u32 + 1, 1).unwrap() };
    let fc: Vec<J> = (1..=horizon).map(|h| {
        let pt = predict(best.0, &y, h);
        let half = 1.2816 * best.2 * (h as f64).sqrt();
        json!({ "month": shift(last_month, h as u32).format("%Y-%m").to_string(), "forecast": pt.round(), "low_80": (pt - half).max(0.0).round(), "high_80": (pt + half).round() })
    }).collect();
    // pipeline view: open deals expected to close engage_date + median cycle
    let cyc = row(&st.db, "SELECT COALESCE(percentile_cont(0.5) WITHIN GROUP (ORDER BY (close_date-engage_date)),45)::float8 AS m, COALESCE(percentile_cont(0.9) WITHIN GROUP (ORDER BY (close_date-engage_date)),90)::float8 AS p90 FROM crm_opportunities WHERE stage='closed_won' AND engage_date IS NOT NULL AND close_date IS NOT NULL AND ($1::text IS NULL OR source_dataset=$1)", vec![ds.clone().into()]).await?;
    let med = cyc["m"].as_f64().unwrap_or(45.0).round() as i64;
    let p90 = cyc["p90"].as_f64().unwrap_or(90.0).round() as i64;
    let as_of: NaiveDate = match p.get("as_of") { Some(a) => a.parse().map_err(|_| AppError::BadRequest("as_of must be YYYY-MM-DD".into()))?, None => last_month + Duration::days(30) - Duration::days(1) };
    let open = rows(&st.db, "SELECT amount::float8 AS amount, COALESCE(ml_win_probability, probability/100.0, 0)::float8 AS p, engage_date::text AS engage_date FROM crm_opportunities WHERE stage NOT IN ('closed_won','closed_lost') AND ($1::text IS NULL OR source_dataset=$1)", vec![ds.clone().into()]).await?;
    let mut by_month: BTreeMap<String, (f64, i64)> = BTreeMap::new();
    let (mut stale_value, mut stale_deals) = (0.0, 0i64);
    let first_future = shift(last_month, 1);
    for o in &open {
        let eng: NaiveDate = o["engage_date"].as_str().and_then(|d| d.parse().ok()).unwrap_or(as_of);
        let mut m = eng + Duration::days(med); m = NaiveDate::from_ymd_opt(m.year(), m.month(), 1).unwrap();
        if m < first_future { m = first_future; }
        let v = o["amount"].as_f64().unwrap_or(0.0) * o["p"].as_f64().unwrap_or(0.0);
        if o["engage_date"].as_str().is_some() && (as_of - eng).num_days() > p90 { stale_value += v; stale_deals += 1; continue; }
        let e = by_month.entry(m.format("%Y-%m").to_string()).or_default();
        e.0 += v; e.1 += 1;
    }
    let pipe: Vec<J> = by_month.iter().map(|(m, (v, n))| json!({ "month": m, "expected_revenue": v.round(), "deals": n })).collect();
    let total: f64 = by_month.values().map(|v| v.0).sum();
    let hist_out: Vec<J> = hist.iter().map(|h| json!({ "month": h["month"], "revenue": h["revenue"], "deals": h["deals"] })).collect();
    Ok(Json(json!({ "dataset": ds, "history": hist_out,
        "backtest": { "scheme": "rolling-origin, one step ahead", "results": bt, "selected": best.0, "selected_mape_pct": (best.1 * 10.0).round() / 10.0 },
        "forecast": fc, "pipeline_view": { "method": format!("sum(amount x win probability) placed at engage_date + median cycle ({med} days); deals open longer than the historical 90th-percentile cycle ({p90} days) are treated as stale and excluded"),
            "by_month": pipe, "total_expected_live": total.round(), "excluded_stale": { "deals": stale_deals, "expected_revenue": stale_value.round() } },
        "caveats": [format!("only {} months of history: no seasonality can be estimated and intervals are wide", y.len()),
                    "if the selected method's backtest MAPE is high, treat the forecast as indicative; the pipeline view is an independent cross-check"] })))
}

async fn weighted_pipeline(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let ds = dataset(&st.db, &p).await?;
    let by_stage = rows(&st.db, "SELECT o.stage, count(*) AS deals, COALESCE(sum(o.amount),0)::float8 AS amount, round(avg(o.probability),1)::float8 AS avg_probability_pct,
        COALESCE(sum(o.amount*o.probability/100.0),0)::float8 AS weighted_revenue FROM crm_opportunities o WHERE o.stage NOT IN ('closed_won','closed_lost') AND ($1::text IS NULL OR o.source_dataset=$1)
        GROUP BY o.stage ORDER BY array_position(ARRAY['prospecting','qualification','proposal','negotiation'], o.stage)", vec![ds.clone().into()]).await?;
    let by_agent = rows(&st.db, "SELECT COALESCE(a.name,'(none)') AS agent, count(*) AS deals, COALESCE(sum(o.amount*o.probability/100.0),0)::float8 AS weighted_revenue FROM crm_opportunities o LEFT JOIN crm_sales_agents a ON a.id=o.sales_agent_id
        WHERE o.stage NOT IN ('closed_won','closed_lost') AND ($1::text IS NULL OR o.source_dataset=$1) GROUP BY 1 ORDER BY 3 DESC LIMIT 15", vec![ds.clone().into()]).await?;
    let t = row(&st.db, "SELECT COALESCE(sum(amount),0)::float8 AS open_amount, COALESCE(sum(amount*probability/100.0),0)::float8 AS weighted_forecast,
        COALESCE((SELECT sum(close_value) FROM crm_opportunities WHERE stage='closed_won' AND ($1::text IS NULL OR source_dataset=$1)),0)::float8 AS won_to_date
        FROM crm_opportunities WHERE stage NOT IN ('closed_won','closed_lost') AND ($1::text IS NULL OR source_dataset=$1)", vec![ds.clone().into()]).await?;
    Ok(Json(json!({ "dataset": ds, "method": "sum(amount x stage probability)", "totals": t, "by_stage": by_stage, "top_agents": by_agent })))
}
