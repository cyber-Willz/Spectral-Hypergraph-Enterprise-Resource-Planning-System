//! Generic renderer for analytics responses: objects become stat tiles and cards, arrays of records become
//! tables (with a chart when there is a label + numeric column), scalars are formatted by their key.
use serde_json::{Map, Value};
use stylist::yew::styled_component;
use stylist::StyleSource;
use yew::prelude::*;

use crate::components::charts::{BarChart, LineChart};
use crate::components::table::{Column, DataTable};
use crate::components::ui::{Card, Grid, StatTile};
use crate::util;

#[derive(Properties, PartialEq)]
pub struct JsonProps {
    pub value: Value,
    #[prop_or_default]
    pub depth: u8,
}

fn is_scalar(v: &Value) -> bool {
    !matches!(v, Value::Object(_) | Value::Array(_))
}

/// Tiles for numbers that signal a problem when non-zero.
fn tone_for_metric(key: &str, v: &Value) -> &'static str {
    let n = util::as_f64(v).unwrap_or(0.0);
    let bad = ["overdue", "not_", "error", "fail", "breach", "stale", "exceed", "mismatch", "unposted", "high_or_critical", "violation", "orphan"];
    if n > 0.0 && bad.iter().any(|b| key.contains(b)) { "warn" } else { "" }
}

/// Pick (label column, value column) when a table is chartable.
pub fn chart_axes(rows: &[Value], cols: &[String]) -> Option<(String, String)> {
    if rows.len() < 2 || rows.len() > 40 {
        return None;
    }
    let first = rows.first()?;
    let numeric = |c: &String| rows.iter().all(|r| r.get(c).map(|v| v.is_null() || util::as_f64(v).is_some()).unwrap_or(true)) && rows.iter().any(|r| r.get(c).map(|v| util::as_f64(v).is_some() && !v.is_string() || (v.is_string() && util::is_money_key(c))).unwrap_or(false));
    let label = cols.iter().find(|c| matches!(first.get(*c), Some(Value::String(_))) && !numeric(c) && !c.ends_with("_id") && *c != "id")?;
    let prefer = ["value", "amount", "total", "revenue", "count", "score", "rate", "cost", "balance", "n"];
    let nums: Vec<&String> = cols.iter().filter(|c| numeric(c) && !c.ends_with("_id") && *c != "id" && *c != label).collect();
    let value = nums.iter().find(|c| prefer.iter().any(|p| c.contains(p))).or(nums.first())?;
    Some((label.clone(), (*value).clone()))
}

fn looks_like_time(labels: &[String]) -> bool {
    labels.len() >= 4 && labels.iter().all(|l| l.len() >= 4 && l.as_bytes()[..4].iter().all(|b| b.is_ascii_digit()))
}

#[styled_component(JsonView)]
pub fn json_view(p: &JsonProps) -> Html {
    let note = css!("color: var(--muted); font-size: 12.5px; margin: 6px 0;");
    let chips = css!("display: flex; flex-wrap: wrap; gap: 6px; span { background: var(--surface-2); border: 1px solid var(--line); border-radius: 99px; padding: 1px 10px; font-size: 12.5px; }");
    let pre = css!("background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; padding: 10px; overflow: auto; max-height: 320px; font-size: 12px; margin: 0;");
    let para = css!("margin: 8px 0; color: var(--ink-2); white-space: pre-wrap; overflow-wrap: anywhere;");
    if p.depth > 5 {
        return html! { <pre class={pre}>{ serde_json::to_string_pretty(&p.value).unwrap_or_default() }</pre> };
    }
    match &p.value {
        Value::Object(m) => object_view(m, p.depth, &note, &para),
        Value::Array(a) => {
            if a.is_empty() {
                return html! { <div class={note}>{ "No data." }</div> };
            }
            if a.iter().all(|v| v.is_object()) {
                let cols = util::auto_columns(a, 9);
                let columns: Vec<Column> = cols.iter().map(|c| Column::auto(c, a)).collect();
                let chart = chart_axes(a, &cols).map(|(l, v)| {
                    let pts: Vec<(String, f64)> = a.iter().filter_map(|r| Some((util::display(&l, r.get(&l)?), util::as_f64(r.get(&v)?)?))).collect();
                    let labels: Vec<String> = pts.iter().map(|(l, _)| l.clone()).collect();
                    let body = if looks_like_time(&labels) { html! { <LineChart points={pts} /> } } else { html! { <BarChart items={pts.into_iter().take(14).collect::<Vec<_>>()} /> } };
                    html! { <div style="margin-bottom:12px"><div class={note.clone()}>{ format!("{} by {}", util::title_case(&v), util::title_case(&l).to_lowercase()) }</div>{ body }</div> }
                });
                html! { <>{ chart }<DataTable {columns} rows={a.clone()} /></> }
            } else if a.iter().all(is_scalar) {
                html! { <div class={chips}>{ for a.iter().take(80).map(|v| html! { <span>{ util::display("", v) }</span> }) }</div> }
            } else {
                html! { <pre class={pre}>{ serde_json::to_string_pretty(&p.value).unwrap_or_default() }</pre> }
            }
        }
        v => html! { <span>{ util::display("", v) }</span> },
    }
}

fn object_view(m: &Map<String, Value>, depth: u8, note: &StyleSource, para: &StyleSource) -> Html {
    let tiles: Vec<(&String, &Value)> = m.iter().filter(|(_, v)| is_scalar(v) && v.as_str().map(|s| s.len() <= 60).unwrap_or(true)).collect();
    let long: Vec<(&String, &Value)> = m.iter().filter(|(_, v)| v.as_str().map(|s| s.len() > 60).unwrap_or(false)).collect();
    let nested: Vec<(&String, &Value)> = m.iter().filter(|(_, v)| !is_scalar(v)).collect();
    html! {
        <>
            { if tiles.is_empty() { html! {} } else { html! {
                <div style="margin-bottom:12px"><Grid min={170}>
                    { for tiles.iter().map(|(k, v)| html! { <StatTile label={util::title_case(k)} value={util::display(k, v)} tone={tone_for_metric(k, v)} /> }) }
                </Grid></div>
            } } }
            { for long.iter().map(|(k, v)| html! { <p class={para.clone()}><b>{ format!("{}: ", util::title_case(k)) }</b>{ v.as_str().unwrap_or_default() }</p> }) }
            { for nested.iter().map(|(k, v)| {
                let empty = matches!(v, Value::Array(a) if a.is_empty()) || matches!(v, Value::Object(o) if o.is_empty());
                if empty { return html! { <div class={note.clone()}>{ format!("{}: none", util::title_case(k)) }</div> }; }
                html! { <Card title={util::title_case(k)}><JsonView value={(*v).clone()} depth={depth + 1} /></Card> }
            }) }
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_chart_axes() {
        let rows = vec![json!({"region":"North","revenue":120.5,"id":1}), json!({"region":"South","revenue":80.0,"id":2})];
        let cols = vec!["region".to_string(), "revenue".to_string()];
        assert_eq!(chart_axes(&rows, &cols), Some(("region".into(), "revenue".into())));
        // one row is not worth a chart
        assert_eq!(chart_axes(&rows[..1], &cols), None);
        // no numeric column -> no chart
        let names = vec![json!({"a":"x","b":"y"}), json!({"a":"z","b":"w"})];
        assert_eq!(chart_axes(&names, &["a".into(), "b".into()]), None);
    }

    #[test]
    fn time_labels() {
        assert!(looks_like_time(&["2026-01".into(), "2026-02".into(), "2026-03".into(), "2026-04".into()]));
        assert!(!looks_like_time(&["North".into(), "South".into()]));
    }

    #[test]
    fn warns_only_on_problem_metrics() {
        assert_eq!(tone_for_metric("receivables_overdue", &json!(5)), "warn");
        assert_eq!(tone_for_metric("receivables_overdue", &json!(0)), "");
        assert_eq!(tone_for_metric("headcount", &json!(40)), "");
    }
}
