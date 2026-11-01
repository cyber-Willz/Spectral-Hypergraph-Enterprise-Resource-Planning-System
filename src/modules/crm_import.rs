//! CRM dataset import: cleaning, validation, quarantine and bulk upserts.
//! One endpoint per file type: POST /api/crm/import/{kind} with the raw CSV as the body.
//! kinds: accounts, products, sales-teams, pipeline, funnel, product-info, customers, sales-orders, donors
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::post;
use axum::{Extension, Json, Router};
use chrono::NaiveDate;
use sea_orm::{ConnectionTrait, TransactionTrait, Value};
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/crm/import/{kind}", post(import).layer(DefaultBodyLimit::max(25 * 1024 * 1024)))
}

// ------------------------------------------------------------------ helpers

pub fn norm_key(s: &str) -> String { s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase() }
fn norm(s: &str) -> String { s.trim().to_lowercase().replace('_', " ").split_whitespace().collect::<Vec<_>>().join(" ") }
fn pkey(s: &str) -> String { norm_key(s) }

fn lev(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() { cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1])); }
        prev = cur;
    }
    prev[b.len()]
}

fn is_subseq(small: &str, big: &str) -> bool {
    let mut it = big.chars();
    small.chars().all(|c| it.any(|x| x == c))
}

/// Maps a messy value onto a canonical set: exact (case/space-insensitive), then abbreviation typos
/// ("gld" -> gold, "delrd" -> delivered; same first letter, characters in order, unique), then edit distance <= 2 (unique).
pub fn canon(raw: &str, set: &[&str]) -> Option<String> {
    let n = norm(raw);
    if let Some(c) = set.iter().find(|c| norm(c) == n) { return Some(c.to_string()); }
    if n.len() >= 3 {
        let a: Vec<&&str> = set.iter().filter(|c| { let cn = norm(c); cn.chars().next() == n.chars().next() && is_subseq(&n, &cn) }).collect();
        if a.len() == 1 { return Some(a[0].to_string()); }
        let b: Vec<&&str> = set.iter().filter(|c| lev(&n, &norm(c)) <= 2).collect();
        if b.len() == 1 { return Some(b[0].to_string()); }
    }
    None
}

fn word_number(s: &str) -> Option<i32> {
    ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"].iter().position(|w| *w == norm(s)).map(|i| i as i32)
}

fn dmy(s: &str) -> Option<NaiveDate> { NaiveDate::parse_from_str(s.trim(), "%d-%m-%y").ok() }
fn mdy(s: &str) -> Option<NaiveDate> { NaiveDate::parse_from_str(s.trim(), "%m/%d/%Y").ok() }
fn iso(s: &str) -> Option<NaiveDate> { NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok() }
fn money(s: &str) -> Option<f64> { s.replace([',', '$', ' '], "").parse::<f64>().ok().filter(|v| v.is_finite()) }
fn m2(v: f64) -> String { format!("{v:.2}") }
type Cell = Option<String>;

struct Rec { line: u64, m: HashMap<String, String> }
impl Rec {
    fn get(&self, k: &str) -> Option<&str> { self.m.get(k).map(|s| s.as_str()).filter(|s| !s.is_empty()) }
    fn raw(&self) -> J { json!(self.m) }
}

#[derive(Default)]
struct Report {
    total: u64, inserted: u64, updated: u64,
    rejects: Vec<(u64, String, J)>,
    corrections: BTreeMap<String, BTreeMap<String, u64>>,
    warnings: Vec<String>,
}
impl Report {
    fn reject(&mut self, r: &Rec, why: impl Into<String>) { self.rejects.push((r.line, why.into(), r.raw())); }
    fn fix(&mut self, field: &str, from: &str, to: &str) {
        if from.trim() != to { *self.corrections.entry(field.into()).or_default().entry(format!("{}→{}", from.trim(), to)).or_insert(0) += 1; }
    }
    fn note(&mut self, field: &str, what: &str) { *self.corrections.entry(field.into()).or_default().entry(what.into()).or_insert(0) += 1; }
}

fn read_csv(text: &str) -> AppResult<Vec<Rec>> {
    let text = text.trim_start_matches('\u{feff}');
    let mut rdr = csv::ReaderBuilder::new().trim(csv::Trim::All).flexible(true).from_reader(text.as_bytes());
    let headers: Vec<String> = rdr.headers().map_err(|e| AppError::BadRequest(format!("unreadable CSV header: {e}")))?.iter().map(norm_key).collect();
    let mut out = vec![];
    for r in rdr.records() {
        let r = r.map_err(|e| AppError::BadRequest(format!("malformed CSV: {e}")))?;
        let line = r.position().map(|p| p.line()).unwrap_or(0);
        out.push(Rec { line, m: headers.iter().cloned().zip(r.iter().map(|s| s.trim().to_string())).collect() });
    }
    Ok(out)
}

fn require(recs: &[Rec], cols: &[&str]) -> AppResult<()> {
    let first = recs.first().ok_or_else(|| AppError::BadRequest("CSV has no data rows".into()))?;
    for c in cols { if !first.m.contains_key(*c) { return Err(AppError::BadRequest(format!("missing required column '{c}'"))); } }
    Ok(())
}

/// Multi-row upsert. All values travel as text and are cast in SQL. Returns (inserted, updated).
async fn bulk(tx: &impl ConnectionTrait, table: &str, cols: &[(&str, &str)], data: &[Vec<Cell>], conflict: &str) -> AppResult<(u64, u64)> {
    if data.is_empty() { return Ok((0, 0)); }
    let chunk = (30000 / cols.len()).min(1000);
    let names = cols.iter().map(|c| c.0).collect::<Vec<_>>().join(",");
    let sets = cols.iter().filter(|c| c.0 != conflict).map(|c| format!("{0}=EXCLUDED.{0}", c.0)).collect::<Vec<_>>().join(",");
    let (mut ins, mut upd) = (0, 0);
    for part in data.chunks(chunk) {
        let mut vals: Vec<Value> = vec![];
        let mut groups = vec![];
        for r in part {
            let mut g = vec![];
            for (i, c) in cols.iter().enumerate() { vals.push(r[i].clone().into()); g.push(format!("${}::{}", vals.len(), c.1)); }
            groups.push(format!("({})", g.join(",")));
        }
        let sql = format!("INSERT INTO {table} ({names}) VALUES {} ON CONFLICT ({conflict}) DO UPDATE SET {sets} RETURNING (xmax = 0) AS inserted", groups.join(","));
        for r in rows(tx, &sql, vals).await? { if r["inserted"] == true { ins += 1 } else { upd += 1 } }
    }
    Ok((ins, upd))
}

async fn id_map(tx: &impl ConnectionTrait, sql: &str) -> AppResult<HashMap<String, String>> {
    Ok(rows(tx, sql, vec![]).await?.iter().filter_map(|r| Some((r["k"].as_str()?.to_string(), r["id"].as_str()?.to_string()))).collect())
}

fn s(v: Option<&str>) -> Cell { v.map(String::from) }
fn n<T: ToString>(v: Option<T>) -> Cell { v.map(|x| x.to_string()) }

// ----------------------------------------------------------------- endpoint

async fn import(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Path(kind): Path<String>, Query(p): Query<HashMap<String, String>>, body: String) -> AppResult<Json<J>> {
    u.require(&["admin", "manager", "user"])?;
    let recs = read_csv(&body)?;
    let mut rep = Report { total: recs.len() as u64, ..Default::default() };
    let tx = st.db.begin().await?;
    match kind.as_str() {
        "accounts" => accounts(&tx, &recs, &mut rep).await?,
        "products" => products(&tx, &recs, &mut rep).await?,
        "sales-teams" => sales_teams(&tx, &recs, &mut rep).await?,
        "pipeline" => pipeline(&tx, &recs, &mut rep).await?,
        "funnel" => funnel(&tx, &recs, &mut rep).await?,
        "product-info" => product_info(&tx, &recs, &mut rep).await?,
        "customers" => customers(&tx, &recs, &mut rep).await?,
        "sales-orders" => sales_orders(&tx, &recs, &mut rep).await?,
        "donors" => donors(&tx, &recs, &mut rep).await?,
        _ => return Err(AppError::BadRequest("kind must be one of: accounts, products, sales-teams, pipeline, funnel, product-info, customers, sales-orders, donors".into())),
    }
    if p.get("strict").map(|v| v == "true").unwrap_or(false) && !rep.rejects.is_empty() {
        return Err(AppError::Unprocessable(format!("strict import aborted: {} rejected row(s); first: line {} - {}", rep.rejects.len(), rep.rejects[0].0, rep.rejects[0].1)));
    }
    let b = row(&tx, "INSERT INTO crm_import_batches (kind, filename, rows_total, rows_inserted, rows_updated, rows_rejected, corrections, warnings, created_by)
        VALUES ($1,$2,$3,$4,$5,$6,$7::text::jsonb,$8::text::jsonb,$9) RETURNING id::text AS id",
        vec![kind.clone().into(), p.get("filename").cloned().into(), (rep.total as i32).into(), (rep.inserted as i32).into(), (rep.updated as i32).into(),
             (rep.rejects.len() as i32).into(), json!(rep.corrections).to_string().into(), json!(rep.warnings).to_string().into(), u.id.into()]).await?;
    let bid: Uuid = b["id"].as_str().unwrap().parse().unwrap();
    let rej: Vec<Vec<Cell>> = rep.rejects.iter().map(|(l, w, raw)| vec![Some(bid.to_string()), Some(l.to_string()), Some(w.clone()), Some(raw.to_string())]).collect();
    for part in rej.chunks(2000) {
        let mut vals: Vec<Value> = vec![]; let mut g = vec![];
        for r in part { let base = vals.len(); for c in r { vals.push(c.clone().into()); } g.push(format!("(${}::uuid,${}::int,${},${}::text::jsonb)", base + 1, base + 2, base + 3, base + 4)); }
        row(&tx, &format!("INSERT INTO crm_import_rejects (batch_id, line, reason, raw) VALUES {} RETURNING 1 AS x", g.join(",")), vals).await?;
    }
    tx.commit().await?;
    let mut reasons: BTreeMap<String, u64> = BTreeMap::new();
    for (_, w, _) in &rep.rejects { *reasons.entry(w.clone()).or_default() += 1; }
    Ok(Json(json!({ "batch_id": bid, "kind": kind, "rows": rep.total, "inserted": rep.inserted, "updated": rep.updated, "rejected": rep.rejects.len(),
        "reject_reasons": reasons, "corrections": rep.corrections, "warnings": rep.warnings,
        "first_rejects": rep.rejects.iter().take(10).map(|(l, w, _)| json!({ "line": l, "reason": w })).collect::<Vec<_>>() })))
}

// ------------------------------------------------- opportunities dataset

async fn accounts(tx: &impl ConnectionTrait, recs: &[Rec], rep: &mut Report) -> AppResult<()> {
    require(recs, &["account", "sector", "revenue", "employees", "officelocation"])?;
    let (mut data, mut seen, mut parents) = (vec![], HashSet::new(), vec![]);
    for r in recs {
        let Some(name) = r.get("account") else { rep.reject(r, "account name is empty"); continue };
        if !seen.insert(name.to_lowercase()) { rep.reject(r, "duplicate account in file"); continue; }
        let Some(rev) = r.get("revenue").and_then(money) else { rep.reject(r, "invalid revenue"); continue };
        let emp = r.get("employees").and_then(|e| e.parse::<i32>().ok()).filter(|e| *e >= 0);
        if r.get("employees").is_some() && emp.is_none() { rep.reject(r, "invalid employees"); continue; }
        if let Some(p) = r.get("subsidiaryof") { parents.push((name.to_string(), p.to_string())); }
        data.push(vec![Some(format!("acct:{}", name.to_lowercase())), Some(name.into()), s(r.get("sector")), n(r.get("yearestablished").and_then(|y| y.parse::<i32>().ok())),
            Some(m2(rev * 1_000_000.0)), n(emp), s(r.get("officelocation")), Some("customer".into()), Some("active".into())]);
    }
    let (i, u) = bulk(tx, "crm_accounts", &[("external_ref", "text"), ("name", "text"), ("industry", "text"), ("year_established", "int"), ("annual_revenue", "numeric"),
        ("employees", "int"), ("country", "text"), ("account_type", "text"), ("status", "text")], &data, "external_ref").await?;
    rep.inserted += i; rep.updated += u;
    rep.warnings.push("revenue converted from millions of USD to USD".into());
    let ids = id_map(tx, "SELECT external_ref AS k, id::text AS id FROM crm_accounts WHERE external_ref LIKE 'acct:%'").await?;
    for (child, parent) in parents {
        match (ids.get(&format!("acct:{}", child.to_lowercase())), ids.get(&format!("acct:{}", parent.to_lowercase()))) {
            (Some(c), Some(p)) => { row(tx, "UPDATE crm_accounts SET parent_account_id=$2::uuid WHERE id=$1::uuid RETURNING 1 AS x", vec![c.clone().into(), p.clone().into()]).await?; }
            _ => rep.warnings.push(format!("parent account '{parent}' of '{child}' not found")),
        }
    }
    Ok(())
}

async fn products(tx: &impl ConnectionTrait, recs: &[Rec], rep: &mut Report) -> AppResult<()> {
    require(recs, &["product", "salesprice"])?;
    let mut data = vec![];
    for r in recs {
        let (Some(name), Some(price)) = (r.get("product"), r.get("salesprice").and_then(money)) else { rep.reject(r, "missing product or invalid sales_price"); continue };
        data.push(vec![Some(format!("prod:{}", pkey(name))), Some(name.into()), s(r.get("series")), s(r.get("series")), Some(m2(price)), Some("crm_sales_opportunities".into())]);
    }
    let (i, u) = bulk(tx, "crm_products", &[("product_code", "text"), ("name", "text"), ("category", "text"), ("series", "text"), ("list_price", "numeric"), ("source_dataset", "text")], &data, "product_code").await?;
    rep.inserted += i; rep.updated += u;
    Ok(())
}

async fn sales_teams(tx: &impl ConnectionTrait, recs: &[Rec], rep: &mut Report) -> AppResult<()> {
    require(recs, &["salesagent", "manager", "regionaloffice"])?;
    let (mut data, mut seen) = (vec![], HashSet::new());
    for r in recs {
        let Some(a) = r.get("salesagent") else { rep.reject(r, "sales agent is empty"); continue };
        if !seen.insert(a.to_lowercase()) { rep.reject(r, "duplicate agent in file"); continue; }
        data.push(vec![Some(a.into()), s(r.get("manager")), s(r.get("regionaloffice")), Some("crm_sales_opportunities".into())]);
    }
    let (i, u) = bulk(tx, "crm_sales_agents", &[("name", "text"), ("manager_name", "text"), ("regional_office", "text"), ("source_dataset", "text")], &data, "name").await?;
    rep.inserted += i; rep.updated += u;
    Ok(())
}

const OPP_COLS: &[(&str, &str)] = &[("external_ref", "text"), ("name", "text"), ("account_id", "uuid"), ("product_id", "uuid"), ("sales_agent_id", "uuid"), ("stage", "text"),
    ("probability", "int"), ("amount", "numeric"), ("close_value", "numeric"), ("engage_date", "date"), ("close_date", "date"), ("closed_at", "timestamptz"),
    ("source_stage", "text"), ("source_dataset", "text")];

async fn pipeline(tx: &impl ConnectionTrait, recs: &[Rec], rep: &mut Report) -> AppResult<()> {
    require(recs, &["opportunityid", "salesagent", "product", "account", "dealstage", "closevalue"])?;
    let accts = id_map(tx, "SELECT external_ref AS k, id::text AS id FROM crm_accounts WHERE external_ref LIKE 'acct:%'").await?;
    let prods = id_map(tx, "SELECT product_code AS k, id::text AS id FROM crm_products WHERE product_code LIKE 'prod:%'").await?;
    let names: HashMap<String, String> = rows(tx, "SELECT product_code, name FROM crm_products WHERE product_code LIKE 'prod:%'", vec![]).await?
        .iter().map(|r| (r["product_code"].as_str().unwrap().to_string(), r["name"].as_str().unwrap().to_string())).collect();
    let agents = id_map(tx, "SELECT lower(name) AS k, id::text AS id FROM crm_sales_agents").await?;
    let list_price: HashMap<String, f64> = rows(tx, "SELECT product_code, list_price::float8 AS p FROM crm_products WHERE product_code LIKE 'prod:%'", vec![]).await?
        .iter().map(|r| (r["product_code"].as_str().unwrap().to_string(), r["p"].as_f64().unwrap_or(0.0))).collect();
    if accts.is_empty() || prods.is_empty() || agents.is_empty() {
        return Err(AppError::Unprocessable("import accounts, products and sales-teams before the pipeline".into()));
    }
    let (mut data, mut seen) = (vec![], HashSet::new());
    for r in recs {
        let Some(oid) = r.get("opportunityid") else { rep.reject(r, "opportunity_id is empty"); continue };
        if !seen.insert(oid.to_string()) { rep.reject(r, "duplicate opportunity_id in file"); continue; }
        let Some(stage_raw) = r.get("dealstage") else { rep.reject(r, "deal_stage is empty"); continue };
        let Some(stage) = canon(stage_raw, &["Prospecting", "Engaging", "Won", "Lost"]) else { rep.reject(r, format!("unknown deal_stage '{stage_raw}'")); continue };
        let Some(agent) = r.get("salesagent").and_then(|a| agents.get(&a.to_lowercase())) else { rep.reject(r, "unknown sales agent"); continue };
        let pc = format!("prod:{}", pkey(r.get("product").unwrap_or("")));
        let Some(prod) = prods.get(&pc) else { rep.reject(r, format!("unknown product '{}'", r.get("product").unwrap_or(""))); continue };
        if let (Some(raw), Some(canonical)) = (r.get("product"), names.get(&pc)) { rep.fix("product", raw, canonical); }
        let acct = match r.get("account") {
            None => None,
            Some(a) => match accts.get(&format!("acct:{}", a.to_lowercase())) { Some(id) => Some(id.clone()), None => { rep.reject(r, format!("unknown account '{a}'")); continue } },
        };
        let (engage, close) = (r.get("engagedate").and_then(iso), r.get("closedate").and_then(iso));
        if r.get("engagedate").is_some() && engage.is_none() || r.get("closedate").is_some() && close.is_none() { rep.reject(r, "invalid date"); continue; }
        if let (Some(e), Some(c)) = (engage, close) { if c < e { rep.reject(r, "close_date before engage_date"); continue; } }
        let value = r.get("closevalue").and_then(money);
        let price = list_price.get(&pc).copied().unwrap_or(0.0);
        let (st, closed, amount, cv) = match stage.as_str() {
            "Won" => { let Some(v) = value.filter(|v| *v > 0.0) else { rep.reject(r, "won deal without positive close_value"); continue }; ("closed_won", true, v, Some(v)) }
            "Lost" => ("closed_lost", true, price, Some(0.0)),
            "Engaging" => ("qualification", false, price, None),
            _ => ("prospecting", false, price, None),
        };
        if closed && close.is_none() { rep.reject(r, "closed deal without close_date"); continue; }
        let acct_name = r.get("account").unwrap_or("(no account yet)");
        data.push(vec![Some(format!("opp:{oid}")), Some(format!("{acct_name} - {} [{oid}]", names.get(&pc).map(String::as_str).unwrap_or(""))), acct, Some(prod.clone()), Some(agent.clone()),
            Some(st.into()), None, Some(m2(amount)), cv.map(m2), n(engage), n(close), if closed { close.map(|d| format!("{d}T12:00:00Z")) } else { None },
            Some(stage), Some("crm_sales_opportunities".into())]);
    }
    let (i, u) = bulk(tx, "crm_opportunities", OPP_COLS, &data, "external_ref").await?;
    rep.inserted += i; rep.updated += u;
    rep.warnings.push("open deals use the product list price as amount; lost deals keep the list price as the value that was lost (close_value = 0)".into());
    Ok(())
}

async fn funnel(tx: &impl ConnectionTrait, recs: &[Rec], rep: &mut Report) -> AppResult<()> {
    require(recs, &["orderid", "customername", "salesrep", "productline", "dealsize", "orderdate", "stage", "probability"])?;
    // reference data (accounts, agents, product lines) is created on the fly
    let mut accts = BTreeMap::new(); let mut agents = BTreeMap::new(); let mut lines = BTreeMap::new();
    for r in recs {
        if let Some(c) = r.get("customername") { accts.insert(c.to_lowercase(), c.to_string()); }
        if let Some(a) = r.get("salesrep") { agents.insert(a.to_lowercase(), a.to_string()); }
        if let Some(l) = r.get("productline") { lines.insert(l.to_lowercase(), l.to_string()); }
    }
    bulk(tx, "crm_accounts", &[("external_ref", "text"), ("name", "text"), ("account_type", "text")], &accts.iter().map(|(k, v)| vec![Some(format!("acct:{k}")), Some(v.clone()), Some("customer".into())]).collect::<Vec<_>>(), "external_ref").await?;
    bulk(tx, "crm_sales_agents", &[("name", "text"), ("source_dataset", "text")], &agents.values().map(|v| vec![Some(v.clone()), Some("sales_funnel".into())]).collect::<Vec<_>>(), "name").await?;
    bulk(tx, "crm_products", &[("product_code", "text"), ("name", "text"), ("category", "text"), ("source_dataset", "text")],
        &lines.iter().map(|(k, v)| vec![Some(format!("line:{k}")), Some(v.clone()), Some(v.clone()), Some("sales_funnel".into())]).collect::<Vec<_>>(), "product_code").await?;
    let aid = id_map(tx, "SELECT external_ref AS k, id::text AS id FROM crm_accounts WHERE external_ref LIKE 'acct:%'").await?;
    let gid = id_map(tx, "SELECT lower(name) AS k, id::text AS id FROM crm_sales_agents").await?;
    let pid = id_map(tx, "SELECT product_code AS k, id::text AS id FROM crm_products WHERE product_code LIKE 'line:%'").await?;
    let (mut data, mut seen, mut mismatches) = (vec![], HashSet::new(), 0);
    for r in recs {
        let Some(oid) = r.get("orderid") else { rep.reject(r, "OrderID is empty"); continue };
        if !seen.insert(oid.to_string()) { rep.reject(r, "duplicate OrderID in file"); continue; }
        let Some(stage_raw) = r.get("stage") else { rep.reject(r, "Stage is empty"); continue };
        let Some(stage) = canon(stage_raw, &["Lead", "MQL", "SQL", "Opportunity", "Closed Won", "Closed Lost"]) else { rep.reject(r, format!("unknown Stage '{stage_raw}'")); continue };
        let Some(deal) = r.get("dealsize").and_then(money).filter(|d| *d > 0.0) else { rep.reject(r, "invalid DealSize"); continue };
        let Some(date) = r.get("orderdate").and_then(iso) else { rep.reject(r, "invalid OrderDate"); continue };
        let Some(prob) = r.get("probability").and_then(|p| p.parse::<f64>().ok()).filter(|p| (0.0..=1.0).contains(p)) else { rep.reject(r, "invalid Probability"); continue };
        let (st, closed) = match stage.as_str() { "Lead" => ("prospecting", false), "MQL" => ("qualification", false), "SQL" => ("proposal", false), "Opportunity" => ("negotiation", false), "Closed Won" => ("closed_won", true), _ => ("closed_lost", true) };
        let won_rev = r.get("wonrevenue").and_then(money).unwrap_or(0.0);
        if stage == "Closed Won" && (won_rev - deal).abs() > 0.5 { rep.reject(r, "Closed Won deal whose Won Revenue differs from DealSize"); continue; }
        if let Some(f) = r.get("forecastedrevenue").and_then(money) { if !closed && (f - deal * prob).abs() > 0.5 { mismatches += 1; } }
        let acct = aid.get(&format!("acct:{}", r.get("customername").unwrap_or("").to_lowercase())).cloned();
        let agent = gid.get(&r.get("salesrep").unwrap_or("").to_lowercase()).cloned();
        let prod = pid.get(&format!("line:{}", r.get("productline").unwrap_or("").to_lowercase())).cloned();
        data.push(vec![Some(format!("funnel:{oid}")), Some(format!("{} - {} [{oid}]", r.get("customername").unwrap_or(""), r.get("productline").unwrap_or(""))), acct, prod, agent,
            Some(st.into()), if closed { None } else { Some(((prob * 100.0).round() as i32).to_string()) }, Some(m2(deal)), if closed { Some(m2(if stage == "Closed Won" { deal } else { 0.0 })) } else { None },
            Some(date.to_string()), if closed { Some(date.to_string()) } else { None }, if closed { Some(format!("{date}T12:00:00Z")) } else { None },
            Some(stage), Some("sales_funnel".into())]);
    }
    let (i, u) = bulk(tx, "crm_opportunities", OPP_COLS, &data, "external_ref").await?;
    rep.inserted += i; rep.updated += u;
    if mismatches > 0 { rep.warnings.push(format!("{mismatches} open deal(s) where Forecasted Revenue != DealSize x Probability")); }
    rep.warnings.push("source has a single OrderDate: used as engage_date and, for closed deals, as close_date".into());
    Ok(())
}

// ------------------------------------------------ customer behaviour dataset

async fn product_info(tx: &impl ConnectionTrait, recs: &[Rec], rep: &mut Report) -> AppResult<()> {
    require(recs, &["productid", "productname", "category", "baseprice"])?;
    let (mut data, mut seen) = (vec![], HashSet::new());
    for r in recs {
        let Some(id) = r.get("productid") else { rep.reject(r, "product_id is empty"); continue };
        if !seen.insert(id.to_string()) { rep.reject(r, "duplicate product_id in file"); continue; }
        let Some(price) = r.get("baseprice").and_then(money).filter(|p| *p >= 0.0) else { rep.reject(r, "invalid base_price"); continue };
        let launch = r.get("launchdate").and_then(dmy);
        if r.get("launchdate").is_some() && launch.is_none() { rep.reject(r, "invalid launch_date (expected dd-mm-yy)"); continue; }
        data.push(vec![Some(id.into()), s(r.get("productname")), s(r.get("category")), Some(m2(price)), s(r.get("suppliercode")), n(launch), Some("customer_behaviour".into())]);
    }
    let (i, u) = bulk(tx, "crm_products", &[("product_code", "text"), ("name", "text"), ("category", "text"), ("list_price", "numeric"), ("supplier_code", "text"), ("launch_date", "date"), ("source_dataset", "text")], &data, "product_code").await?;
    rep.inserted += i; rep.updated += u;
    Ok(())
}

const REGIONS: &[&str] = &["Central", "East", "North", "South", "West"];

async fn customers(tx: &impl ConnectionTrait, recs: &[Rec], rep: &mut Report) -> AppResult<()> {
    require(recs, &["customerid", "email", "signupdate", "gender", "region", "loyaltytier"])?;
    let (mut data, mut seen, mut emails, mut dup_emails, mut bad_email) = (vec![], HashSet::new(), HashSet::new(), 0, 0);
    for r in recs {
        let Some(id) = r.get("customerid") else { rep.reject(r, "customer_id is empty"); continue };
        if !seen.insert(id.to_string()) { rep.reject(r, "duplicate customer_id in file"); continue; }
        let signup = r.get("signupdate").and_then(dmy);
        if r.get("signupdate").is_some() && signup.is_none() { rep.reject(r, "invalid signup_date (expected dd-mm-yy)"); continue; }
        let pick = |key: &str, field: &str, set: &[&str]| -> Result<Cell, String> {
            match r.get(key) { None => Ok(None), Some(v) => match canon(v, set) { Some(c) => Ok(Some(c)), None => Err(format!("unrecognised {field} '{v}'")) } }
        };
        let gender = match pick("gender", "gender", &["female", "male", "other"]) { Ok(v) => v, Err(e) => { rep.reject(r, e); continue } };
        let tier = match pick("loyaltytier", "loyalty_tier", &["bronze", "silver", "gold"]) { Ok(v) => v, Err(e) => { rep.reject(r, e); continue } };
        let region = match pick("region", "region", REGIONS) { Ok(v) => v, Err(e) => { rep.reject(r, e); continue } };
        for (k, f, c) in [("gender", "gender", &gender), ("loyaltytier", "loyalty_tier", &tier), ("region", "region", &region)] { if let (Some(raw), Some(cv)) = (r.get(k), c) { rep.fix(f, raw, cv); } }
        let email = r.get("email").map(|e| e.to_lowercase());
        if let Some(e) = &email { if !e.contains('@') { bad_email += 1; } if !emails.insert(e.clone()) { dup_emails += 1; } }
        data.push(vec![Some(id.into()), Some("consumer".into()), email, n(signup), gender, region, tier, Some("customer_behaviour".into())]);
    }
    let (i, u) = bulk(tx, "crm_customers", &[("external_id", "text"), ("kind", "text"), ("email", "text"), ("signup_date", "date"), ("gender", "text"), ("region", "text"), ("loyalty_tier", "text"), ("source_dataset", "text")], &data, "external_id").await?;
    rep.inserted += i; rep.updated += u;
    if dup_emails > 0 { rep.warnings.push(format!("{dup_emails} duplicate e-mail address(es) in file")); }
    if bad_email > 0 { rep.warnings.push(format!("{bad_email} e-mail address(es) without '@'")); }
    let blanks = recs.iter().filter(|r| r.get("email").is_none()).count();
    if blanks > 0 { rep.warnings.push(format!("{blanks} customer(s) without an e-mail address")); }
    Ok(())
}

async fn sales_orders(tx: &impl ConnectionTrait, recs: &[Rec], rep: &mut Report) -> AppResult<()> {
    require(recs, &["orderid", "customerid", "productid", "quantity", "unitprice", "orderdate", "deliverystatus", "paymentmethod", "region"])?;
    let cust = id_map(tx, "SELECT external_id AS k, id::text AS id FROM crm_customers WHERE kind='consumer' AND external_id IS NOT NULL").await?;
    let prods = id_map(tx, "SELECT product_code AS k, id::text AS id FROM crm_products WHERE source_dataset='customer_behaviour'").await?;
    let base: HashMap<String, f64> = rows(tx, "SELECT product_code, list_price::float8 AS p FROM crm_products WHERE source_dataset='customer_behaviour'", vec![]).await?
        .iter().map(|r| (r["product_code"].as_str().unwrap().to_string(), r["p"].as_f64().unwrap_or(0.0))).collect();
    if cust.is_empty() || prods.is_empty() { return Err(AppError::Unprocessable("import product-info and customers before sales-orders".into())); }
    let (mut data, mut seen, mut dates, mut anomalies) = (vec![], HashMap::<String, usize>::new(), HashSet::new(), 0);
    for r in recs {
        let Some(oid) = r.get("orderid") else { rep.reject(r, "order_id is empty"); continue };
        if seen.contains_key(oid) { rep.reject(r, "duplicate order_id (first occurrence kept)"); continue; }
        let Some(c) = r.get("customerid").and_then(|c| cust.get(c)) else { rep.reject(r, "missing or unknown customer_id"); continue };
        let Some(pcode) = r.get("productid") else { rep.reject(r, "missing product_id"); continue };
        let Some(p) = prods.get(pcode) else { rep.reject(r, format!("unknown product_id '{pcode}'")); continue };
        let qty = match r.get("quantity") { None => { rep.reject(r, "missing quantity"); continue }, Some(q) => match q.parse::<i32>().ok().or_else(|| word_number(q)) { Some(v) if v > 0 => { if q.parse::<i32>().is_err() { rep.fix("quantity", q, &v.to_string()); } v }, _ => { rep.reject(r, format!("invalid quantity '{q}'")); continue } } };
        let list = base.get(pcode).copied().unwrap_or(0.0);
        let price = match r.get("unitprice").and_then(money) { Some(v) if v >= 0.0 => v, Some(_) => { rep.reject(r, "negative unit_price"); continue }, None => { rep.note("unit_price", "imputed from catalog base_price"); list } };
        if list > 0.0 && (price / list > 2.0 || price / list < 0.5) { anomalies += 1; }
        let date = r.get("orderdate").and_then(dmy);
        if r.get("orderdate").is_some() && date.is_none() { rep.reject(r, "invalid order_date (expected dd-mm-yy)"); continue; }
        if let Some(d) = date { dates.insert(d); }
        let pick = |key: &str, field: &str, set: &[&str]| -> Result<Cell, String> {
            match r.get(key) { None => Ok(None), Some(v) => match canon(v, set) { Some(c) => { Ok(Some(c)) } None => Err(format!("unrecognised {field} '{v}'")) } }
        };
        let delivery = match pick("deliverystatus", "delivery_status", &["delivered", "delayed", "cancelled"]) { Ok(v) => v, Err(e) => { rep.reject(r, e); continue } };
        let pay = match pick("paymentmethod", "payment_method", &["credit_card", "paypal", "bank_transfer"]) { Ok(v) => v, Err(e) => { rep.reject(r, e); continue } };
        let region = match pick("region", "region", REGIONS) { Ok(v) => v, Err(e) => { rep.reject(r, e); continue } };
        for (k, f, v) in [("deliverystatus", "delivery_status", &delivery), ("paymentmethod", "payment_method", &pay), ("region", "region", &region)] { if let (Some(raw), Some(cv)) = (r.get(k), v) { rep.fix(f, raw, cv); } }
        let disc = match r.get("discountapplied") { None => None, Some(d) => match d.parse::<f64>().ok().filter(|x| (0.0..=1.0).contains(x)) { Some(v) => Some(v), None => { rep.reject(r, "invalid discount_applied"); continue } } };
        seen.insert(oid.to_string(), 1);
        data.push(vec![Some(oid.into()), Some(c.clone()), Some(p.clone()), Some(qty.to_string()), Some(m2(price)), n(disc), n(date), delivery, pay, region, Some("customer_behaviour".into())]);
    }
    let (i, u) = bulk(tx, "crm_orders", &[("order_code", "text"), ("customer_id", "uuid"), ("product_id", "uuid"), ("quantity", "int"), ("unit_price", "numeric"), ("discount_rate", "numeric"),
        ("order_date", "date"), ("delivery_status", "text"), ("payment_method", "text"), ("region", "text"), ("source_dataset", "text")], &data, "order_code").await?;
    rep.inserted += i; rep.updated += u;
    if dates.len() == 1 { rep.warnings.push(format!("every order carries the same order_date ({}): recency and trend analysis are not meaningful for this data", dates.iter().next().unwrap())); }
    if anomalies > 0 { rep.warnings.push(format!("{anomalies} order(s) priced more than 2x or less than 0.5x the catalog base price (imported, flagged in the data-quality report)")); }
    let missing_disc = recs.iter().filter(|r| r.get("discountapplied").is_none()).count();
    if missing_disc > 0 { rep.warnings.push(format!("{missing_disc} order(s) have no discount value; treated as no discount in net_amount but excluded from discount analysis")); }
    Ok(())
}

// --------------------------------------------------------- donor dataset

async fn donors(tx: &impl ConnectionTrait, recs: &[Rec], rep: &mut Report) -> AppResult<()> {
    require(recs, &["donorid", "lastdonationdate", "totalgifts", "totalamountdonated", "engagementscore"])?;
    let (mut data, mut seen, mut emails, mut dup_emails) = (vec![], HashSet::new(), HashSet::new(), 0);
    for r in recs {
        let Some(id) = r.get("donorid") else { rep.reject(r, "DonorID is empty"); continue };
        if !seen.insert(id.to_string()) { rep.reject(r, "duplicate DonorID in file"); continue; }
        let Some(last) = r.get("lastdonationdate").and_then(mdy) else { rep.reject(r, "invalid LastDonationDate (expected m/d/yyyy)"); continue };
        let Some(gifts) = r.get("totalgifts").and_then(|g| g.parse::<i32>().ok()).filter(|g| *g >= 0) else { rep.reject(r, "invalid TotalGifts"); continue };
        let Some(amt) = r.get("totalamountdonated").and_then(money).filter(|a| *a >= 0.0) else { rep.reject(r, "invalid TotalAmountDonated"); continue };
        let Some(score) = r.get("engagementscore").and_then(|g| g.parse::<i32>().ok()).filter(|g| (0..=100).contains(g)) else { rep.reject(r, "EngagementScore must be 0-100"); continue };
        let event = match r.get("eventparticipation").map(|e| norm(e)) { Some(e) if e == "yes" => Some("true"), Some(e) if e == "no" => Some("false"), None => None, Some(_) => { rep.reject(r, "EventParticipation must be Yes/No"); continue } };
        let zip = r.get("zipcode").map(|z| { let t = z.trim(); if t.len() < 5 && t.chars().all(|c| c.is_ascii_digit()) { rep.note("postal_code", "zero-padded to 5 digits"); format!("{t:0>5}") } else { t.to_string() } });
        let email = r.get("email").map(|e| e.to_lowercase());
        if let Some(e) = &email { if !emails.insert(e.clone()) { dup_emails += 1; } }
        data.push(vec![Some(id.into()), Some("donor".into()), s(r.get("firstname")), s(r.get("lastname")), email, s(r.get("phone")), s(r.get("city")), r.get("state").map(|x| x.to_uppercase()), zip,
            Some(last.to_string()), Some(gifts.to_string()), Some(m2(amt)), Some(score.to_string()), event.map(String::from), Some("donor_simulation".into())]);
    }
    let (i, u) = bulk(tx, "crm_customers", &[("external_id", "text"), ("kind", "text"), ("first_name", "text"), ("last_name", "text"), ("email", "text"), ("phone", "text"), ("city", "text"), ("state", "text"),
        ("postal_code", "text"), ("last_activity_date", "date"), ("gift_count", "int"), ("total_donated", "numeric"), ("engagement_score", "int"), ("event_participation", "boolean"), ("source_dataset", "text")], &data, "external_id").await?;
    rep.inserted += i; rep.updated += u;
    if dup_emails > 0 { rep.warnings.push(format!("{dup_emails} donor(s) share an e-mail address with another donor in the file: possible duplicates (see /api/crm/data-quality/duplicates)")); }
    Ok(())
}
