use serde::{Deserialize, Serialize};
use stylist::yew::styled_component;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::catalog::{self, Report};
use crate::components::data::{show, use_fetch};
use crate::components::jsonview::JsonView;
use crate::components::ui::{Alert, Button, Card, PageHeader, Tabs};
use crate::{schema, util, Route};

/// `?r=<index>` selects the report inside a module.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default, Debug)]
pub struct ReportQuery {
    pub r: Option<usize>,
}

/// Final request path for a report given the chosen parameter values (`{name}` in the path, the rest in the query string).
pub fn report_url(report: &Report, values: &[(String, String)]) -> String {
    let mut path = report.path.to_string();
    let mut query = Vec::new();
    for p in report.params {
        let v = values.iter().find(|(k, _)| k == p.name).map(|(_, v)| v.clone()).unwrap_or_else(|| p.options[0].to_string());
        if p.in_path {
            path = path.replace(&format!("{{{}}}", p.name), &util::percent_encode(&v));
        } else {
            query.push((p.name.to_string(), v));
        }
    }
    util::with_query(&path, &query)
}

#[derive(Properties, PartialEq)]
pub struct InsightsProps {
    pub module: String,
}

#[styled_component(InsightsPage)]
pub fn insights_page(p: &InsightsProps) -> Html {
    let reports: Vec<&'static Report> = catalog::reports_in(&p.module).collect();
    let navigator = use_navigator().expect("navigator");
    let location = use_location();
    let selected = location.as_ref().and_then(|l| l.query::<ReportQuery>().ok()).and_then(|q| q.r).unwrap_or(0).min(reports.len().saturating_sub(1));
    let params = use_state(Vec::<(String, String)>::new);
    let raw = use_state(|| false);
    let tick = use_state(|| 0u32);
    let bar = css!("display: flex; gap: 10px; align-items: center; flex-wrap: wrap; margin-bottom: 12px; select { background: var(--surface); border: 1px solid var(--line-strong); border-radius: 7px; padding: 6px 9px; } label { color: var(--muted); font-size: 12px; display: flex; gap: 6px; align-items: center; } .sp { flex: 1; } code { color: var(--muted); font-size: 12px; }");
    let pre = css!("background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; padding: 12px; overflow: auto; max-height: 560px; font-size: 12px; margin: 0;");
    let Some(report) = reports.get(selected).copied() else {
        return html! { <><PageHeader title="Insights" /><Alert tone="warn">{ "This module has no reports." }</Alert></> };
    };
    let url = report_url(report, &params);
    let fetched = use_fetch(url.clone(), *tick);
    let on_select = {
        let (navigator, module, params) = (navigator.clone(), p.module.clone(), params.clone());
        Callback::from(move |i: usize| { params.set(vec![]); let _ = navigator.push_with_query(&Route::Insights { module: module.clone() }, &ReportQuery { r: Some(i) }); })
    };
    let labels: Vec<AttrValue> = reports.iter().map(|r| AttrValue::from(r.title)).collect();
    html! {
        <>
            <PageHeader title={format!("{} insights", schema::module_title(&p.module))} subtitle="Live analytics from the API. Numbers, tables and charts update when you reload." />
            <Tabs {labels} active={selected} {on_select} />
            <Card title={report.title} actions={html! { <code>{ url.clone() }</code> }}>
                <div class={bar}>
                    { for report.params.iter().map(|prm| {
                        let current = params.iter().find(|(k, _)| k == prm.name).map(|(_, v)| v.clone()).unwrap_or_else(|| prm.options[0].to_string());
                        let params = params.clone();
                        let name = prm.name;
                        let onchange = Callback::from(move |e: Event| {
                            if let Some(s) = e.target_dyn_into::<web_sys::HtmlSelectElement>() {
                                let mut next: Vec<(String, String)> = params.iter().filter(|(k, _)| k != name).cloned().collect();
                                next.push((name.to_string(), s.value()));
                                params.set(next);
                            }
                        });
                        html! { <label>{ util::title_case(name) }<select {onchange}>{ for prm.options.iter().map(|o| html! { <option value={*o} selected={current == *o}>{ util::title_case(o) }</option> }) }</select></label> }
                    }) }
                    <span class="sp" />
                    <Button small=true onclick={{ let raw = raw.clone(); Callback::from(move |_| raw.set(!*raw)) }}>{ if *raw { "Formatted view" } else { "Raw JSON" } }</Button>
                    <Button small=true icon="refresh" title="Reload" onclick={{ let tick = tick.clone(); Callback::from(move |_| tick.set(*tick + 1)) }} />
                </div>
                { show(&fetched, |v| if *raw { html! { <pre class={pre}>{ serde_json::to_string_pretty(v).unwrap_or_default() }</pre> } } else { html! { <JsonView value={v.clone()} /> } }) }
            </Card>
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(title: &str) -> &'static Report {
        catalog::REPORTS.iter().find(|r| r.title == title).unwrap()
    }

    #[test]
    fn plain_report_url() {
        assert_eq!(report_url(find("Trial balance"), &[]), "/api/finance/reports/trial-balance");
    }

    #[test]
    fn query_params_default_to_first_option() {
        assert_eq!(report_url(find("Sales performance"), &[]), "/api/crm/sales/performance?by=agent");
        assert_eq!(report_url(find("Sales performance"), &[("by".into(), "regional_office".into())]), "/api/crm/sales/performance?by=regional_office");
    }

    #[test]
    fn path_params_are_substituted() {
        assert_eq!(report_url(find("Portfolio breakdown"), &[("dimension".into(), "region".into())]), "/api/pm/portfolio/by/region");
        assert_eq!(report_url(find("Portfolio breakdown"), &[]), "/api/pm/portfolio/by/type");
    }
}
