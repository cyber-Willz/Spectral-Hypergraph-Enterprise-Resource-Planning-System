//! Enterprise Suite web frontend: Yew (WebAssembly) + Stylist.
mod api;
mod browser;
mod catalog;
mod components;
mod pages;
mod schema;
mod schema_generated;
mod theme;
mod util;

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew_router::prelude::*;

use api::User;
use components::layout::Shell;
use components::ui::Spinner;

#[derive(Clone, Routable, PartialEq, Debug)]
pub enum Route {
    #[at("/")]
    Dashboard,
    /// Module landing page: its resources, lists and reports.
    #[at("/m/:module")]
    Module { module: String },
    /// Generated CRUD resource (`/api/<module>/<slug>`).
    #[at("/r/:module/:slug")]
    Resource { module: String, slug: String },
    /// Hand-written list endpoint (finance, HR, risk, supply chain).
    #[at("/f/:section/:slug")]
    Feed { section: String, slug: String },
    #[at("/i/:module")]
    Insights { module: String },
    #[at("/import")]
    Import,
    #[at("/admin/users")]
    Users,
    #[not_found]
    #[at("/404")]
    NotFound,
}

impl Route {
    /// Sidebar group this page belongs to.
    pub fn group(&self) -> Option<String> {
        match self {
            Route::Module { module } | Route::Insights { module } => Some(module.clone()),
            Route::Resource { module, .. } if module == "sys" => Some("system".into()),
            Route::Resource { module, .. } => Some(module.clone()),
            Route::Feed { section, .. } => Some(section.clone()),
            Route::Import | Route::Users => Some("system".into()),
            _ => None,
        }
    }
    pub fn crumb(&self) -> String {
        match self {
            Route::Dashboard => "Dashboard".into(),
            Route::Module { module } => schema::module_title(module).into(),
            Route::Insights { module } => format!("{} / Insights", schema::module_title(module)),
            Route::Resource { module, slug } => format!("{} / {}", schema::module_title(module), util::title_case(slug)),
            Route::Feed { section, slug } => format!("{} / {}", schema::module_title(section), catalog::find_feed(section, slug).map(|f| f.title.to_string()).unwrap_or_else(|| util::title_case(slug))),
            Route::Import => "Administration / Data import".into(),
            Route::Users => "Administration / Users".into(),
            Route::NotFound => "Not found".into(),
        }
    }
}

/// Signed-in user + logout, available to every page through a context.
#[derive(Clone, PartialEq)]
pub struct Session {
    pub user: User,
    pub logout: Callback<()>,
}

fn switch(route: Route) -> Html {
    match route {
        Route::Dashboard => html! { <pages::dashboard::DashboardPage /> },
        Route::Module { module } => html! { <pages::module::ModulePage key={module.clone()} module={module.clone()} /> },
        Route::Resource { module, slug } => html! { <pages::resource::ResourcePage key={format!("{module}/{slug}")} module={module.clone()} slug={slug.clone()} /> },
        Route::Feed { section, slug } => html! { <pages::resource::FeedPage key={format!("{section}/{slug}")} section={section.clone()} slug={slug.clone()} /> },
        Route::Insights { module } => html! { <pages::insights::InsightsPage key={module.clone()} module={module.clone()} /> },
        Route::Import => html! { <pages::import::ImportPage /> },
        Route::Users => html! { <pages::users::UsersPage /> },
        Route::NotFound => html! { <pages::not_found::NotFoundPage /> },
    }
}

#[function_component(Root)]
fn root() -> Html {
    let user = use_state(api::stored_user);
    // a stored token may have expired: confirm it once on load
    let checked = use_state(|| api::stored_user().is_none());
    {
        let (user, checked) = (user.clone(), checked.clone());
        use_effect_with((), move |_| {
            if !*checked {
                spawn_local(async move {
                    match api::whoami().await {
                        Ok(u) => user.set(Some(u)),
                        Err(e) if e.status == 401 || e.status == 403 => {
                            api::clear_session();
                            user.set(None);
                        }
                        Err(_) => {} // server unreachable: keep the cached session, pages will report the error
                    }
                    checked.set(true);
                });
            }
            || ()
        });
    }
    let on_login = {
        let user = user.clone();
        Callback::from(move |u: User| user.set(Some(u)))
    };
    let logout = {
        let user = user.clone();
        Callback::from(move |_| {
            api::clear_session();
            user.set(None);
        })
    };
    if !*checked {
        return html! { <Spinner /> };
    }
    match (*user).clone() {
        None => html! { <pages::login::LoginPage {on_login} /> },
        Some(u) => html! {
            <ContextProvider<Session> context={Session { user: u, logout }}>
                <BrowserRouter>
                    <Shell><Switch<Route> render={switch} /></Shell>
                </BrowserRouter>
            </ContextProvider<Session>>
        },
    }
}

#[function_component(App)]
fn app() -> Html {
    html! { <theme::ThemeProvider><Root /></theme::ThemeProvider> }
}

fn main() {
    yew::Renderer::<App>::new().render();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_resource_has_a_route_group_and_crumb() {
        for r in schema::RESOURCES {
            let route = Route::Resource { module: r.module.into(), slug: r.slug.into() };
            assert!(route.group().is_some());
            assert!(route.crumb().contains(&r.title()));
        }
        assert_eq!(Route::Feed { section: "scm".into(), slug: "products".into() }.crumb(), "Supply Chain / Products");
    }

    #[test]
    fn nav_respects_roles() {
        use components::layout::nav_groups;
        let keys = |role: &str| nav_groups(role).iter().map(|g| g.key).collect::<Vec<_>>();
        assert!(keys("admin").contains(&"finance") && keys("admin").contains(&"system"));
        assert!(!keys("user").contains(&"finance") && !keys("user").contains(&"hr"));
        assert!(keys("finance").contains(&"finance") && !keys("finance").contains(&"hr"));
        assert!(keys("hr").contains(&"hr") && !keys("hr").contains(&"risk"));
        // auditors read everything but cannot import
        let aud = nav_groups("auditor");
        let sys = aud.iter().find(|g| g.key == "system").unwrap();
        assert!(sys.items.iter().all(|i| i.route != Route::Import));
    }
}
