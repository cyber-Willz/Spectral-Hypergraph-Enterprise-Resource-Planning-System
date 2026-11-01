//! The workhorse list page: server-paged for generated CRUD resources, client-paged for hand-written endpoints.
//! Search / filter / sort, CSV export, detail view, create / edit / delete and workflow actions.
use serde_json::Value;
use stylist::yew::styled_component;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::browser;
use crate::catalog::{self, RowAction};
use crate::components::data::{use_fetch, Fetch};
use crate::components::form::{ActionForm, DynamicForm, JsonForm};
use crate::components::jsonview::JsonView;
use crate::components::table::{Column, DataTable};
use crate::components::ui::{Alert, Button, Card, Modal, PageHeader, Pager, Spinner, Variant};
use crate::schema::ResDef;
use crate::util;
use crate::Session;

const PER_PAGE: u32 = 25;

#[derive(Properties, PartialEq)]
pub struct ListProps {
    pub title: AttrValue,
    #[prop_or_default]
    pub subtitle: AttrValue,
    pub path: AttrValue,
    #[prop_or_default]
    pub def: Option<&'static ResDef>,
    /// Hand-written endpoints: allow creating through a JSON body.
    #[prop_or_default]
    pub create_json: bool,
}

#[derive(Clone, PartialEq)]
enum Editor {
    Closed,
    Create,
    Edit(Value),
}

/// Sort rows client-side by one column (numbers numerically, everything else as text).
pub fn sort_rows(rows: &mut [Value], col: &str, desc: bool) {
    rows.sort_by(|a, b| {
        let (x, y) = (a.get(col).unwrap_or(&Value::Null), b.get(col).unwrap_or(&Value::Null));
        let ord = match (util::as_f64(x), util::as_f64(y)) {
            (Some(p), Some(q)) => p.partial_cmp(&q).unwrap_or(std::cmp::Ordering::Equal),
            _ => util::display(col, x).to_lowercase().cmp(&util::display(col, y).to_lowercase()),
        };
        if desc { ord.reverse() } else { ord }
    });
}

/// Case-insensitive substring match over the displayed text of every scalar field.
pub fn row_matches(row: &Value, needle: &str) -> bool {
    let n = needle.to_lowercase();
    row.as_object().map(|m| m.values().any(|v| !v.is_array() && !v.is_object() && util::display("", v).to_lowercase().contains(&n))).unwrap_or(false)
}

#[styled_component(ListView)]
pub fn list_view(p: &ListProps) -> Html {
    let session = use_context::<Session>().expect("session");
    let role = session.user.role.clone();
    let path = p.path.to_string();
    let can_write = util::role_allows(&role, true, &path);
    let can_delete = util::can_delete(&role, &path) && can_write;
    let toolbar = css!("display: flex; gap: 8px; flex-wrap: wrap; align-items: center; margin-bottom: 12px; select, input { background: var(--surface); border: 1px solid var(--line-strong); border-radius: 7px; padding: 6px 9px; } input[type=search] { min-width: 220px; } .chip { display: inline-flex; gap: 6px; align-items: center; background: var(--brand-soft); color: var(--brand); border-radius: 99px; padding: 2px 4px 2px 10px; font-size: 12px; font-weight: 600; } .chip button { background: none; border: 0; cursor: pointer; color: inherit; font-size: 14px; line-height: 1; } .sp { flex: 1; }");
    let flash_css = css!("margin-bottom: 8px;");

    let page = use_state(|| 1u32);
    let sort = use_state(|| Option::<(String, bool)>::None);
    let filters = use_state(Vec::<(String, String)>::new);
    let search = use_state(String::new);
    let fcol = use_state(String::new);
    let fval = use_state(String::new);
    let tick = use_state(|| 0u32);
    let selected = use_state(|| Option::<Value>::None);
    let editor = use_state(|| Editor::Closed);
    let pending = use_state(|| Option::<(&'static RowAction, Value)>::None);
    let busy = use_state(|| false);
    let error = use_state(|| Option::<String>::None);
    let flash = use_state(|| Option::<String>::None);

    // ---- request
    let url = match p.def {
        Some(_) => {
            let mut q = vec![("page".to_string(), page.to_string()), ("per_page".to_string(), PER_PAGE.to_string())];
            if let Some((c, desc)) = &*sort {
                q.push(("sort".into(), c.clone()));
                q.push(("order".into(), if *desc { "desc" } else { "asc" }.into()));
            }
            q.extend(filters.iter().cloned());
            util::with_query(&path, &q)
        }
        None => path.clone(),
    };
    let fetched = use_fetch(url, *tick);

    // ---- helpers
    let reload = {
        let tick = tick.clone();
        Callback::from(move |_| tick.set(*tick + 1))
    };
    let close_all = {
        let (editor, selected, pending, error) = (editor.clone(), selected.clone(), pending.clone(), error.clone());
        Callback::from(move |_| {
            editor.set(Editor::Closed);
            selected.set(None);
            pending.set(None);
            error.set(None);
        })
    };
    // run a request, then refresh and report
    let run = {
        let (busy, error, flash, tick, editor, selected, pending) = (busy.clone(), error.clone(), flash.clone(), tick.clone(), editor.clone(), selected.clone(), pending.clone());
        Callback::from(move |(method, url, body, done): (&'static str, String, Option<Value>, String)| {
            busy.set(true);
            error.set(None);
            let (busy, error, flash, tick, editor, selected, pending) = (busy.clone(), error.clone(), flash.clone(), tick.clone(), editor.clone(), selected.clone(), pending.clone());
            spawn_local(async move {
                match api::send(method, &url, body.as_ref()).await {
                    Ok(_) => {
                        flash.set(Some(done));
                        editor.set(Editor::Closed);
                        selected.set(None);
                        pending.set(None);
                        tick.set(*tick + 1);
                    }
                    Err(e) => error.set(Some(e.message)),
                }
                busy.set(false);
            });
        })
    };

    // ---- rows
    let (rows, total, columns) = match &fetched {
        Fetch::Ready(v) => {
            let (mut all, total) = util::rows_of(v);
            match p.def {
                Some(def) => {
                    let mut cols: Vec<Column> = def.list_columns().into_iter().map(Column::from_col).collect();
                    cols.push(Column { key: "created_at".into(), label: "Created".into(), kind: crate::components::table::Kind::Date, sortable: true });
                    (all, total, cols)
                }
                None => {
                    let cols: Vec<Column> = util::auto_columns(&all, 9).iter().map(|c| Column::auto(c, &all)).collect();
                    if !search.is_empty() { all.retain(|r| row_matches(r, &search)); }
                    if let Some((c, desc)) = &*sort { sort_rows(&mut all, c, *desc); }
                    let n = all.len() as u64;
                    let start = ((*page - 1) * PER_PAGE) as usize;
                    let slice: Vec<Value> = all.into_iter().skip(start).take(PER_PAGE as usize).collect();
                    (slice, Some(n), cols.into_iter().map(|mut c| { c.sortable = true; c }).collect())
                }
            }
        }
        _ => (vec![], None, vec![]),
    };

    // ---- row actions
    let actions = catalog::actions_for(&path);
    let can_act = can_write || actions.iter().any(|a| util::role_allows(&role, true, &format!("{path}/x{}", a.suffix)));
    let row_actions = {
        let pending = pending.clone();
        let role = role.clone();
        let path = path.clone();
        let actions = actions.clone();
        Callback::from(move |row: Value| -> Html {
            let mine: Vec<&&RowAction> = actions.iter().filter(|a| a.applies(&row) && util::role_allows(&role, true, &format!("{path}/x{}", a.suffix))).take(2).collect();
            html! { <>{ for mine.into_iter().map(|a| {
                let (pending, row, a) = (pending.clone(), row.clone(), *a);
                html! { <Button small=true variant={if a.danger { Variant::Danger } else { Variant::Secondary }} onclick={Callback::from(move |_| pending.set(Some((a, row.clone()))))}>{ a.label }</Button> }
            }) }</> }
        })
    };
    let show_actions = can_act && !actions.is_empty();

    let on_sort = {
        let (sort, page) = (sort.clone(), page.clone());
        Callback::from(move |col: String| {
            sort.set(match &*sort { Some((c, desc)) if *c == col => if *desc { None } else { Some((col, true)) }, _ => Some((col, false)) });
            page.set(1);
        })
    };
    let on_page = { let page = page.clone(); Callback::from(move |n| page.set(n)) };
    let on_row = { let selected = selected.clone(); Callback::from(move |r: Value| selected.set(Some(r))) };

    // ---- toolbar
    let enum_filters = p.def.map(|d| d.cols.iter().filter(|c| !c.choices.is_empty()).take(3).collect::<Vec<_>>()).unwrap_or_default();
    let toolbar_html = html! {
        <div class={toolbar}>
            { if p.def.is_none() { html! {
                <input type="search" placeholder="Search this list…" aria-label="Search" value={(*search).clone()}
                    oninput={{ let (search, page) = (search.clone(), page.clone()); Callback::from(move |e: InputEvent| { if let Some(i) = e.target_dyn_into::<web_sys::HtmlInputElement>() { search.set(i.value()); page.set(1); } }) }} />
            } } else { html! {} } }
            { for enum_filters.iter().map(|c| {
                let name = c.name;
                let current = filters.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
                let (filters, page) = (filters.clone(), page.clone());
                let onchange = Callback::from(move |e: Event| {
                    if let Some(s) = e.target_dyn_into::<web_sys::HtmlSelectElement>() {
                        let mut f: Vec<(String, String)> = filters.iter().filter(|(k, _)| k != name).cloned().collect();
                        if !s.value().is_empty() { f.push((name.to_string(), s.value())); }
                        filters.set(f);
                        page.set(1);
                    }
                });
                html! { <select {onchange} aria-label={util::title_case(name)}>
                    <option value="" selected={current.is_empty()}>{ format!("{}: all", util::title_case(name)) }</option>
                    { for c.choices.iter().map(|o| html! { <option value={*o} selected={current == *o}>{ util::title_case(o) }</option> }) }
                </select> }
            }) }
            { if let Some(def) = p.def { html! { <>
                <select aria-label="Filter column" onchange={{ let fcol = fcol.clone(); Callback::from(move |e: Event| { if let Some(s) = e.target_dyn_into::<web_sys::HtmlSelectElement>() { fcol.set(s.value()); } }) }}>
                    <option value="">{ "Filter by…" }</option>
                    { for def.filterable().into_iter().filter(|c| c.choices.is_empty()).map(|c| html! { <option value={c.name} selected={*fcol == c.name}>{ util::title_case(c.name) }</option> }) }
                </select>
                <input type="text" placeholder="equals…" aria-label="Filter value" value={(*fval).clone()}
                    oninput={{ let fval = fval.clone(); Callback::from(move |e: InputEvent| { if let Some(i) = e.target_dyn_into::<web_sys::HtmlInputElement>() { fval.set(i.value()); } }) }} />
                <Button small=true disabled={fcol.is_empty() || fval.is_empty()} onclick={{
                    let (filters, fcol, fval, page) = (filters.clone(), fcol.clone(), fval.clone(), page.clone());
                    Callback::from(move |_| {
                        let mut f: Vec<(String, String)> = filters.iter().filter(|(k, _)| *k != *fcol).cloned().collect();
                        f.push(((*fcol).clone(), (*fval).clone()));
                        filters.set(f);
                        fval.set(String::new());
                        page.set(1);
                    })
                }}>{ "Add filter" }</Button>
            </> } } else { html! {} } }
            { for filters.iter().filter(|(k, _)| p.def.and_then(|d| d.col(k)).map(|c| c.choices.is_empty()).unwrap_or(true)).map(|(k, v)| {
                let (filters, k2, page) = (filters.clone(), k.clone(), page.clone());
                html! { <span class="chip">{ format!("{} = {}", util::title_case(k), v) }<button type="button" aria-label="Remove filter" onclick={Callback::from(move |_| { filters.set(filters.iter().filter(|(x, _)| *x != k2).cloned().collect()); page.set(1); })}>{ "×" }</button></span> }
            }) }
            <span class="sp" />
            <Button small=true icon="refresh" title="Reload" onclick={reload.clone()} />
            <Button small=true icon="download" title="Download this page as CSV" disabled={rows.is_empty()} onclick={{
                let (rows, columns, title) = (rows.clone(), columns.clone(), p.title.to_string());
                Callback::from(move |_| { let keys: Vec<String> = columns.iter().map(|c| c.key.clone()).collect(); let _ = browser::download_text(&format!("{}.csv", title.to_lowercase().replace(' ', "_")), "text/csv", &browser::to_csv(&rows, &keys)); })
            }}>{ "CSV" }</Button>
        </div>
    };

    // ---- modals
    let id_of = |r: &Value| util::row_id(r, "id").unwrap_or_default();
    let modal = match (&*pending, &*editor, &*selected) {
        (Some((a, row)), _, _) => {
            let (a, row) = (*a, row.clone());
            let url = format!("{path}/{}{}", id_of(&row), a.suffix);
            let run = run.clone();
            let done = format!("{} done.", a.label);
            let on_submit = Callback::from(move |body| run.emit(("POST", url.clone(), Some(body), done.clone())));
            html! { <Modal title={a.label} on_close={close_all.clone()}>
                { if !a.confirm.is_empty() { html! { <p>{ a.confirm }</p> } } else if a.fields.is_empty() { html! { <p>{ format!("{} “{}”?", a.label, util::row_label(&row)) }</p> } } else { html! {} } }
                <ActionForm fields={a.fields} submit_label={a.label} on_submit={on_submit} on_cancel={close_all.clone()} busy={*busy} error={(*error).clone().map(AttrValue::from)} danger={a.danger} />
            </Modal> }
        }
        (None, Editor::Create, _) => {
            let run = run.clone();
            let create_path = path.clone();
            let cb = Callback::from(move |body: Value| run.emit(("POST", create_path.clone(), Some(body), "Created.".into())));
            match p.def {
                Some(def) => html! { <Modal title={format!("New {}", singular(&def.title()))} on_close={close_all.clone()} wide=true>
                    <DynamicForm {def} on_submit={cb} on_cancel={close_all.clone()} busy={*busy} error={(*error).clone().map(AttrValue::from)} /></Modal> },
                None => html! { <Modal title={format!("New — {}", p.title)} on_close={close_all.clone()} wide=true>
                    <JsonForm on_submit={cb} on_cancel={close_all.clone()} busy={*busy} error={(*error).clone().map(AttrValue::from)} /></Modal> },
            }
        }
        (None, Editor::Edit(rec), _) => {
            let (run, rec2) = (run.clone(), rec.clone());
            let url = format!("{path}/{}", id_of(&rec2));
            let cb = Callback::from(move |body: Value| run.emit(("PATCH", url.clone(), Some(body), "Saved.".into())));
            match p.def {
                Some(def) => html! { <Modal title={format!("Edit {}", singular(&def.title()))} on_close={close_all.clone()} wide=true>
                    <DynamicForm {def} initial={Some(rec.clone())} on_submit={cb} on_cancel={close_all.clone()} busy={*busy} error={(*error).clone().map(AttrValue::from)} /></Modal> },
                None => html! {},
            }
        }
        (None, Editor::Closed, Some(row)) => {
            let mut shown = row.clone();
            if let Value::Object(m) = &mut shown { m.retain(|_, v| !v.is_null()); }
            let mine: Vec<&&RowAction> = actions.iter().filter(|a| a.applies(row) && util::role_allows(&role, true, &format!("{path}/x{}", a.suffix))).collect();
            let edit_row = row.clone();
            let del_url = format!("{path}/{}", id_of(row));
            let run = run.clone();
            let confirm_name = util::row_label(row);
            html! { <Modal title={util::row_label(row)} on_close={close_all.clone()} wide=true>
                <div style="display:flex;gap:8px;flex-wrap:wrap;margin-bottom:14px">
                    { for mine.into_iter().map(|a| { let (pending, row, a) = (pending.clone(), row.clone(), *a); html! { <Button variant={if a.danger { Variant::Danger } else { Variant::Secondary }} onclick={Callback::from(move |_| pending.set(Some((a, row.clone()))))}>{ a.label }</Button> } }) }
                    { if p.def.map(|d| d.can('u')).unwrap_or(false) && can_write { html! { <Button icon="edit" onclick={{ let editor = editor.clone(); Callback::from(move |_| editor.set(Editor::Edit(edit_row.clone()))) }}>{ "Edit" }</Button> } } else { html! {} } }
                    { if p.def.map(|d| d.can('d')).unwrap_or(false) && can_delete { html! { <Button variant={Variant::Danger} icon="trash" onclick={Callback::from(move |_| {
                        if web_sys::window().and_then(|w| w.confirm_with_message(&format!("Delete “{confirm_name}”? This cannot be undone.")).ok()).unwrap_or(false) {
                            run.emit(("DELETE", del_url.clone(), None, "Deleted.".into()));
                        }
                    })}>{ "Delete" }</Button> } } else { html! {} } }
                </div>
                { if let Some(e) = &*error { html! { <Alert tone="err">{ e.clone() }</Alert> } } else { html! {} } }
                <JsonView value={shown} />
            </Modal> }
        }
        _ => html! {},
    };

    let body = match &fetched {
        Fetch::Loading => html! { <Spinner /> },
        Fetch::Failed(e) => html! { <Alert tone="err">{ e.message.clone() }</Alert> },
        Fetch::Ready(_) => html! { <>
            <DataTable columns={columns} rows={rows.clone()} on_row={Some(on_row)} sort={(*sort).clone()} on_sort={on_sort}
                row_extra={show_actions.then_some(row_actions)} />
            <Pager page={*page} per_page={PER_PAGE} total={total} shown={rows.len()} on_page={on_page} />
        </> },
    };

    html! {
        <>
            <PageHeader title={p.title.clone()} subtitle={p.subtitle.clone()}>
                { if can_write && (p.def.map(|d| d.can('c')).unwrap_or(p.create_json)) { html! { <Button variant={Variant::Primary} icon="plus" onclick={{ let editor = editor.clone(); Callback::from(move |_| editor.set(Editor::Create)) }}>{ "New" }</Button> } } else { html! {} } }
            </PageHeader>
            { if let Some(m) = &*flash { html! { <div class={flash_css}><Alert tone="ok">{ m.clone() }<button type="button" style="float:right;background:none;border:0;cursor:pointer;color:inherit" onclick={{ let flash = flash.clone(); Callback::from(move |_| flash.set(None)) }}>{ "×" }</button></Alert></div> } } else { html! {} } }
            <Card pad=false>
                <div style="padding:14px 16px 4px">{ toolbar_html }</div>
                <div style="padding:0 16px 14px">{ body }</div>
            </Card>
            { modal }
        </>
    }
}

fn singular(s: &str) -> String {
    let lower = s.to_lowercase();
    if let Some(stem) = lower.strip_suffix("ies") { format!("{stem}y") }
    else if lower.ends_with("sses") || lower.ends_with("xes") { lower[..lower.len() - 2].to_string() }
    else if let Some(stem) = lower.strip_suffix('s') { if lower.ends_with("ss") { lower } else { stem.to_string() } }
    else { lower }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_numbers_numerically() {
        let mut rows = vec![json!({"n": 10}), json!({"n": 9}), json!({"n": "100.5"})];
        sort_rows(&mut rows, "n", false);
        assert_eq!(rows.iter().map(|r| util::as_f64(&r["n"]).unwrap()).collect::<Vec<_>>(), vec![9.0, 10.0, 100.5]);
        sort_rows(&mut rows, "n", true);
        assert_eq!(rows[0]["n"], "100.5");
    }

    #[test]
    fn sorts_text_case_insensitively() {
        let mut rows = vec![json!({"s":"beta"}), json!({"s":"Alpha"}), json!({"s": null})];
        sort_rows(&mut rows, "s", false);
        assert_eq!(rows[0]["s"], "Alpha");
        assert_eq!(rows[2]["s"], Value::Null);
    }

    #[test]
    fn searches_scalars_only() {
        let r = json!({"name":"Acme Corp","tags":["secret"],"n":5});
        assert!(row_matches(&r, "acme"));
        assert!(!row_matches(&r, "secret"));
    }

    #[test]
    fn singularises_titles() {
        assert_eq!(singular("Accounts"), "account");
        assert_eq!(singular("Opportunities"), "opportunity");
        assert_eq!(singular("Corrective actions"), "corrective action");
    }
}
