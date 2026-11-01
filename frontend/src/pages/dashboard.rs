use serde_json::Value;
use stylist::yew::styled_component;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api::User;
use crate::components::charts::BarChart;
use crate::components::data::{show, use_fetch};
use crate::components::ui::{Alert, Card, Grid, PageHeader, StatTile};
use crate::{schema, util, Route, Session};

fn res(module: &str, slug: &str) -> Route {
    Route::Resource { module: module.into(), slug: slug.into() }
}
fn feed(section: &str, slug: &str) -> Route {
    Route::Feed { section: section.into(), slug: slug.into() }
}

/// Where each section of `/api/enterprise/overview` lives in the UI.
fn section_route(key: &str) -> Option<(&'static str, Route)> {
    Some(match key {
        "crm" => ("CRM", Route::Module { module: "crm".into() }),
        "projects" => ("Projects", Route::Module { module: "pm".into() }),
        "grc" => ("Risk & Compliance", Route::Module { module: "grc".into() }),
        "assets" => ("Assets", Route::Module { module: "am".into() }),
        "supply_chain" => ("Supply chain", Route::Module { module: "scm".into() }),
        "quality" => ("Quality", Route::Module { module: "qms".into() }),
        "ehs" => ("Health & Safety", Route::Module { module: "ehs".into() }),
        "finance" => ("Finance", Route::Module { module: "finance".into() }),
        "hr" => ("People", Route::Module { module: "hr".into() }),
        _ => return None,
    })
}

/// Open items that need somebody's attention: (section, metric, message, page to fix it).
pub fn attention(overview: &Value) -> Vec<(String, Route)> {
    let rules: [(&str, &str, &str, Route); 12] = [
        ("crm", "won_not_invoiced", "won opportunities have not been invoiced", res("crm", "opportunities")),
        ("crm", "open_cases", "support cases are open", res("crm", "cases")),
        ("projects", "invoices_not_in_ledger", "project invoices are not in the ledger", res("pm", "invoices")),
        ("grc", "high_or_critical", "open risks are rated high or critical", res("grc", "risks")),
        ("assets", "not_capitalised", "assets have not been capitalised", res("am", "assets")),
        ("supply_chain", "shipped_not_invoiced", "shipped sales orders have not been invoiced", feed("scm", "sales-orders")),
        ("supply_chain", "supplier_invoices_not_posted", "supplier invoices are not posted to the ledger", feed("scm", "supplier-invoices")),
        ("supply_chain", "backordered_sales_orders", "sales orders are backordered", feed("scm", "sales-orders")),
        ("supply_chain", "open_quality_incidents", "quality incidents are open", feed("scm", "quality-incidents")),
        ("quality", "open_nonconformances", "nonconformances are open", res("qms", "nonconformances")),
        ("ehs", "open_incidents", "safety incidents are open", res("ehs", "incidents")),
        ("finance", "receivables_overdue", "of receivables are overdue", feed("finance", "invoices")),
    ];
    rules
        .into_iter()
        .filter_map(|(sec, key, msg, route)| {
            let n = util::as_f64(overview.get(sec)?.get(key)?)?;
            (n > 0.0).then(|| (format!("{} {msg}", if key == "receivables_overdue" { util::money(n) } else { util::integer(n) }), route))
        })
        .collect()
}

fn greeting(u: &User) -> String {
    format!("Welcome back, {}", u.display_name().split_whitespace().next().unwrap_or("there"))
}

#[styled_component(DashboardPage)]
pub fn dashboard_page() -> Html {
    let session = use_context::<Session>().expect("session");
    let overview = use_fetch("/api/enterprise/overview".into(), 0);
    let attn = css!("display: grid; gap: 6px; a { display: flex; justify-content: space-between; gap: 12px; padding: 8px 12px; border-radius: 8px; background: var(--warn-soft); color: var(--warn); font-weight: 550; text-decoration: none; } a:hover { filter: brightness(.97); } a span.go { opacity: .7; }");
    let cards = css!("display: grid; grid-template-columns: repeat(auto-fill, minmax(340px, 1fr)); gap: 16px; align-items: start; > section { margin-bottom: 0; }");
    html! {
        <>
            <PageHeader title={greeting(&session.user)} subtitle="A live snapshot across every module you can access." />
            { show(&overview, |v| {
                let items = attention(v);
                let kpis: Vec<(&str, String, &str)> = [
                    ("finance", "cash", "Cash", ""),
                    ("finance", "receivables_open", "Open receivables", ""),
                    ("crm", "open_pipeline", "Open pipeline", ""),
                    ("projects", "active_budget", "Active project budget", ""),
                    ("supply_chain", "inventory_value_at_cost", "Inventory at cost", ""),
                    ("hr", "headcount", "Headcount", ""),
                ].iter().filter_map(|(s, k, l, _)| { let n = util::as_f64(v.get(*s)?.get(*k)?)?; Some((*l, if *k == "headcount" { util::integer(n) } else { util::compact(n) }, *k)) }).collect();
                let fin: Vec<(String, f64)> = ["cash", "receivables_open", "receivables_overdue", "payables_open"].iter().filter_map(|k| Some((util::title_case(k), util::as_f64(v.get("finance")?.get(*k)?)?))).collect();
                html! { <>
                    <Grid min={190}>{ for kpis.iter().map(|(l, val, _)| html! { <StatTile label={*l} value={val.clone()} /> }) }</Grid>
                    <div style="height:16px" />
                    { if items.is_empty() { html! { <Alert tone="ok">{ "Nothing needs attention right now." }</Alert> } } else { html! {
                        <Card title="Needs attention"><div class={attn}>
                            { for items.into_iter().map(|(msg, route)| html! { <Link<Route> to={route}><span>{ msg }</span><span class="go">{ "Review →" }</span></Link<Route>> }) }
                        </div></Card>
                    } } }
                    { if fin.is_empty() { html! {} } else { html! { <Card title="Financial position"><BarChart items={fin} money=true /></Card> } } }
                    <div class={cards}>
                        { for v.as_object().into_iter().flatten().filter_map(|(k, sec)| {
                            let (title, route) = section_route(k)?;
                            let m = sec.as_object()?;
                            Some(html! {
                                <Card title={title} actions={html! { <Link<Route> to={route}>{ "Open →" }</Link<Route>> }}>
                                    <Grid min={140}>{ for m.iter().filter(|(_, x)| !x.is_object()).map(|(mk, mv)| html! {
                                        <StatTile label={util::title_case(mk)} value={if util::is_money_key(mk) { util::compact(util::as_f64(mv).unwrap_or(0.0)) } else { util::display(mk, mv) }} />
                                    }) }</Grid>
                                </Card>
                            })
                        }) }
                    </div>
                </> }
            }) }
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn attention_lists_only_nonzero_items() {
        let v = json!({
            "crm": {"won_not_invoiced": 3, "open_cases": 0},
            "finance": {"receivables_overdue": 1250.5},
            "grc": {"high_or_critical": 0}
        });
        let a = attention(&v);
        assert_eq!(a.len(), 2);
        assert!(a[0].0.starts_with("3 won opportunities"));
        assert_eq!(a[0].1, res("crm", "opportunities"));
        assert!(a[1].0.starts_with("1,250.50 of receivables"));
    }

    #[test]
    fn attention_targets_exist() {
        // every route the dashboard links to must resolve to a real page
        let all = json!({
            "crm": {"won_not_invoiced": 1, "open_cases": 1}, "projects": {"invoices_not_in_ledger": 1}, "grc": {"high_or_critical": 1},
            "assets": {"not_capitalised": 1}, "supply_chain": {"shipped_not_invoiced": 1, "supplier_invoices_not_posted": 1, "backordered_sales_orders": 1, "open_quality_incidents": 1},
            "quality": {"open_nonconformances": 1}, "ehs": {"open_incidents": 1}, "finance": {"receivables_overdue": 1}
        });
        for (_, r) in attention(&all) {
            match r {
                Route::Resource { module, slug } => assert!(schema::find(&module, &slug).is_some(), "{module}/{slug}"),
                Route::Feed { section, slug } => assert!(crate::catalog::find_feed(&section, &slug).is_some(), "{section}/{slug}"),
                _ => {}
            }
        }
    }

    #[test]
    fn every_overview_section_is_linked() {
        for k in ["crm", "projects", "grc", "assets", "supply_chain", "quality", "ehs", "finance", "hr"] {
            assert!(section_route(k).is_some(), "{k}");
        }
    }
}
