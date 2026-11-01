use yew::prelude::*;

use crate::catalog;
use crate::components::ui::{Alert, PageHeader};
use crate::pages::list::ListView;
use crate::schema;

#[derive(Properties, PartialEq)]
pub struct ResourceProps {
    pub module: String,
    pub slug: String,
}

/// Generated CRUD resource, described by `schema_generated.rs`.
#[function_component(ResourcePage)]
pub fn resource_page(p: &ResourceProps) -> Html {
    match schema::find(&p.module, &p.slug) {
        Some(def) => html! { <ListView title={def.title()} subtitle={format!("{} · /api/{}/{}", schema::module_title(def.module), def.module, def.slug)} path={def.path()} def={Some(def)} /> },
        None => html! { <><PageHeader title="Unknown resource" /><Alert tone="err">{ format!("There is no resource {}/{}.", p.module, p.slug) }</Alert></> },
    }
}

#[derive(Properties, PartialEq)]
pub struct FeedProps {
    pub section: String,
    pub slug: String,
}

/// Hand-written list endpoint from the catalogue.
#[function_component(FeedPage)]
pub fn feed_page(p: &FeedProps) -> Html {
    match catalog::find_feed(&p.section, &p.slug) {
        Some(f) => html! { <ListView title={f.title} subtitle={f.path} path={f.path} create_json={f.create} /> },
        None => html! { <><PageHeader title="Unknown list" /><Alert tone="err">{ format!("There is no list {}/{}.", p.section, p.slug) }</Alert></> },
    }
}
