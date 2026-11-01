//! Schema-driven forms: one input per column, FK pickers, and the small parameter forms for row actions.
use serde_json::{json, Value};
use stylist::yew::styled_component;
use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

use crate::catalog::{ActField, FieldKind};
use crate::components::data::{use_fetch, Fetch};
use crate::components::ui::{Alert, Button, Variant};
use crate::schema::{self, ColDef, ResDef};
use crate::util;

#[derive(Clone, PartialEq)]
pub enum Input {
    Text,
    Secret,
    Area,
    Number(&'static str),
    Date,
    Check,
    Json,
    Select(Vec<String>),
    /// Foreign key: the referenced table name.
    Fk(&'static str),
}

fn is_long_text(name: &str) -> bool {
    ["description", "notes", "content_text", "comments", "summary", "details", "reason", "root_cause", "remediation", "body", "address", "objective", "scope"].iter().any(|w| name.contains(w))
}

fn input_for(c: &ColDef) -> Input {
    if !c.fk.is_empty() {
        return Input::Fk(c.fk);
    }
    if !c.choices.is_empty() {
        return Input::Select(c.choices.iter().map(|s| s.to_string()).collect());
    }
    match c.ty {
        "i" | "bi" => Input::Number("1"),
        "d" | "f" => Input::Number("any"),
        "b" => Input::Check,
        "dt" | "ts" => Input::Date,
        "j" => Input::Json,
        _ if is_long_text(c.name) => Input::Area,
        _ => Input::Text,
    }
}

#[derive(Properties, PartialEq)]
pub struct FieldProps {
    pub name: AttrValue,
    pub label: AttrValue,
    pub input: Input,
    pub value: String,
    #[prop_or_default]
    pub req: bool,
    pub on_change: Callback<String>,
}

#[styled_component(FieldInput)]
pub fn field_input(p: &FieldProps) -> Html {
    let wrap = css!(
        r#"
        display: flex; flex-direction: column; gap: 4px; min-width: 0;
        > span { font-size: 12px; color: var(--muted); font-weight: 550; }
        > span.req::after { content: " *"; color: var(--err); }
        input:not([type=checkbox]), select, textarea { width: 100%; background: var(--surface-2); border: 1px solid var(--line-strong); border-radius: 7px; padding: 7px 10px; }
        input:focus, select:focus, textarea:focus { border-color: var(--brand); outline: none; box-shadow: 0 0 0 3px var(--brand-soft); }
        textarea { min-height: 76px; resize: vertical; }
        .chk { display: flex; align-items: center; gap: 8px; min-height: 34px; }
        "#
    );
    let wide = css!("grid-column: 1 / -1;");
    let oc = p.on_change.clone();
    let on_input = Callback::from(move |e: InputEvent| {
        if let Some(i) = e.target_dyn_into::<HtmlInputElement>() { oc.emit(i.value()); }
    });
    let oc = p.on_change.clone();
    let on_area = Callback::from(move |e: InputEvent| {
        if let Some(i) = e.target_dyn_into::<HtmlTextAreaElement>() { oc.emit(i.value()); }
    });
    let oc = p.on_change.clone();
    let on_select = Callback::from(move |e: Event| {
        if let Some(i) = e.target_dyn_into::<HtmlSelectElement>() { oc.emit(i.value()); }
    });
    let oc = p.on_change.clone();
    let on_check = Callback::from(move |e: Event| {
        if let Some(i) = e.target_dyn_into::<HtmlInputElement>() { oc.emit(i.checked().to_string()); }
    });
    let name = p.name.clone();
    let control = match &p.input {
        Input::Text => html! { <input type="text" {name} value={p.value.clone()} oninput={on_input} /> },
        Input::Secret => html! { <input type="password" autocomplete="new-password" {name} value={p.value.clone()} oninput={on_input} /> },
        Input::Number(step) => html! { <input type="number" step={*step} {name} value={p.value.clone()} oninput={on_input} /> },
        Input::Date => html! { <input type="date" {name} value={p.value.chars().take(10).collect::<String>()} oninput={on_input} /> },
        Input::Check => html! { <label class="chk"><input type="checkbox" {name} checked={matches!(p.value.as_str(), "true" | "yes" | "on" | "1")} onchange={on_check} />{ "Yes" }</label> },
        Input::Area => html! { <textarea {name} value={p.value.clone()} oninput={on_area} /> },
        Input::Json => html! { <textarea {name} placeholder="{ }" style="font-family:var(--mono);font-size:12px" value={p.value.clone()} oninput={on_area} /> },
        Input::Select(opts) => html! {
            <select {name} onchange={on_select}>
                <option value="" selected={p.value.is_empty()}>{ "—" }</option>
                { for opts.iter().map(|o| html! { <option value={o.clone()} selected={*o == p.value}>{ util::title_case(o) }</option> }) }
            </select>
        },
        Input::Fk(table) => html! { <FkSelect table={*table} value={p.value.clone()} on_change={p.on_change.clone()} /> },
    };
    let is_wide = matches!(p.input, Input::Area | Input::Json);
    html! {
        <label class={classes!(wrap, is_wide.then_some(wide))}>
            <span class={p.req.then_some("req")}>{ p.label.clone() }</span>
            { control }
        </label>
    }
}

#[derive(Properties, PartialEq)]
pub struct FkProps {
    pub table: &'static str,
    pub value: String,
    pub on_change: Callback<String>,
}

/// Dropdown of the records of the referenced table; falls back to a plain id field when the list is not readable.
#[function_component(FkSelect)]
pub fn fk_select(p: &FkProps) -> Html {
    let path = if p.table == "users" { "/api/users".to_string() } else { schema::find_table(p.table).map(|d| format!("{}?per_page=200", d.path())).unwrap_or_default() };
    let fetched = use_fetch(path, 0);
    let oc = p.on_change.clone();
    let on_select = Callback::from(move |e: Event| {
        if let Some(i) = e.target_dyn_into::<HtmlSelectElement>() { oc.emit(i.value()); }
    });
    let oc = p.on_change.clone();
    let on_text = Callback::from(move |e: InputEvent| {
        if let Some(i) = e.target_dyn_into::<HtmlInputElement>() { oc.emit(i.value()); }
    });
    match fetched {
        Fetch::Ready(v) => {
            let (rows, _) = util::rows_of(&v);
            let opts: Vec<(String, String)> = rows.iter().filter_map(|r| Some((util::row_id(r, "id")?, util::row_label(r)))).collect();
            let missing = !p.value.is_empty() && !opts.iter().any(|(id, _)| *id == p.value);
            html! {
                <select onchange={on_select}>
                    <option value="" selected={p.value.is_empty()}>{ "—" }</option>
                    { if missing { html! { <option value={p.value.clone()} selected=true>{ p.value.clone() }</option> } } else { html! {} } }
                    { for opts.iter().map(|(id, l)| html! { <option value={id.clone()} selected={*id == p.value}>{ l.clone() }</option> }) }
                </select>
            }
        }
        Fetch::Loading => html! { <select disabled=true><option>{ "Loading…" }</option></select> },
        Fetch::Failed(_) => html! { <input type="text" placeholder="record id" value={p.value.clone()} oninput={on_text} /> },
    }
}

fn set_value(values: &UseStateHandle<Vec<(String, String)>>, key: &str, v: String) {
    let mut next = (**values).clone();
    match next.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = v,
        None => next.push((key.to_string(), v)),
    }
    values.set(next);
}

#[derive(Properties, PartialEq)]
pub struct DynFormProps {
    pub def: &'static ResDef,
    #[prop_or_default]
    pub initial: Option<Value>,
    pub on_submit: Callback<Value>,
    pub on_cancel: Callback<()>,
    #[prop_or_default]
    pub busy: bool,
    #[prop_or_default]
    pub error: Option<AttrValue>,
}

/// Create / edit form for a generated CRUD resource.
#[styled_component(DynamicForm)]
pub fn dynamic_form(p: &DynFormProps) -> Html {
    let grid = css!("display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 12px 16px; margin-bottom: 16px;");
    let footer = css!("display: flex; gap: 8px; justify-content: flex-end;");
    let editing = p.initial.is_some();
    let values = use_state(|| schema::initial_values(p.def, p.initial.as_ref()));
    let local_error = use_state(|| Option::<String>::None);
    let onsubmit = {
        let values = values.clone();
        let local_error = local_error.clone();
        let def = p.def;
        let cb = p.on_submit.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            match schema::build_body(def, &values, editing) {
                Ok(body) => { local_error.set(None); cb.emit(body); }
                Err(m) => local_error.set(Some(m)),
            }
        })
    };
    let cancel = { let c = p.on_cancel.clone(); Callback::from(move |_| c.emit(())) };
    html! {
        <form {onsubmit}>
            <div class={grid}>
                { for p.def.editable().map(|c| {
                    let key = c.name;
                    let value = values.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
                    let values = values.clone();
                    html! { <FieldInput name={key} label={util::title_case(key)} input={input_for(c)} {value} req={c.req}
                        on_change={Callback::from(move |v: String| set_value(&values, key, v))} /> }
                }) }
            </div>
            { if let Some(m) = (*local_error).clone().or_else(|| p.error.as_ref().map(|e| e.to_string())) { html! { <Alert tone="err">{ m }</Alert> } } else { html! {} } }
            <div class={footer}>
                <Button onclick={cancel}>{ "Cancel" }</Button>
                <Button variant={Variant::Primary} submit=true busy={p.busy}>{ if editing { "Save changes" } else { "Create" } }</Button>
            </div>
        </form>
    }
}

// ------------------------------------------------------------------ action + JSON forms

fn act_input(k: FieldKind) -> Input {
    match k {
        FieldKind::Text => Input::Text,
        FieldKind::Secret => Input::Secret,
        FieldKind::Area => Input::Area,
        FieldKind::Number => Input::Number("any"),
        FieldKind::Int => Input::Number("1"),
        FieldKind::Date => Input::Date,
        FieldKind::Bool => Input::Check,
        FieldKind::Select(o) => Input::Select(o.iter().map(|s| s.to_string()).collect()),
    }
}

/// Build the JSON body for a row action from its field values.
pub fn action_body(fields: &[ActField], values: &[(String, String)]) -> Result<Value, String> {
    let mut m = serde_json::Map::new();
    for f in fields {
        let raw = values.iter().find(|(k, _)| k == f.key).map(|(_, v)| v.as_str()).unwrap_or("");
        let ty = match f.kind {
            FieldKind::Number => "f",
            FieldKind::Int => "i",
            FieldKind::Bool => "b",
            _ => "s",
        };
        match schema::coerce(ty, raw).map_err(|e| format!("{}: {e}", f.label))? {
            Some(v) => { m.insert(f.key.into(), v); }
            None if f.kind == FieldKind::Bool => { m.insert(f.key.into(), json!(false)); }
            None if f.req => return Err(format!("{} is required", f.label)),
            None => {}
        }
    }
    Ok(Value::Object(m))
}

#[derive(Properties, PartialEq)]
pub struct ActionFormProps {
    pub fields: &'static [ActField],
    pub submit_label: AttrValue,
    pub on_submit: Callback<Value>,
    pub on_cancel: Callback<()>,
    #[prop_or_default]
    pub busy: bool,
    #[prop_or_default]
    pub error: Option<AttrValue>,
    #[prop_or_default]
    pub danger: bool,
}

#[styled_component(ActionForm)]
pub fn action_form(p: &ActionFormProps) -> Html {
    let grid = css!("display: grid; grid-template-columns: 1fr; gap: 12px; margin-bottom: 16px;");
    let footer = css!("display: flex; gap: 8px; justify-content: flex-end;");
    let values = use_state(Vec::<(String, String)>::new);
    let local_error = use_state(|| Option::<String>::None);
    let onsubmit = {
        let values = values.clone();
        let local_error = local_error.clone();
        let fields = p.fields;
        let cb = p.on_submit.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            match action_body(fields, &values) {
                Ok(b) => { local_error.set(None); cb.emit(b); }
                Err(m) => local_error.set(Some(m)),
            }
        })
    };
    let cancel = { let c = p.on_cancel.clone(); Callback::from(move |_| c.emit(())) };
    html! {
        <form {onsubmit}>
            <div class={grid}>
                { for p.fields.iter().map(|f| {
                    let key = f.key;
                    let value = values.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
                    let values = values.clone();
                    html! { <FieldInput name={key} label={f.label} input={act_input(f.kind)} {value} req={f.req}
                        on_change={Callback::from(move |v: String| set_value(&values, key, v))} /> }
                }) }
            </div>
            { if let Some(m) = (*local_error).clone().or_else(|| p.error.as_ref().map(|e| e.to_string())) { html! { <Alert tone="err">{ m }</Alert> } } else { html! {} } }
            <div class={footer}>
                <Button onclick={cancel}>{ "Cancel" }</Button>
                <Button variant={if p.danger { Variant::Danger } else { Variant::Primary }} submit=true busy={p.busy}>{ p.submit_label.clone() }</Button>
            </div>
        </form>
    }
}

#[derive(Properties, PartialEq)]
pub struct JsonFormProps {
    pub on_submit: Callback<Value>,
    pub on_cancel: Callback<()>,
    #[prop_or_default]
    pub busy: bool,
    #[prop_or_default]
    pub error: Option<AttrValue>,
    #[prop_or_default]
    pub template: AttrValue,
}

/// Free-form JSON body, for hand-written endpoints whose payloads are not described by the schema.
#[styled_component(JsonForm)]
pub fn json_form(p: &JsonFormProps) -> Html {
    let footer = css!("display: flex; gap: 8px; justify-content: flex-end; margin-top: 12px;");
    let text = use_state(|| if p.template.is_empty() { "{\n  \n}".to_string() } else { p.template.to_string() });
    let local_error = use_state(|| Option::<String>::None);
    let onsubmit = {
        let text = text.clone();
        let local_error = local_error.clone();
        let cb = p.on_submit.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            match serde_json::from_str::<Value>(&text) {
                Ok(v) if v.is_object() => { local_error.set(None); cb.emit(v); }
                Ok(_) => local_error.set(Some("The body must be a JSON object.".into())),
                Err(e) => local_error.set(Some(format!("Invalid JSON: {e}"))),
            }
        })
    };
    let cancel = { let c = p.on_cancel.clone(); Callback::from(move |_| c.emit(())) };
    let set = { let text = text.clone(); Callback::from(move |v: String| text.set(v)) };
    html! {
        <form {onsubmit}>
            <FieldInput name="body" label="JSON body" input={Input::Json} value={(*text).clone()} on_change={set} />
            { if let Some(m) = (*local_error).clone().or_else(|| p.error.as_ref().map(|e| e.to_string())) { html! { <Alert tone="err">{ m }</Alert> } } else { html! {} } }
            <div class={footer}>
                <Button onclick={cancel}>{ "Cancel" }</Button>
                <Button variant={Variant::Primary} submit=true busy={p.busy}>{ "Create" }</Button>
            </div>
        </form>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ACTIONS;

    #[test]
    fn action_bodies() {
        let stage = ACTIONS.iter().find(|a| a.suffix == "/stage").unwrap();
        let ok = action_body(stage.fields, &[("stage".into(), "closed_won".into())]).unwrap();
        assert_eq!(ok["stage"], "closed_won");
        assert!(action_body(stage.fields, &[]).unwrap_err().contains("required"));
        let pay = ACTIONS.iter().find(|a| a.label == "Pay").unwrap();
        let b = action_body(pay.fields, &[("bank_account_id".into(), "3".into()), ("amount".into(), "10.5".into())]).unwrap();
        assert_eq!(b["bank_account_id"], 3);
        assert_eq!(b["amount"], 10.5);
        assert!(action_body(pay.fields, &[("bank_account_id".into(), "x".into())]).is_err());
        let decide = ACTIONS.iter().find(|a| a.suffix == "/decide").unwrap();
        assert_eq!(action_body(decide.fields, &[]).unwrap()["approve"], false);
    }

    #[test]
    fn input_kinds_follow_schema() {
        let def = schema::find("crm", "opportunities").unwrap();
        assert!(matches!(input_for(def.col("stage").unwrap()), Input::Select(_)));
        assert!(matches!(input_for(def.col("amount").unwrap()), Input::Number("any")));
        assert!(matches!(input_for(def.col("account_id").unwrap()), Input::Fk("crm_accounts")));
    }
}
