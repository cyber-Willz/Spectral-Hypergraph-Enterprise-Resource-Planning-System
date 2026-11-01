use serde_json::{json, Value};
use stylist::yew::styled_component;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::catalog::{ActField, FieldKind};
use crate::components::data::{show, use_fetch};
use crate::components::form::ActionForm;
use crate::components::table::{Column, DataTable, Kind};
use crate::components::ui::{Alert, Button, Card, Modal, PageHeader, Variant};
use crate::{api, util, Session};

const ROLES: &[&str] = &["admin", "manager", "user", "auditor", "finance", "hr"];
const NEW_USER: &[ActField] = &[
    ActField { key: "email", label: "Email", kind: FieldKind::Text, req: true },
    ActField { key: "full_name", label: "Full name", kind: FieldKind::Text, req: false },
    ActField { key: "password", label: "Initial password", kind: FieldKind::Secret, req: true },
    ActField { key: "role", label: "Role", kind: FieldKind::Select(ROLES), req: true },
];
const SET_ROLE: &[ActField] = &[ActField { key: "role", label: "Role", kind: FieldKind::Select(ROLES), req: true }];
const SET_PASSWORD: &[ActField] = &[ActField { key: "password", label: "New password", kind: FieldKind::Secret, req: true }];

#[derive(Clone, PartialEq)]
enum Dialog {
    None,
    New,
    Role(Value),
    Password(Value),
}

#[styled_component(UsersPage)]
pub fn users_page() -> Html {
    let session = use_context::<Session>().expect("session");
    let tick = use_state(|| 0u32);
    let fetched = use_fetch("/api/users".into(), *tick);
    let dialog = use_state(|| Dialog::None);
    let busy = use_state(|| false);
    let error = use_state(|| Option::<String>::None);
    let flash = use_state(|| Option::<String>::None);
    let _ = css!("");
    if session.user.role != "admin" {
        return html! { <><PageHeader title="Users" /><Alert tone="warn">{ "Only administrators can manage users." }</Alert></> };
    }
    let run = {
        let (busy, error, flash, tick, dialog) = (busy.clone(), error.clone(), flash.clone(), tick.clone(), dialog.clone());
        Callback::from(move |(method, url, body, done): (&'static str, String, Value, String)| {
            busy.set(true);
            error.set(None);
            let (busy, error, flash, tick, dialog) = (busy.clone(), error.clone(), flash.clone(), tick.clone(), dialog.clone());
            spawn_local(async move {
                match api::send(method, &url, Some(&body)).await {
                    Ok(_) => { flash.set(Some(done)); dialog.set(Dialog::None); tick.set(*tick + 1); }
                    Err(e) => error.set(Some(e.message)),
                }
                busy.set(false);
            });
        })
    };
    let close = { let (d, e) = (dialog.clone(), error.clone()); Callback::from(move |_| { d.set(Dialog::None); e.set(None); }) };
    let err = (*error).clone().map(AttrValue::from);
    let modal = match &*dialog {
        Dialog::None => html! {},
        Dialog::New => {
            let run = run.clone();
            html! { <Modal title="New user" on_close={close.clone()}><ActionForm fields={NEW_USER} submit_label="Create user" busy={*busy} error={err.clone()} on_cancel={close.clone()}
                on_submit={Callback::from(move |b: Value| run.emit(("POST", "/api/users".into(), b, "User created.".into())))} /></Modal> }
        }
        Dialog::Role(u) => {
            let (run, id) = (run.clone(), util::row_id(u, "id").unwrap_or_default());
            html! { <Modal title={format!("Change role — {}", util::row_label(u))} on_close={close.clone()}><ActionForm fields={SET_ROLE} submit_label="Save role" busy={*busy} error={err.clone()} on_cancel={close.clone()}
                on_submit={Callback::from(move |b: Value| run.emit(("PATCH", format!("/api/users/{id}"), b, "Role updated.".into())))} /></Modal> }
        }
        Dialog::Password(u) => {
            let (run, id) = (run.clone(), util::row_id(u, "id").unwrap_or_default());
            html! { <Modal title={format!("Reset password — {}", util::row_label(u))} on_close={close.clone()}><ActionForm fields={SET_PASSWORD} submit_label="Set password" busy={*busy} error={err.clone()} on_cancel={close.clone()}
                on_submit={Callback::from(move |b: Value| run.emit(("PATCH", format!("/api/users/{id}"), b, "Password changed.".into())))} /></Modal> }
        }
    };
    let columns = vec![
        Column::plain("full_name"),
        Column::plain("email"),
        Column { key: "role".into(), label: "Role".into(), kind: Kind::Badge, sortable: false },
        Column { key: "is_active".into(), label: "Active".into(), kind: Kind::Bool, sortable: false },
        Column { key: "last_login_at".into(), label: "Last sign-in".into(), kind: Kind::Date, sortable: false },
    ];
    let me = session.user.id.clone();
    let row_extra = {
        let (dialog, run) = (dialog.clone(), run.clone());
        Callback::from(move |u: Value| -> Html {
            let id = util::row_id(&u, "id").unwrap_or_default();
            let active = u.get("is_active").and_then(|v| v.as_bool()).unwrap_or(true);
            let is_me = id == me;
            let (d1, d2, u1, u2) = (dialog.clone(), dialog.clone(), u.clone(), u.clone());
            let run = run.clone();
            html! { <>
                <Button small=true onclick={Callback::from(move |_| d1.set(Dialog::Role(u1.clone())))}>{ "Role" }</Button>
                <Button small=true onclick={Callback::from(move |_| d2.set(Dialog::Password(u2.clone())))}>{ "Password" }</Button>
                <Button small=true variant={if active { Variant::Danger } else { Variant::Secondary }} disabled={is_me}
                    onclick={Callback::from(move |_| run.emit(("PATCH", format!("/api/users/{id}"), json!({ "is_active": !active }), if active { "User deactivated." } else { "User activated." }.into())))}>{ if active { "Deactivate" } else { "Activate" } }</Button>
            </> }
        })
    };
    html! {
        <>
            <PageHeader title="Users" subtitle="Accounts and roles. The role decides which modules a person can read or change.">
                <Button variant={Variant::Primary} icon="plus" onclick={{ let d = dialog.clone(); Callback::from(move |_| d.set(Dialog::New)) }}>{ "New user" }</Button>
            </PageHeader>
            { if let Some(m) = &*flash { html! { <Alert tone="ok">{ m.clone() }</Alert> } } else { html! {} } }
            <Card pad=false><div style="padding:14px 16px">
                { show(&fetched, |v| { let (rows, _) = util::rows_of(v); html! { <DataTable columns={columns.clone()} {rows} row_extra={Some(row_extra.clone())} /> } }) }
            </div></Card>
            { modal }
        </>
    }
}
