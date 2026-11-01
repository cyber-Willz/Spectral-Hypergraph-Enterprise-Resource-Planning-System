use stylist::yew::styled_component;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::catalog::IMPORTS;
use crate::components::jsonview::JsonView;
use crate::components::ui::{Alert, Button, Card, PageHeader, Variant};
use crate::{api, browser, util, Session};

#[styled_component(ImportPage)]
pub fn import_page() -> Html {
    let session = use_context::<Session>().expect("session");
    let css = css!("display: grid; gap: 14px; max-width: 640px; label { display: grid; gap: 4px; font-size: 12px; color: var(--muted); font-weight: 550; } select, input[type=file] { background: var(--surface-2); border: 1px solid var(--line-strong); border-radius: 7px; padding: 8px 10px; } .row { display: flex; align-items: center; gap: 8px; } p { margin: 0; color: var(--muted); }");
    let kind = use_state(|| IMPORTS[0].0.to_string());
    let strict = use_state(|| false);
    let file = use_node_ref();
    let busy = use_state(|| false);
    let result = use_state(|| Option::<Result<serde_json::Value, String>>::None);

    if !util::role_allows(&session.user.role, true, "/api/crm/import") {
        return html! { <><PageHeader title="Data import" /><Alert tone="warn">{ "Your role cannot import data." }</Alert></> };
    }
    let upload = {
        let (kind, strict, file, busy, result) = (kind.clone(), strict.clone(), file.clone(), busy.clone(), result.clone());
        Callback::from(move |_| {
            let Some(input) = file.cast::<web_sys::HtmlInputElement>() else { return };
            let Some(f) = input.files().and_then(|l| l.get(0)) else {
                result.set(Some(Err("Choose a CSV file first.".into())));
                return;
            };
            let Some((_, _, path)) = IMPORTS.iter().find(|(k, _, _)| *k == *kind) else { return };
            let mut url = path.to_string();
            let mut q = Vec::new();
            if *strict { q.push(("strict".to_string(), "true".to_string())); }
            if path.contains("/pm/") { q.push(("filename".into(), f.name())); }
            url = util::with_query(&url, &q);
            busy.set(true);
            result.set(None);
            let (busy, result) = (busy.clone(), result.clone());
            spawn_local(async move {
                let r = match browser::read_file_text(&f).await {
                    Ok(text) => api::post_csv(&url, text).await.map_err(|e| e.message),
                    Err(e) => Err(e),
                };
                result.set(Some(r));
                busy.set(false);
            });
        })
    };
    let on_kind = { let kind = kind.clone(); Callback::from(move |e: Event| { if let Some(s) = e.target_dyn_into::<web_sys::HtmlSelectElement>() { kind.set(s.value()); } }) };
    let on_strict = { let strict = strict.clone(); Callback::from(move |e: Event| { if let Some(i) = e.target_dyn_into::<web_sys::HtmlInputElement>() { strict.set(i.checked()); } }) };
    html! {
        <>
            <PageHeader title="Data import" subtitle="Load CSV datasets. Imports are idempotent: re-running updates rows instead of duplicating them." />
            <Card title="Upload a CSV file">
                <div class={css}>
                    <label>{ "Dataset" }<select onchange={on_kind}>{ for IMPORTS.iter().map(|(k, label, _)| html! { <option value={*k} selected={*kind == *k}>{ *label }</option> }) }</select></label>
                    <label>{ "CSV file" }<input type="file" accept=".csv,text/csv" ref={file} /></label>
                    <label class="row"><input type="checkbox" checked={*strict} onchange={on_strict} />{ "Strict: abort if any row is rejected" }</label>
                    <p>{ "Rejected rows are never dropped silently: they are quarantined and listed in the import report." }</p>
                    <div><Button variant={Variant::Primary} icon="upload" busy={*busy} onclick={upload}>{ "Import" }</Button></div>
                </div>
            </Card>
            { match &*result {
                Some(Ok(v)) => html! { <Card title="Import report"><JsonView value={v.clone()} /></Card> },
                Some(Err(e)) => html! { <Alert tone="err">{ e.clone() }</Alert> },
                None => html! {},
            } }
        </>
    }
}
