//! Data table with typed cells, sortable headers and optional row actions.
use serde_json::Value;
use stylist::yew::styled_component;
use yew::prelude::*;

use crate::components::ui::{Badge, Icon};
use crate::schema::ColDef;
use crate::util;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Text,
    Money,
    Number,
    Date,
    Bool,
    Badge,
    Id,
    Json,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Column {
    pub key: String,
    pub label: String,
    pub kind: Kind,
    pub sortable: bool,
}

impl Column {
    pub fn from_col(c: &ColDef) -> Self {
        let kind = match c.ty {
            "d" => Kind::Money,
            "i" | "bi" | "f" => Kind::Number,
            "dt" | "ts" => Kind::Date,
            "b" => Kind::Bool,
            "u" => Kind::Id,
            "j" => Kind::Json,
            _ if !c.choices.is_empty() => Kind::Badge,
            _ => Kind::Text,
        };
        Column { key: c.name.into(), label: util::title_case(c.name), kind, sortable: c.ty != "j" }
    }
    pub fn plain(key: &str) -> Self {
        Column { key: key.into(), label: util::title_case(key), kind: Kind::Text, sortable: false }
    }
    /// Infer the cell kind from the data (used for endpoints without a schema).
    pub fn auto(key: &str, rows: &[Value]) -> Self {
        let sample = rows.iter().filter_map(|r| r.get(key)).find(|v| !v.is_null());
        let kind = match sample {
            Some(Value::Bool(_)) => Kind::Bool,
            Some(Value::Number(_)) => Kind::Number,
            Some(Value::Array(_)) | Some(Value::Object(_)) => Kind::Json,
            Some(Value::String(s)) => {
                let b = s.as_bytes();
                let datey = s.len() >= 10 && b[4] == b'-' && b[7] == b'-' && b[..4].iter().all(|c| c.is_ascii_digit());
                if datey { Kind::Date }
                else if matches!(key, "status" | "stage" | "state" | "priority" | "severity" | "rating" | "disposition" | "role") { Kind::Badge }
                else if s.parse::<f64>().is_ok() && util::is_money_key(key) && !key.ends_with("_id") && !key.ends_with("code") && !key.ends_with("number") { Kind::Money }
                else { Kind::Text }
            }
            _ => Kind::Text,
        };
        Column { key: key.into(), label: util::title_case(key), kind, sortable: false }
    }
}

#[derive(Properties, PartialEq)]
pub struct TableProps {
    pub columns: Vec<Column>,
    pub rows: Vec<Value>,
    #[prop_or_default]
    pub on_row: Option<Callback<Value>>,
    /// (column, descending)
    #[prop_or_default]
    pub sort: Option<(String, bool)>,
    #[prop_or_default]
    pub on_sort: Callback<String>,
    /// Extra cell appended to each row (action buttons).
    #[prop_or_default]
    pub row_extra: Option<Callback<Value, Html>>,
}

fn cell(col: &Column, row: &Value) -> Html {
    let v = row.get(&col.key).unwrap_or(&Value::Null);
    if v.is_null() {
        return html! { <span style="color:var(--muted)">{ "—" }</span> };
    }
    match col.kind {
        Kind::Badge => html! { <Badge text={v.as_str().map(String::from).unwrap_or_else(|| v.to_string())} /> },
        Kind::Bool => html! { <span style={if v.as_bool().unwrap_or(false) { "color:var(--ok)" } else { "color:var(--muted)" }}>{ if v.as_bool().unwrap_or(false) { "✓" } else { "—" } }</span> },
        Kind::Id => html! { <code title={v.as_str().unwrap_or_default().to_string()} style="font-size:12px;color:var(--muted)">{ util::short_id(v) }</code> },
        Kind::Date => html! { { util::display(&col.key, v).chars().take(if col.key.ends_with("_at") { 16 } else { 10 }).collect::<String>() } },
        Kind::Money => html! { { util::as_f64(v).map(util::money).unwrap_or_else(|| util::display(&col.key, v)) } },
        Kind::Number => html! { { util::display(&col.key, v) } },
        Kind::Json => html! { <code style="font-size:11.5px;color:var(--muted)">{ util::display(&col.key, v) }</code> },
        Kind::Text => html! { { util::display(&col.key, v) } },
    }
}

#[styled_component(DataTable)]
pub fn data_table(p: &TableProps) -> Html {
    let wrap = css!(
        r#"
        overflow-x: auto; border: 1px solid var(--line); border-radius: 8px;
        table { width: 100%; border-collapse: collapse; font-size: 13px; }
        th { text-align: left; font-size: 11.5px; letter-spacing: .02em; text-transform: uppercase; color: var(--muted); background: var(--surface-2); padding: 8px 12px; border-bottom: 1px solid var(--line); white-space: nowrap; position: sticky; top: 0; }
        th.sortable { cursor: pointer; user-select: none; } th.sortable:hover { color: var(--ink); }
        td { padding: 8px 12px; border-bottom: 1px solid var(--line); vertical-align: top; max-width: 360px; overflow-wrap: anywhere; }
        tr:last-child td { border-bottom: 0; }
        tr.click { cursor: pointer; } tr.click:hover td { background: var(--surface-2); }
        .r { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
        .act { white-space: nowrap; text-align: right; }
        "#
    );
    html! {
        <div class={wrap}>
            <table>
                <thead><tr>
                    { for p.columns.iter().map(|c| {
                        let key = c.key.clone();
                        let on_sort = p.on_sort.clone();
                        let active = p.sort.as_ref().filter(|(k, _)| *k == c.key);
                        let right = matches!(c.kind, Kind::Money | Kind::Number);
                        html! {
                            <th class={classes!(c.sortable.then_some("sortable"), right.then_some("r"))}
                                onclick={c.sortable.then(|| Callback::from(move |_| on_sort.emit(key.clone())))}
                                aria-sort={active.map(|(_, d)| if *d { "descending" } else { "ascending" })}>
                                { c.label.clone() }
                                { if let Some((_, desc)) = active { html! { <span style="margin-left:4px">{ if *desc { "▼" } else { "▲" } }</span> } } else { html! {} } }
                            </th>
                        }
                    }) }
                    { if p.row_extra.is_some() { html! { <th /> } } else { html! {} } }
                </tr></thead>
                <tbody>
                    { for p.rows.iter().map(|row| {
                        let on_row = p.on_row.clone();
                        let r2 = row.clone();
                        html! {
                            <tr class={on_row.is_some().then_some("click")} onclick={on_row.map(|cb| Callback::from(move |_| cb.emit(r2.clone())))}>
                                { for p.columns.iter().map(|c| html! { <td class={matches!(c.kind, Kind::Money | Kind::Number).then_some("r")}>{ cell(c, row) }</td> }) }
                                { if let Some(extra) = &p.row_extra { html! { <td class="act" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>{ extra.emit(row.clone()) }</td> } } else { html! {} } }
                            </tr>
                        }
                    }) }
                </tbody>
            </table>
            { if p.rows.is_empty() { html! { <div style="padding:28px;text-align:center;color:var(--muted)"><Icon name="search" size={20} /><div>{ "Nothing to show." }</div></div> } } else { html! {} } }
        </div>
    }
}
