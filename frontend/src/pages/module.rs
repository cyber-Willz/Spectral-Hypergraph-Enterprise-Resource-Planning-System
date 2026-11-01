use stylist::yew::styled_component;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::catalog;
use crate::components::data::{use_fetch, Fetch};
use crate::components::ui::{Alert, Card, PageHeader};
use crate::pages::insights::ReportQuery;
use crate::{schema, util, Route, Session};

#[derive(Properties, PartialEq)]
struct CountProps {
    path: String,
}

/// Total number of records behind a generated resource.
#[function_component(Count)]
fn count(p: &CountProps) -> Html {
    match use_fetch(format!("{}?per_page=1", p.path), 0) {
        Fetch::Ready(v) => html! { { v.get("total").and_then(|t| t.as_u64()).map(|t| util::integer(t as f64)).unwrap_or_default() } },
        _ => html! {},
    }
}

#[derive(Properties, PartialEq)]
pub struct ModuleProps {
    pub module: String,
}

#[styled_component(ModulePage)]
pub fn module_page(p: &ModuleProps) -> Html {
    let session = use_context::<Session>().expect("session");
    let tiles = css!("display: grid; grid-template-columns: repeat(auto-fill, minmax(210px, 1fr)); gap: 10px; a { display: flex; justify-content: space-between; align-items: center; gap: 8px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface-2); color: var(--ink); font-weight: 600; text-decoration: none; } a:hover { border-color: var(--brand); } a small { color: var(--muted); font-weight: 500; font-variant-numeric: tabular-nums; }");
    let m = p.module.as_str();
    if !crate::catalog::MODULES.iter().any(|(k, _)| *k == m) || !util::role_allows(&session.user.role, false, &format!("/api/{m}")) {
        return html! { <><PageHeader title="Module unavailable" /><Alert tone="warn">{ "This module does not exist or your role cannot open it." }</Alert></> };
    }
    let resources: Vec<_> = schema::in_module(m).collect();
    let feeds: Vec<_> = catalog::feeds_in(m).collect();
    let reports: Vec<_> = catalog::reports_in(m).collect();
    html! {
        <>
            <PageHeader title={schema::module_title(m)} subtitle="Records, lists and analytics for this module.">
                { if reports.is_empty() { html! {} } else { html! { <Link<Route> to={Route::Insights { module: m.to_string() }} classes="btn">{ "Open insights →" }</Link<Route>> } } }
            </PageHeader>
            { if resources.is_empty() { html! {} } else { html! { <Card title="Records"><div class={tiles.clone()}>
                { for resources.iter().map(|r| html! { <Link<Route> to={Route::Resource { module: r.module.into(), slug: r.slug.into() }}>{ r.title() }<small><Count path={r.path()} /></small></Link<Route>> }) }
            </div></Card> } } }
            { if feeds.is_empty() { html! {} } else { html! { <Card title="Lists and workflows"><div class={tiles.clone()}>
                { for feeds.iter().map(|f| html! { <Link<Route> to={Route::Feed { section: f.section.into(), slug: f.slug.into() }}>{ f.title }<small>{ "→" }</small></Link<Route>> }) }
            </div></Card> } } }
            { if reports.is_empty() { html! {} } else { html! { <Card title="Reports and analytics"><div class={tiles}>
                { for reports.iter().enumerate().map(|(i, r)| html! { <Link<Route, ReportQuery> to={Route::Insights { module: m.to_string() }} query={Some(ReportQuery { r: Some(i) })}>{ r.title }<small>{ "→" }</small></Link<Route, ReportQuery>> }) }
            </div></Card> } } }
        </>
    }
}
