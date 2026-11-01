//! CRM: lead conversion, opportunity pipeline, marketing automation (lead scoring), service & customer analytics.
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/crm/leads/{id}/convert", post(convert_lead))
        .route("/api/crm/opportunities/{id}/stage", post(set_stage))
        .route("/api/crm/marketing/score-leads", post(score_leads))
        .route("/api/crm/analytics/pipeline", get(pipeline))
        .route("/api/crm/analytics/lead-funnel", get(lead_funnel))
        .route("/api/crm/analytics/campaigns", get(campaign_performance))
        .route("/api/crm/analytics/service", get(service_metrics))
        .route("/api/crm/analytics/customer-360/{account_id}", get(customer_360))
}

/// Converts a lead into account + contact (+ optional opportunity) in one transaction.
async fn convert_lead(State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let tx = st.db.begin().await?;
    let lead = row(&tx, "SELECT * FROM crm_leads WHERE id = $1 FOR UPDATE", vec![id.into()]).await?;
    if lead.is_null() { return Err(AppError::NotFound(None)); }
    if lead["status"] == "converted" { return Err(AppError::Conflict("lead already converted".into())); }
    let last = lead["last_name"].as_str().unwrap_or_default().to_string();
    let first = lead["first_name"].as_str().map(String::from);
    let company = lead["company"].as_str().map(String::from)
        .unwrap_or_else(|| format!("{} {}", first.clone().unwrap_or_default(), last).trim().to_string());
    let owner: Option<Uuid> = lead["owner_id"].as_str().and_then(|s| s.parse().ok());
    let campaign: Option<Uuid> = lead["campaign_id"].as_str().and_then(|s| s.parse().ok());
    let s = |k: &str| lead[k].as_str().map(String::from);

    let acct = row(&tx, "INSERT INTO crm_accounts (name, account_type, owner_id, phone, email) VALUES ($1,'customer',$2,$3,$4) RETURNING id::text AS id",
        vec![company.clone().into(), owner.into(), s("phone").into(), s("email").into()]).await?;
    let acct_id: Uuid = acct["id"].as_str().unwrap().parse().unwrap();
    let contact = row(&tx, "INSERT INTO crm_contacts (account_id, first_name, last_name, email, phone, job_title, is_primary, owner_id) VALUES ($1,$2,$3,$4,$5,$6,true,$7) RETURNING id::text AS id",
        vec![acct_id.into(), first.unwrap_or_default().into(), last.into(), s("email").into(), s("phone").into(), s("job_title").into(), owner.into()]).await?;
    let contact_id: Uuid = contact["id"].as_str().unwrap().parse().unwrap();

    let mut opp_id: Option<Uuid> = None;
    if b["create_opportunity"].as_bool().unwrap_or(true) {
        let name = b["opportunity_name"].as_str().map(String::from).unwrap_or_else(|| format!("{company} - New business"));
        let amount = b["amount"].as_f64().unwrap_or(0.0);
        let o = row(&tx, "INSERT INTO crm_opportunities (account_id, contact_id, name, amount, owner_id, campaign_id) VALUES ($1,$2,$3,$4::float8::numeric,$5,$6) RETURNING id::text AS id",
            vec![acct_id.into(), contact_id.into(), name.into(), amount.into(), owner.into(), campaign.into()]).await?;
        opp_id = Some(o["id"].as_str().unwrap().parse().unwrap());
    }
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE crm_leads SET status='converted', converted_at=now(), converted_account_id=$2, converted_contact_id=$3, converted_opportunity_id=$4 WHERE id=$1",
        [id.into(), acct_id.into(), contact_id.into(), opp_id.into()])).await?;
    tx.commit().await?;
    Ok(Json(json!({ "lead_id": id, "account_id": acct_id, "contact_id": contact_id, "opportunity_id": opp_id })))
}

async fn set_stage(State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let stage = b["stage"].as_str().ok_or_else(|| AppError::BadRequest("stage required".into()))?;
    if stage == "closed_lost" && b["lost_reason"].as_str().is_none() {
        return Err(AppError::Unprocessable("lost_reason is required when closing as lost".into()));
    }
    let r = row(&st.db, "UPDATE crm_opportunities SET stage=$2, lost_reason=COALESCE($3, lost_reason) WHERE id=$1 RETURNING id::text AS id, stage, probability, closed_at",
        vec![id.into(), stage.into(), b["lost_reason"].as_str().map(String::from).into()]).await?;
    if r.is_null() { return Err(AppError::NotFound(None)); }
    Ok(Json(r))
}

#[derive(Debug)]
struct Rule { field: String, op: String, value: Option<String>, points: i64 }

fn rule_matches(lead: &Value, r: &Rule) -> bool {
    let v = &lead[&r.field];
    let text = match v { Value::String(s) => Some(s.clone()), Value::Null => None, o => Some(o.to_string()) };
    match r.op.as_str() {
        "not_null" => text.as_deref().map(|t| !t.is_empty()).unwrap_or(false),
        "is_null" => text.as_deref().map(|t| t.is_empty()).unwrap_or(true),
        "eq" => text.as_deref() == r.value.as_deref(),
        "neq" => text.as_deref() != r.value.as_deref(),
        "contains" => match (&text, &r.value) { (Some(t), Some(x)) => t.to_lowercase().contains(&x.to_lowercase()), _ => false },
        "gte" | "lte" => match (text.and_then(|t| t.parse::<f64>().ok()), r.value.as_ref().and_then(|x| x.parse::<f64>().ok())) {
            (Some(a), Some(b)) => if r.op == "gte" { a >= b } else { a <= b },
            _ => false,
        },
        _ => false,
    }
}

/// Marketing automation: applies the active scoring rules to every unconverted lead.
async fn score_leads(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let rules: Vec<Rule> = rows(&st.db, "SELECT field, operator, value, points::bigint AS points FROM crm_scoring_rules WHERE is_active", vec![]).await?
        .iter().map(|r| Rule { field: r["field"].as_str().unwrap_or_default().into(), op: r["operator"].as_str().unwrap_or_default().into(),
            value: r["value"].as_str().map(String::from), points: r["points"].as_i64().unwrap_or(0) }).collect();
    let leads = rows(&st.db, "SELECT * FROM crm_leads WHERE status <> 'converted'", vec![]).await?;
    let tx = st.db.begin().await?;
    let mut updated = 0;
    for l in &leads {
        let score: i64 = rules.iter().filter(|r| rule_matches(l, r)).map(|r| r.points).sum();
        let id: Uuid = l["id"].as_str().unwrap().parse().unwrap();
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE crm_leads SET score=$2 WHERE id=$1 AND score IS DISTINCT FROM $2",
            [id.into(), (score as i32).into()])).await?;
        updated += 1;
    }
    tx.commit().await?;
    Ok(Json(json!({ "rules_applied": rules.len(), "leads_scored": updated })))
}

async fn pipeline(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let stages = rows(&st.db, "SELECT stage, count(*) AS deals, COALESCE(sum(amount),0)::float8 AS amount, COALESCE(sum(amount*probability/100.0),0)::float8 AS weighted_amount
        FROM crm_opportunities GROUP BY stage ORDER BY array_position(ARRAY['prospecting','qualification','proposal','negotiation','closed_won','closed_lost'], stage)", vec![]).await?;
    let t = row(&st.db, "SELECT count(*) FILTER (WHERE stage NOT IN ('closed_won','closed_lost')) AS open_deals,
        COALESCE(sum(amount) FILTER (WHERE stage NOT IN ('closed_won','closed_lost')),0)::float8 AS open_amount,
        COALESCE(sum(amount*probability/100.0) FILTER (WHERE stage NOT IN ('closed_won','closed_lost')),0)::float8 AS weighted_forecast,
        COALESCE(sum(amount) FILTER (WHERE stage='closed_won'),0)::float8 AS won_amount,
        round(100.0 * count(*) FILTER (WHERE stage='closed_won') / NULLIF(count(*) FILTER (WHERE stage IN ('closed_won','closed_lost')),0),1)::float8 AS win_rate_pct,
        COALESCE(avg(amount) FILTER (WHERE stage='closed_won'),0)::float8 AS avg_deal_size FROM crm_opportunities", vec![]).await?;
    Ok(Json(json!({ "by_stage": stages, "totals": t })))
}

async fn lead_funnel(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let by_status = rows(&st.db, "SELECT status, count(*) AS leads FROM crm_leads GROUP BY status ORDER BY 2 DESC", vec![]).await?;
    let by_source = rows(&st.db, "SELECT source, count(*) AS leads, count(*) FILTER (WHERE status='converted') AS converted,
        round(100.0*count(*) FILTER (WHERE status='converted')/count(*),1)::float8 AS conversion_pct, round(avg(score),1)::float8 AS avg_score FROM crm_leads GROUP BY source ORDER BY 2 DESC", vec![]).await?;
    let t = row(&st.db, "SELECT count(*) AS total, count(*) FILTER (WHERE status='converted') AS converted,
        round(100.0*count(*) FILTER (WHERE status='converted')/NULLIF(count(*),0),1)::float8 AS conversion_pct,
        round(avg(EXTRACT(epoch FROM (converted_at - created_at))/86400) FILTER (WHERE status='converted')::numeric,1)::float8 AS avg_days_to_convert FROM crm_leads", vec![]).await?;
    Ok(Json(json!({ "by_status": by_status, "by_source": by_source, "totals": t })))
}

async fn campaign_performance(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let r = rows(&st.db, "SELECT c.id::text AS id, c.name, c.channel, c.status, c.budget::float8 AS budget, c.actual_cost::float8 AS actual_cost,
        (SELECT count(*) FROM crm_leads l WHERE l.campaign_id=c.id) AS leads,
        (SELECT count(*) FROM crm_leads l WHERE l.campaign_id=c.id AND l.status='converted') AS converted_leads,
        COALESCE((SELECT sum(o.amount) FROM crm_opportunities o WHERE o.campaign_id=c.id AND o.stage='closed_won'),0)::float8 AS won_revenue,
        CASE WHEN c.actual_cost > 0 THEN round((COALESCE((SELECT sum(o.amount) FROM crm_opportunities o WHERE o.campaign_id=c.id AND o.stage='closed_won'),0) - c.actual_cost) / c.actual_cost * 100, 1)::float8 END AS roi_pct,
        CASE WHEN c.actual_cost > 0 THEN round(c.actual_cost / NULLIF((SELECT count(*) FROM crm_leads l WHERE l.campaign_id=c.id),0), 2)::float8 END AS cost_per_lead
        FROM crm_campaigns c ORDER BY c.created_at DESC", vec![]).await?;
    Ok(Json(json!({ "data": r })))
}

async fn service_metrics(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let t = row(&st.db, "SELECT count(*) AS total, count(*) FILTER (WHERE status NOT IN ('resolved','closed')) AS open_cases,
        count(*) FILTER (WHERE status NOT IN ('resolved','closed') AND sla_due_at < now()) AS open_sla_breached,
        count(*) FILTER (WHERE resolved_at IS NOT NULL AND resolved_at > sla_due_at) AS resolved_late,
        round(100.0*count(*) FILTER (WHERE resolved_at IS NOT NULL AND resolved_at <= sla_due_at)/NULLIF(count(*) FILTER (WHERE resolved_at IS NOT NULL),0),1)::float8 AS sla_compliance_pct,
        round((avg(EXTRACT(epoch FROM (resolved_at-created_at))/3600) FILTER (WHERE resolved_at IS NOT NULL))::numeric,2)::float8 AS avg_resolution_hours,
        round((avg(EXTRACT(epoch FROM (first_response_at-created_at))/3600) FILTER (WHERE first_response_at IS NOT NULL))::numeric,2)::float8 AS avg_first_response_hours,
        round(avg(satisfaction_score),2)::float8 AS avg_csat FROM crm_cases", vec![]).await?;
    let by_status = rows(&st.db, "SELECT status, count(*) AS cases FROM crm_cases GROUP BY status", vec![]).await?;
    let by_priority = rows(&st.db, "SELECT priority, count(*) AS cases FROM crm_cases WHERE status NOT IN ('resolved','closed') GROUP BY priority", vec![]).await?;
    Ok(Json(json!({ "totals": t, "by_status": by_status, "open_by_priority": by_priority })))
}

async fn customer_360(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let account = row(&st.db, "SELECT to_jsonb(a) AS a FROM crm_accounts a WHERE id=$1", vec![id.into()]).await?;
    if account.is_null() { return Err(AppError::NotFound(None)); }
    let v = |sql: &'static str| { let db = st.db.clone(); async move { row(&db, sql, vec![id.into()]).await.map(|r| r["x"].clone()) } };
    let contacts = v("SELECT COALESCE(jsonb_agg(to_jsonb(c)),'[]'::jsonb) AS x FROM crm_contacts c WHERE account_id=$1").await?;
    let opps = v("SELECT COALESCE(jsonb_agg(jsonb_build_object('id',o.id,'name',o.name,'stage',o.stage,'amount',o.amount,'probability',o.probability) ORDER BY o.created_at DESC),'[]'::jsonb) AS x FROM crm_opportunities o WHERE account_id=$1").await?;
    let cases = v("SELECT COALESCE(jsonb_agg(jsonb_build_object('id',c.id,'case_number',c.case_number,'subject',c.subject,'status',c.status,'priority',c.priority) ORDER BY c.created_at DESC),'[]'::jsonb) AS x FROM crm_cases c WHERE account_id=$1").await?;
    let acts = v("SELECT COALESCE(jsonb_agg(x ORDER BY (x->>'created_at') DESC),'[]'::jsonb) AS x FROM (SELECT to_jsonb(a) AS x FROM crm_activities a WHERE account_id=$1 ORDER BY created_at DESC LIMIT 20) q").await?;
    let m = row(&st.db, "SELECT COALESCE(sum(amount) FILTER (WHERE stage='closed_won'),0)::float8 AS lifetime_value,
        COALESCE(sum(amount) FILTER (WHERE stage NOT IN ('closed_won','closed_lost')),0)::float8 AS open_pipeline,
        (SELECT count(*) FROM crm_cases WHERE account_id=$1 AND status NOT IN ('resolved','closed')) AS open_cases
        FROM crm_opportunities WHERE account_id=$1", vec![id.into()]).await?;
    Ok(Json(json!({ "account": account["a"], "metrics": m, "contacts": contacts, "opportunities": opps, "cases": cases, "recent_activities": acts })))
}
