//! Application shell: role-aware sidebar, top bar with theme switch and user menu.
use stylist::yew::styled_component;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::catalog::{self, MODULES};
use crate::components::ui::{Button, Icon, Variant};
use crate::schema;
use crate::theme::{Mode, ThemeCtx};
use crate::{Route, Session};

pub struct NavItem {
    pub label: String,
    pub route: Route,
}
pub struct NavGroup {
    pub key: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub items: Vec<NavItem>,
}

/// Sidebar content for a role: only modules the backend would let that role read.
pub fn nav_groups(role: &str) -> Vec<NavGroup> {
    let mut groups = Vec::new();
    for (module, icon) in MODULES {
        if !crate::util::role_allows(role, false, &format!("/api/{module}")) {
            continue;
        }
        let mut items = vec![NavItem { label: "Overview".into(), route: Route::Module { module: module.to_string() } }];
        for r in schema::in_module(module) {
            items.push(NavItem { label: r.title(), route: Route::Resource { module: module.to_string(), slug: r.slug.to_string() } });
        }
        for f in catalog::feeds_in(module) {
            items.push(NavItem { label: f.title.to_string(), route: Route::Feed { section: module.to_string(), slug: f.slug.to_string() } });
        }
        if catalog::reports_in(module).next().is_some() {
            items.push(NavItem { label: "Insights".into(), route: Route::Insights { module: module.to_string() } });
        }
        groups.push(NavGroup { key: module, title: schema::module_title(module), icon, items });
    }
    let mut system = Vec::new();
    if crate::util::role_allows(role, true, "/api/crm/import") {
        system.push(NavItem { label: "Data import".into(), route: Route::Import });
    }
    if crate::util::role_allows(role, false, "/api/sys/audit-log") {
        system.push(NavItem { label: "Audit log".into(), route: Route::Resource { module: "sys".into(), slug: "audit-log".into() } });
    }
    if role == "admin" {
        system.push(NavItem { label: "Users".into(), route: Route::Users });
    }
    if !system.is_empty() {
        groups.push(NavGroup { key: "system", title: "Administration", icon: "sliders", items: system });
    }
    groups
}

#[derive(Properties, PartialEq)]
pub struct ShellProps {
    pub children: Html,
}

#[styled_component(Shell)]
pub fn shell(p: &ShellProps) -> Html {
    let session = use_context::<Session>().expect("session");
    let theme = use_context::<ThemeCtx>().expect("theme");
    let route = use_route::<Route>().unwrap_or(Route::Dashboard);
    let nav_open = use_state(|| false);
    let query = use_state(String::new);
    let open_group = use_state(|| route.group());
    {
        // follow navigation: open the group of the current page
        let open_group = open_group.clone();
        use_effect_with(route.clone(), move |r| {
            if let Some(g) = r.group() { open_group.set(Some(g)); }
            || ()
        });
    }
    {
        let nav_open = nav_open.clone();
        use_effect_with(route.clone(), move |_| { nav_open.set(false); || () });
    }

    let css = css!(
        r#"
        display: grid; grid-template-columns: 252px minmax(0, 1fr); min-height: 100vh;
        aside { background: var(--side-bg); color: var(--side-ink); display: flex; flex-direction: column; position: sticky; top: 0; height: 100vh; border-right: 1px solid var(--side-line); }
        .brand { display: flex; gap: 10px; align-items: center; padding: 16px 16px 12px; color: #fff; font-weight: 700; font-size: 15px; letter-spacing: -.01em; }
        .logo { width: 28px; height: 28px; border-radius: 8px; background: var(--brand); color: var(--brand-ink); display: grid; place-items: center; }
        .brand small { display: block; font-weight: 500; font-size: 11px; color: var(--side-muted); }
        .find { margin: 0 12px 8px; }
        .find input { width: 100%; background: rgba(255,255,255,.06); border: 1px solid var(--side-line); color: #fff; border-radius: 7px; padding: 6px 10px; font-size: 13px; }
        .find input::placeholder { color: var(--side-muted); }
        nav { flex: 1; overflow-y: auto; padding: 4px 8px 16px; }
        .grp > button { width: 100%; display: flex; align-items: center; gap: 10px; background: none; border: 0; color: var(--side-ink); padding: 8px 10px; border-radius: 7px; cursor: pointer; font-weight: 600; text-align: left; }
        .grp > button:hover { background: var(--side-active); }
        .grp > button .chev { margin-left: auto; transition: transform .15s; opacity: .6; }
        .grp.open > button .chev { transform: rotate(180deg); }
        .grp .items { display: none; padding: 2px 0 6px 18px; margin-left: 10px; border-left: 1px solid var(--side-line); }
        .grp.open .items { display: block; }
        .items a { display: block; color: var(--side-ink); padding: 5px 10px; border-radius: 6px; font-size: 13px; text-decoration: none; opacity: .85; }
        .items a:hover { background: var(--side-active); opacity: 1; }
        .items a.on { background: var(--brand); color: var(--brand-ink); opacity: 1; font-weight: 600; }
        .home { display: flex; gap: 10px; align-items: center; padding: 8px 10px; border-radius: 7px; color: var(--side-ink); font-weight: 600; text-decoration: none; margin-bottom: 4px; }
        .home:hover { background: var(--side-active); } .home.on { background: var(--brand); color: var(--brand-ink); }
        .col { min-width: 0; display: flex; flex-direction: column; }
        header.top { position: sticky; top: 0; z-index: 10; display: flex; align-items: center; gap: 12px; padding: 10px 24px; background: color-mix(in srgb, var(--bg) 88%, transparent); backdrop-filter: blur(8px); border-bottom: 1px solid var(--line); }
        header.top .crumb { font-weight: 600; color: var(--ink-2); }
        header.top .sp { flex: 1; }
        .burger { display: none; }
        .who { text-align: right; line-height: 1.25; font-size: 12.5px; } .who b { display: block; font-size: 13px; } .who span { color: var(--muted); text-transform: capitalize; }
        main { padding: 24px; max-width: 1400px; width: 100%; }
        @media (max-width: 900px) {
          grid-template-columns: minmax(0, 1fr);
          aside { position: fixed; left: 0; top: 0; z-index: 40; width: 280px; transform: translateX(-100%); transition: transform .2s; }
          aside.open { transform: none; box-shadow: 0 0 0 100vmax rgba(0,0,0,.45); }
          .burger { display: inline-flex; } header.top { padding: 10px 14px; } main { padding: 16px 14px; } .who { display: none; }
        }
        "#
    );

    let groups = nav_groups(&session.user.role);
    let q = (*query).to_lowercase();
    let searching = !q.is_empty();
    let theme_icon = match theme.mode { Mode::System => "monitor", Mode::Light => "sun", Mode::Dark => "moon" };
    let theme_label = format!("Theme: {} (click to change)", theme.mode.as_str());
    let logout = { let l = session.logout.clone(); Callback::from(move |_| l.emit(())) };
    let toggle_nav = { let n = nav_open.clone(); Callback::from(move |_| n.set(!*n)) };
    let cycle = { let c = theme.cycle.clone(); Callback::from(move |_| c.emit(())) };
    let on_query = { let query = query.clone(); Callback::from(move |e: InputEvent| { if let Some(i) = e.target_dyn_into::<web_sys::HtmlInputElement>() { query.set(i.value()); } }) };

    html! {
        <div class={css}>
            <aside class={nav_open.then_some("open")} aria-label="Primary">
                <div class="brand"><span class="logo"><Icon name="chart" size={16} /></span><span>{ "Enterprise Suite" }<small>{ "Operations · Finance · People" }</small></span></div>
                <div class="find"><input type="search" placeholder="Find a page…" value={(*query).clone()} oninput={on_query} aria-label="Find a page" /></div>
                <nav>
                    <Link<Route> to={Route::Dashboard} classes={classes!("home", (route == Route::Dashboard).then_some("on"))}><Icon name="home" />{ "Dashboard" }</Link<Route>>
                    { for groups.iter().filter_map(|g| {
                        let items: Vec<&NavItem> = g.items.iter().filter(|i| !searching || i.label.to_lowercase().contains(&q) || g.title.to_lowercase().contains(&q)).collect();
                        if items.is_empty() { return None; }
                        let is_open = searching || open_group.as_deref() == Some(g.key);
                        let key = g.key;
                        let og = open_group.clone();
                        let toggle = Callback::from(move |_| og.set(if og.as_deref() == Some(key) { None } else { Some(key.to_string()) }));
                        Some(html! {
                            <div class={classes!("grp", is_open.then_some("open"))}>
                                <button type="button" onclick={toggle} aria-expanded={is_open.to_string()}><Icon name={g.icon} />{ g.title }<span class="chev"><Icon name="chevron" size={14} /></span></button>
                                <div class="items">
                                    { for items.iter().map(|i| html! { <Link<Route> to={i.route.clone()} classes={classes!((route == i.route).then_some("on"))}>{ i.label.clone() }</Link<Route>> }) }
                                </div>
                            </div>
                        })
                    }) }
                </nav>
            </aside>
            <div class="col">
                <header class="top">
                    <span class="burger"><Button variant={Variant::Ghost} icon="menu" title="Menu" onclick={toggle_nav} /></span>
                    <span class="crumb">{ route.crumb() }</span>
                    <span class="sp" />
                    <Button variant={Variant::Ghost} icon={theme_icon} title={theme_label} onclick={cycle} />
                    <div class="who"><b>{ session.user.display_name() }</b><span>{ session.user.role.clone() }</span></div>
                    <Button variant={Variant::Ghost} icon="logout" title="Sign out" onclick={logout} />
                </header>
                <main>{ p.children.clone() }</main>
            </div>
        </div>
    }
}
