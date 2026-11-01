//! Customer & donor analytics (RFM segmentation, lapse risk, giving insights), order analytics,
//! product affinity and data-quality tooling.
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use sea_orm::DatabaseConnection;
use serde_json::{json, Value as J};
use std::collections::HashMap;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/crm/customers/overview", get(overview))
        .route("/api/crm/customers/rfm", get(rfm))
        .route("/api/crm/donors/lapse", get(lapse))
        .route("/api/crm/donors/insights", get(donor_insights))
        .route("/api/crm/orders/analytics", get(order_analytics))
        .route("/api/crm/orders/affinity", get(affinity))
        .route("/api/crm/data-quality/duplicates", get(duplicates))
        .route("/api/crm/data-quality/report", get(quality_report))
}

fn kind(p: &HashMap<String, String>) -> AppResult<&'static str> {
    match p.get("kind").map(String::as_str).unwrap_or("donor") { "donor" => Ok("donor"), "consumer" => Ok("consumer"), _ => Err(AppError::BadRequest("kind must be donor or consumer".into())) }
}

/// as_of defaults to the latest donation date in the data (data time), `as_of=today` uses the current date.
async fn as_of(db: &DatabaseConnection, p: &HashMap<String, String>) -> AppResult<String> {
    match p.get("as_of").map(String::as_str) {
        Some("today") => Ok(row(db, "SELECT current_date::text AS d", vec![]).await?["d"].as_str().unwrap().into()),
        Some(d) => { d.parse::<chrono::NaiveDate>().map_err(|_| AppError::BadRequest("as_of must be YYYY-MM-DD or 'today'".into()))?; Ok(d.into()) }
        None => Ok(row(db, "SELECT COALESCE(max(last_activity_date), current_date)::text AS d FROM crm_customers WHERE kind='donor'", vec![]).await?["d"].as_str().unwrap().into()),
    }
}

async fn overview(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    if kind(&p)? == "donor" {
        let t = row(&st.db, "SELECT count(*) AS donors, COALESCE(sum(total_donated),0)::float8 AS total_donated, round(sum(total_donated)/NULLIF(sum(gift_count),0),2)::float8 AS avg_gift,
            percentile_cont(0.5) WITHIN GROUP (ORDER BY total_donated)::float8 AS median_donated, percentile_cont(0.9) WITHIN GROUP (ORDER BY total_donated)::float8 AS p90_donated,
            round(100.0*avg(CASE WHEN event_participation THEN 1 ELSE 0 END),1)::float8 AS event_participation_pct, round(avg(engagement_score),1)::float8 AS avg_engagement,
            min(last_activity_date)::text AS first_last_gift, max(last_activity_date)::text AS latest_gift FROM crm_customers WHERE kind='donor'", vec![]).await?;
        let conc = row(&st.db, "WITH r AS (SELECT total_donated, ntile(100) OVER (ORDER BY total_donated DESC) AS pct FROM crm_customers WHERE kind='donor')
            SELECT round(100*sum(total_donated) FILTER (WHERE pct=1)/NULLIF(sum(total_donated),0),1)::float8 AS top_1pct_share, round(100*sum(total_donated) FILTER (WHERE pct<=10)/NULLIF(sum(total_donated),0),1)::float8 AS top_10pct_share FROM r", vec![]).await?;
        let states = rows(&st.db, "SELECT COALESCE(state,'(none)') AS state, count(*) AS donors, sum(total_donated)::float8 AS total_donated, round(avg(total_donated),2)::float8 AS avg_donated FROM crm_customers WHERE kind='donor' GROUP BY 1 ORDER BY 3 DESC LIMIT 10", vec![]).await?;
        Ok(Json(json!({ "kind": "donor", "totals": t, "concentration": conc, "top_states": states })))
    } else {
        let t = row(&st.db, "WITH o AS (SELECT customer_id, count(*) AS orders, sum(net_amount) AS rev FROM crm_orders GROUP BY 1)
            SELECT (SELECT count(*) FROM crm_customers WHERE kind='consumer') AS customers, count(o.customer_id) AS customers_with_orders, COALESCE(sum(o.orders),0) AS orders, COALESCE(sum(o.rev),0)::float8 AS net_revenue,
            round(sum(o.rev)/NULLIF(sum(o.orders),0),2)::float8 AS avg_order_value, round(avg(o.orders),2)::float8 AS orders_per_buying_customer FROM o", vec![]).await?;
        let tiers = rows(&st.db, "WITH o AS (SELECT customer_id, count(*) AS orders, sum(net_amount) AS rev FROM crm_orders GROUP BY 1)
            SELECT COALESCE(c.loyalty_tier,'(none)') AS tier, count(*) AS customers, COALESCE(sum(o.orders),0) AS orders, COALESCE(sum(o.rev),0)::float8 AS net_revenue,
            round(COALESCE(sum(o.rev),0)/NULLIF(count(*),0),2)::float8 AS revenue_per_customer, round(sum(o.rev)/NULLIF(sum(o.orders),0),2)::float8 AS avg_order_value
            FROM crm_customers c LEFT JOIN o ON o.customer_id=c.id WHERE c.kind='consumer' GROUP BY 1 ORDER BY 4 DESC", vec![]).await?;
        let regions = rows(&st.db, "SELECT COALESCE(region,'(none)') AS region, count(*) AS customers FROM crm_customers WHERE kind='consumer' GROUP BY 1 ORDER BY 2 DESC", vec![]).await?;
        Ok(Json(json!({ "kind": "consumer", "totals": t, "by_loyalty_tier": tiers, "by_region": regions })))
    }
}

const SEGMENT_CASE: &str = "CASE WHEN r>=4 AND f>=4 AND m>=4 THEN 'champions' WHEN r<=2 AND m>=4 AND f>=3 THEN 'cant_lose_them' WHEN r<=2 AND f>=3 THEN 'at_risk'
    WHEN f>=4 AND r>=3 THEN 'loyal' WHEN r>=4 AND f<=2 THEN 'new_or_promising' WHEN r<=2 THEN 'hibernating' ELSE 'potential' END";
fn q5(order: &str) -> String { format!("LEAST(5, 1 + floor(5*percent_rank() OVER (ORDER BY {order}))::int)") }

async fn rfm(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let limit: i64 = p.get("limit").and_then(|v| v.parse().ok()).unwrap_or(10).clamp(0, 200);
    let seg = p.get("segment").cloned();
    if kind(&p)? == "donor" {
        let ao = as_of(&st.db, &p).await?;
        let base = format!("WITH base AS (SELECT id, external_id, first_name, last_name, email, last_activity_date, gift_count, total_donated, engagement_score, ($1::date - last_activity_date) AS recency_days
            FROM crm_customers WHERE kind='donor' AND last_activity_date IS NOT NULL),
            scored AS (SELECT *, {} AS r, {} AS f, {} AS m FROM base), seg AS (SELECT *, {SEGMENT_CASE} AS segment FROM scored)", q5("recency_days DESC"), q5("gift_count"), q5("total_donated"));
        let summary = rows(&st.db, &format!("{base} SELECT segment, count(*) AS donors, round(100.0*count(*)/sum(count(*)) OVER (),1)::float8 AS donor_pct, sum(total_donated)::float8 AS total_donated,
            round(100.0*sum(total_donated)/sum(sum(total_donated)) OVER (),1)::float8 AS value_pct, round(avg(recency_days))::int AS avg_recency_days, round(avg(gift_count),1)::float8 AS avg_gifts,
            round(avg(engagement_score),1)::float8 AS avg_engagement FROM seg GROUP BY 1 ORDER BY 4 DESC"), vec![ao.clone().into()]).await?;
        let sample = if limit > 0 { rows(&st.db, &format!("{base} SELECT id::text AS id, external_id, trim(coalesce(first_name,'')||' '||coalesce(last_name,'')) AS name, email, last_activity_date::text AS last_gift, recency_days,
            gift_count, total_donated::float8 AS total_donated, engagement_score, r, f, m, segment FROM seg WHERE ($2::text IS NULL OR segment=$2) ORDER BY total_donated DESC LIMIT {limit}"), vec![ao.clone().into(), seg.into()]).await? } else { vec![] };
        Ok(Json(json!({ "kind": "donor", "as_of": ao, "scoring": "quintiles 1-5 by percent rank (ties share a score): R = days since last gift (recent = 5), F = gift count, M = total donated", "segments": summary, "members": sample })))
    } else {
        let dates = row(&st.db, "SELECT count(DISTINCT order_date) AS d FROM crm_orders", vec![]).await?;
        let recency = dates["d"].as_i64().unwrap_or(0) > 1;
        let base = format!("WITH agg AS (SELECT c.id, c.external_id, c.email, c.loyalty_tier, count(o.id) AS orders, COALESCE(sum(o.net_amount),0) AS revenue FROM crm_customers c LEFT JOIN crm_orders o ON o.customer_id=c.id WHERE c.kind='consumer' GROUP BY c.id),
            scored AS (SELECT *, {} AS f, {} AS m FROM agg WHERE orders>0), seg AS (SELECT *, CASE WHEN m>=4 AND f>=4 THEN 'champions' WHEN m>=4 THEN 'big_spenders' WHEN f>=4 THEN 'frequent_buyers' WHEN m<=2 AND f<=2 THEN 'low_engagement' ELSE 'regular' END AS segment FROM scored)", q5("orders"), q5("revenue"));
        let summary = rows(&st.db, &format!("{base} SELECT segment, count(*) AS customers, sum(revenue)::float8 AS revenue, round(100.0*sum(revenue)/sum(sum(revenue)) OVER (),1)::float8 AS value_pct, round(avg(orders),1)::float8 AS avg_orders FROM seg GROUP BY 1 ORDER BY 3 DESC"), vec![]).await?;
        let sample = if limit > 0 { rows(&st.db, &format!("{base} SELECT id::text AS id, external_id, email, loyalty_tier, orders, revenue::float8 AS revenue, f, m, segment FROM seg WHERE ($1::text IS NULL OR segment=$1) ORDER BY revenue DESC LIMIT {limit}"), vec![seg.into()]).await? } else { vec![] };
        Ok(Json(json!({ "kind": "consumer", "recency_available": recency, "scoring": "F = order count, M = net revenue (quintiles by percent rank)",
            "note": if recency { J::Null } else { json!("every order has the same order_date, so recency cannot be scored; segments use frequency and monetary value only") }, "segments": summary, "members": sample })))
    }
}

async fn lapse(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let ao = as_of(&st.db, &p).await?;
    let limit: i64 = p.get("limit").and_then(|v| v.parse().ok()).unwrap_or(25).clamp(1, 500);
    let base = "WITH d AS (SELECT *, ($1::date - last_activity_date)/30.4375 AS months FROM crm_customers WHERE kind='donor' AND last_activity_date IS NOT NULL),
        s AS (SELECT *, CASE WHEN months < 12 THEN 'active' WHEN months < 24 THEN 'lapsing' ELSE 'lapsed' END AS status FROM d)";
    let by = rows(&st.db, &format!("{base} SELECT status, count(*) AS donors, round(100.0*count(*)/sum(count(*)) OVER (),1)::float8 AS donor_pct, sum(total_donated)::float8 AS lifetime_value, round(avg(gift_count),1)::float8 AS avg_gifts,
        round(avg(engagement_score),1)::float8 AS avg_engagement, round(100.0*avg(CASE WHEN event_participation THEN 1 ELSE 0 END),1)::float8 AS event_pct FROM s GROUP BY 1 ORDER BY array_position(ARRAY['active','lapsing','lapsed'], status)"), vec![ao.clone().into()]).await?;
    let cand = rows(&st.db, &format!("{base} SELECT id::text AS id, external_id, trim(coalesce(first_name,'')||' '||coalesce(last_name,'')) AS name, email, phone, status, round(months::numeric,1)::float8 AS months_since_gift,
        gift_count, total_donated::float8 AS lifetime_value, engagement_score, round((total_donated*engagement_score/100.0)::numeric,2)::float8 AS priority_index
        FROM s WHERE status IN ('lapsing','lapsed') ORDER BY (total_donated*engagement_score) DESC LIMIT {limit}"), vec![ao.clone().into()]).await?;
    Ok(Json(json!({ "as_of": ao, "thresholds_months": { "active": "< 12", "lapsing": "12-24", "lapsed": ">= 24" }, "by_status": by,
        "reactivation_candidates": { "priority_index": "lifetime value x engagement score / 100 (a ranking heuristic, not a probability)", "data": cand } })))
}

fn welch(m1: f64, s1: f64, n1: f64, m2: f64, s2: f64, n2: f64) -> f64 { (m1 - m2) / (s1 * s1 / n1 + s2 * s2 / n2).sqrt() }

async fn donor_insights(State(st): State<AppState>) -> AppResult<Json<J>> {
    let ev = rows(&st.db, "SELECT event_participation AS attended, count(*) AS donors, avg(total_donated)::float8 AS avg_donated, stddev_samp(total_donated)::float8 AS sd_donated, avg(gift_count)::float8 AS avg_gifts, avg(engagement_score)::float8 AS avg_engagement
        FROM crm_customers WHERE kind='donor' AND event_participation IS NOT NULL GROUP BY 1 ORDER BY 1 DESC", vec![]).await?;
    let corr = row(&st.db, "SELECT corr(engagement_score::float8, total_donated::float8) AS engagement_vs_total, corr(gift_count::float8, total_donated::float8) AS gifts_vs_total,
        corr(engagement_score::float8, gift_count::float8) AS engagement_vs_gifts FROM crm_customers WHERE kind='donor'", vec![]).await?;
    let bands = rows(&st.db, "SELECT CASE WHEN engagement_score<=20 THEN '01-20' WHEN engagement_score<=40 THEN '21-40' WHEN engagement_score<=60 THEN '41-60' WHEN engagement_score<=80 THEN '61-80' ELSE '81-100' END AS engagement_band,
        count(*) AS donors, round(avg(total_donated),2)::float8 AS avg_donated, round(avg(gift_count),1)::float8 AS avg_gifts FROM crm_customers WHERE kind='donor' GROUP BY 1 ORDER BY 1", vec![]).await?;
    let mut findings: Vec<String> = vec![];
    let f = |k: &str| corr[k].as_f64().unwrap_or(0.0);
    if f("engagement_vs_total").abs() < 0.1 { findings.push(format!("engagement score is essentially unrelated to amount donated (r = {:.3}); do not use it as a proxy for giving capacity", f("engagement_vs_total"))); }
    else { findings.push(format!("engagement score correlates with amount donated (r = {:.3})", f("engagement_vs_total"))); }
    if f("gifts_vs_total").abs() < 0.1 { findings.push(format!("number of gifts is unrelated to total donated (r = {:.3}); average gift size varies independently of frequency", f("gifts_vs_total"))); }
    let mut test = J::Null;
    if ev.len() == 2 {
        let g = |i: usize, k: &str| ev[i][k].as_f64().unwrap_or(0.0);
        let t = welch(g(0, "avg_donated"), g(0, "sd_donated"), g(0, "donors"), g(1, "avg_donated"), g(1, "sd_donated"), g(1, "donors"));
        test = json!({ "metric": "avg total donated, attended vs not", "welch_t": (t * 100.0).round() / 100.0, "significant_at_5pct": t.abs() > 1.96 });
        findings.push(format!("event attendees give {:+.0} vs non-attendees on average ({}significant, |t| = {:.2})", g(0, "avg_donated") - g(1, "avg_donated"), if t.abs() > 1.96 { "" } else { "not " }, t.abs()));
    }
    Ok(Json(json!({ "event_participation": ev, "event_test": test, "correlations": corr, "by_engagement_band": bands, "findings": findings })))
}

async fn order_analytics(State(st): State<AppState>) -> AppResult<Json<J>> {
    let t = row(&st.db, "SELECT count(*) AS orders, count(DISTINCT customer_id) AS customers, COALESCE(sum(quantity),0) AS units, COALESCE(sum(quantity*unit_price),0)::float8 AS gross_revenue, COALESCE(sum(net_amount),0)::float8 AS net_revenue,
        COALESCE(sum(quantity*unit_price - net_amount),0)::float8 AS discounts_given, round(sum(net_amount)/NULLIF(count(*),0),2)::float8 AS avg_order_value FROM crm_orders", vec![]).await?;
    let share = "round(100.0*sum(o.net_amount)/sum(sum(o.net_amount)) OVER (),1)::float8 AS revenue_pct";
    let cat = rows(&st.db, &format!("SELECT COALESCE(p.category,'(none)') AS category, count(*) AS orders, sum(o.net_amount)::float8 AS net_revenue, {share} FROM crm_orders o JOIN crm_products p ON p.id=o.product_id GROUP BY 1 ORDER BY 3 DESC"), vec![]).await?;
    let top = rows(&st.db, "SELECT p.product_code, p.name, p.category, count(*) AS orders, sum(o.quantity) AS units, sum(o.net_amount)::float8 AS net_revenue FROM crm_orders o JOIN crm_products p ON p.id=o.product_id GROUP BY 1,2,3 ORDER BY 6 DESC LIMIT 10", vec![]).await?;
    let region = rows(&st.db, &format!("SELECT COALESCE(o.region,'(none)') AS region, count(*) AS orders, sum(o.net_amount)::float8 AS net_revenue, {share} FROM crm_orders o GROUP BY 1 ORDER BY 3 DESC"), vec![]).await?;
    let pay = rows(&st.db, &format!("SELECT COALESCE(o.payment_method,'(none)') AS payment_method, count(*) AS orders, sum(o.net_amount)::float8 AS net_revenue, {share} FROM crm_orders o GROUP BY 1 ORDER BY 3 DESC"), vec![]).await?;
    let tier = rows(&st.db, "SELECT COALESCE(c.loyalty_tier,'(none)') AS loyalty_tier, count(*) AS orders, sum(o.net_amount)::float8 AS net_revenue, round(avg(o.net_amount),2)::float8 AS avg_order_value FROM crm_orders o JOIN crm_customers c ON c.id=o.customer_id GROUP BY 1 ORDER BY 3 DESC", vec![]).await?;
    let delivery = rows(&st.db, "SELECT COALESCE(delivery_status,'(unknown)') AS delivery_status, count(*) AS orders, round(100.0*count(*)/sum(count(*)) OVER (),1)::float8 AS pct, COALESCE(sum(net_amount),0)::float8 AS net_amount FROM crm_orders GROUP BY 1 ORDER BY 2 DESC", vec![]).await?;
    let drg = rows(&st.db, "SELECT COALESCE(region,'(none)') AS region, count(*) AS orders, round(100.0*count(*) FILTER (WHERE delivery_status='delayed')/count(*),1)::float8 AS delayed_pct, round(100.0*count(*) FILTER (WHERE delivery_status='cancelled')/count(*),1)::float8 AS cancelled_pct FROM crm_orders GROUP BY 1 ORDER BY 3 DESC", vec![]).await?;
    let disc = rows(&st.db, "SELECT COALESCE(discount_rate::text,'unknown') AS discount_rate, count(*) AS orders, round(avg(quantity),2)::float8 AS avg_quantity, round(avg(quantity*unit_price),2)::float8 AS avg_gross, round(avg(net_amount),2)::float8 AS avg_net FROM crm_orders GROUP BY 1 ORDER BY 1", vec![]).await?;
    let anom = row(&st.db, "SELECT count(*) FILTER (WHERE o.unit_price > 2*p.list_price) AS above_2x_catalog, count(*) FILTER (WHERE o.unit_price < 0.5*p.list_price) AS below_half_catalog, count(*) AS orders FROM crm_orders o JOIN crm_products p ON p.id=o.product_id WHERE p.list_price > 0", vec![]).await?;
    Ok(Json(json!({ "totals": t, "by_category": cat, "top_products": top, "by_region": region, "by_payment_method": pay, "by_loyalty_tier": tier,
        "delivery": { "overall": delivery, "by_region": drg, "note": "cancelled orders are still included in net_revenue; subtract the cancelled net_amount for realised revenue" },
        "discount_effect": disc, "price_anomalies": anom })))
}

async fn affinity(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let item = match p.get("level").map(String::as_str).unwrap_or("category") { "category" => "COALESCE(p.category,'(none)')", "product" => "p.name", _ => return Err(AppError::BadRequest("level must be category or product".into())) };
    let min_support: i64 = p.get("min_support").and_then(|v| v.parse().ok()).unwrap_or(10).max(1);
    let d = rows(&st.db, &format!("WITH cp AS (SELECT DISTINCT o.customer_id, {item} AS item FROM crm_orders o JOIN crm_products p ON p.id=o.product_id),
        n AS (SELECT count(DISTINCT customer_id)::float8 AS total FROM cp), single AS (SELECT item, count(*)::float8 AS c FROM cp GROUP BY 1),
        pair AS (SELECT a.item AS a, b.item AS b, count(*)::float8 AS both_c FROM cp a JOIN cp b ON a.customer_id=b.customer_id AND a.item < b.item GROUP BY 1,2)
        SELECT pair.a AS item_a, pair.b AS item_b, pair.both_c::int AS customers_buying_both, round((pair.both_c*n.total/(sa.c*sb.c))::numeric,3)::float8 AS lift
        FROM pair CROSS JOIN n JOIN single sa ON sa.item=pair.a JOIN single sb ON sb.item=pair.b WHERE pair.both_c >= {min_support} ORDER BY lift DESC LIMIT 15"), vec![]).await?;
    let max = d.iter().filter_map(|x| x["lift"].as_f64()).fold(0.0, f64::max);
    let min = d.iter().filter_map(|x| x["lift"].as_f64()).fold(f64::MAX, f64::min);
    Ok(Json(json!({ "level": p.get("level").cloned().unwrap_or("category".into()), "min_support_customers": min_support, "pairs": d,
        "interpretation": if d.is_empty() { "no pair reaches the minimum support".to_string() } else if max < 1.25 { format!("all lifts are close to 1 (max {max:.2}): customers buy across items independently, so there is no cross-sell signal to act on") }
                          else { format!("strongest affinity lift {max:.2} (weakest listed {min:.2}); lift > 1 means the pair is bought together more often than chance") } })))
}

async fn duplicates(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let k = kind(&p)?;
    let (key, filter) = match p.get("by").map(String::as_str).unwrap_or("email") { "email" => ("lower(email)", "email IS NOT NULL"), "phone" => ("phone", "phone IS NOT NULL"), _ => return Err(AppError::BadRequest("by must be email or phone".into())) };
    let d = rows(&st.db, &format!("SELECT {key} AS key, count(*) AS records, count(DISTINCT lower(coalesce(first_name,'')||' '||coalesce(last_name,''))) = 1 AS same_name,
        jsonb_agg(jsonb_build_object('id',id,'external_id',external_id,'name',trim(coalesce(first_name,'')||' '||coalesce(last_name,'')),'email',email,'phone',phone,'city',city,'total_donated',total_donated,'last_gift',last_activity_date) ORDER BY external_id) AS members
        FROM crm_customers WHERE kind=$1 AND {filter} GROUP BY 1 HAVING count(*) > 1 ORDER BY 2 DESC, 1 LIMIT 200"), vec![k.into()]).await?;
    let likely = d.iter().filter(|x| x["same_name"] == true).count();
    Ok(Json(json!({ "kind": k, "match_on": p.get("by").cloned().unwrap_or("email".into()), "groups": d.len(), "likely_same_person": likely,
        "note": "same_name = true suggests a duplicate record; shared contact details with different names are usually a shared household or data-entry error and need manual review", "data": d })))
}

async fn quality_report(State(st): State<AppState>) -> AppResult<Json<J>> {
    let batches = rows(&st.db, "SELECT DISTINCT ON (kind) kind, filename, created_at, rows_total, rows_inserted, rows_updated, rows_rejected, corrections, warnings FROM crm_import_batches ORDER BY kind, created_at DESC", vec![]).await?;
    let rejects = rows(&st.db, "SELECT b.kind, r.reason, count(*) AS rows FROM crm_import_rejects r JOIN crm_import_batches b ON b.id=r.batch_id
        WHERE b.id IN (SELECT DISTINCT ON (kind) id FROM crm_import_batches ORDER BY kind, created_at DESC) GROUP BY 1,2 ORDER BY 1,3 DESC", vec![]).await?;
    let pct = |c: &str| format!("round(100.0*count({c})/NULLIF(count(*),0),1)::float8 AS {c}_pct");
    let cons = row(&st.db, &format!("SELECT count(*) AS records, {}, {}, {}, {}, {} FROM crm_customers WHERE kind='consumer'", pct("email"), pct("gender"), pct("region"), pct("loyalty_tier"), pct("signup_date")), vec![]).await?;
    let don = row(&st.db, &format!("SELECT count(*) AS records, {}, {}, {}, {}, {} FROM crm_customers WHERE kind='donor'", pct("email"), pct("phone"), pct("state"), pct("postal_code"), pct("event_participation")), vec![]).await?;
    let ord = row(&st.db, &format!("SELECT count(*) AS records, {}, {}, {}, {}, count(DISTINCT order_date) AS distinct_order_dates FROM crm_orders", pct("discount_rate"), pct("order_date"), pct("delivery_status"), pct("payment_method")), vec![]).await?;
    let opp = row(&st.db, &format!("SELECT count(*) AS records, {}, {}, {}, count(*) FILTER (WHERE stage IN ('closed_won','closed_lost') AND close_date IS NULL) AS closed_without_close_date,
        count(*) FILTER (WHERE close_date < engage_date) AS close_before_engage, count(*) FILTER (WHERE stage='closed_won' AND COALESCE(close_value,0) <= 0) AS won_without_value FROM crm_opportunities WHERE external_ref IS NOT NULL", pct("account_id"), pct("engage_date"), pct("product_id")), vec![]).await?;
    let dup = rows(&st.db, "SELECT kind, count(*) AS duplicate_email_groups FROM (SELECT kind, lower(email) FROM crm_customers WHERE email IS NOT NULL GROUP BY 1,2 HAVING count(*)>1) x GROUP BY 1", vec![]).await?;
    let price = row(&st.db, "SELECT count(*) FILTER (WHERE o.unit_price > 2*p.list_price OR o.unit_price < 0.5*p.list_price) AS orders_outside_half_to_double_catalog, count(*) AS orders FROM crm_orders o JOIN crm_products p ON p.id=o.product_id WHERE p.list_price>0", vec![]).await?;
    Ok(Json(json!({ "imports": batches, "rejected_rows_by_reason": rejects,
        "completeness_pct": { "consumer_customers": cons, "donors": don, "orders": ord, "opportunities": opp },
        "duplicate_email_groups": dup, "order_price_vs_catalog": price })))
}
