//! Design-system primitives, each styled with Stylist.
use stylist::yew::styled_component;
use yew::prelude::*;

// ------------------------------------------------------------------ icons

pub fn icon_path(name: &str) -> &'static str {
    match name {
        "home" => "M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z M9 22V12h6v10",
        "users" => "M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2 M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z M23 21v-2a4 4 0 0 0-3-3.87 M16 3.13a4 4 0 0 1 0 7.75",
        "briefcase" => "M4 7h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2z M16 21V5a2 2 0 0 0-2-2h-4a2 2 0 0 0-2 2v16",
        "shield" => "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z",
        "tool" => "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z",
        "activity" => "M22 12h-4l-3 9L9 3l-3 9H2",
        "check" => "M22 11.08V12a10 10 0 1 1-5.93-9.14 M22 4L12 14.01l-3-3",
        "file" => "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z M14 2v6h6 M16 13H8 M16 17H8 M10 9H8",
        "dollar" => "M12 1v22 M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6",
        "user" => "M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2 M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z",
        "alert" => "M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z M12 9v4 M12 17h.01",
        "truck" => "M1 3h15v13H1z M16 8h4l3 3v5h-7V8z M5.5 21a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5z M18.5 21a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5z",
        "sliders" => "M4 21v-7 M4 10V3 M12 21v-9 M12 8V3 M20 21v-5 M20 12V3 M1 14h6 M9 8h6 M17 16h6",
        "search" => "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16z M21 21l-4.35-4.35",
        "sun" => "M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10z M12 1v2 M12 21v2 M4.22 4.22l1.42 1.42 M18.36 18.36l1.42 1.42 M1 12h2 M21 12h2 M4.22 19.78l1.42-1.42 M18.36 5.64l1.42-1.42",
        "moon" => "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z",
        "monitor" => "M2 3h20v14H2z M8 21h8 M12 17v4",
        "menu" => "M3 12h18 M3 6h18 M3 18h18",
        "chevron" => "M6 9l6 6 6-6",
        "plus" => "M12 5v14 M5 12h14",
        "edit" => "M12 20h9 M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4z",
        "trash" => "M3 6h18 M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6 M10 11v6 M14 11v6 M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2",
        "download" => "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4 M7 10l5 5 5-5 M12 15V3",
        "upload" => "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4 M17 8l-5-5-5 5 M12 3v12",
        "refresh" => "M23 4v6h-6 M1 20v-6h6 M3.51 9a9 9 0 0 1 14.85-3.36L23 10 M1 14l4.64 4.36A9 9 0 0 0 20.49 15",
        "x" => "M18 6L6 18 M6 6l12 12",
        "logout" => "M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4 M16 17l5-5-5-5 M21 12H9",
        "chart" => "M12 20V10 M18 20V4 M6 20v-4",
        "arrow" => "M5 12h14 M12 5l7 7-7 7",
        _ => "M12 12h.01",
    }
}

#[derive(Properties, PartialEq)]
pub struct IconProps {
    pub name: &'static str,
    #[prop_or(18)]
    pub size: u32,
}

#[function_component(Icon)]
pub fn icon(p: &IconProps) -> Html {
    let s = p.size.to_string();
    html! {
        <svg width={s.clone()} height={s} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style="flex:none">
            <path d={icon_path(p.name)} />
        </svg>
    }
}

// ------------------------------------------------------------------ buttons

#[derive(Clone, Copy, PartialEq)]
pub enum Variant {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

#[derive(Properties, PartialEq)]
pub struct ButtonProps {
    #[prop_or(Variant::Secondary)]
    pub variant: Variant,
    #[prop_or_default]
    pub small: bool,
    #[prop_or_default]
    pub disabled: bool,
    #[prop_or_default]
    pub busy: bool,
    #[prop_or_default]
    pub submit: bool,
    #[prop_or_default]
    pub icon: Option<&'static str>,
    #[prop_or_default]
    pub title: AttrValue,
    #[prop_or_default]
    pub onclick: Callback<MouseEvent>,
    #[prop_or_default]
    pub children: Html,
}

#[styled_component(Button)]
pub fn button(p: &ButtonProps) -> Html {
    let base = css!(
        r#"
        display: inline-flex; align-items: center; justify-content: center; gap: 6px;
        border-radius: 8px; border: 1px solid var(--line-strong); background: var(--surface); color: var(--ink);
        padding: 7px 14px; font-weight: 550; cursor: pointer; white-space: nowrap; line-height: 1.2;
        transition: background .12s, border-color .12s, transform .05s;
        &:hover:not(:disabled) { border-color: var(--brand); }
        &:active:not(:disabled) { transform: translateY(1px); }
        &:disabled { opacity: .5; cursor: not-allowed; }
        "#
    );
    let variant = match p.variant {
        Variant::Primary => css!("background: var(--brand); border-color: var(--brand); color: var(--brand-ink); &:hover:not(:disabled) { background: var(--brand-hover); border-color: var(--brand-hover); }"),
        Variant::Secondary => css!(""),
        Variant::Ghost => css!("background: transparent; border-color: transparent; color: var(--ink-2); &:hover:not(:disabled) { background: var(--surface-2); border-color: var(--line); }"),
        Variant::Danger => css!("color: var(--err); border-color: var(--err); background: transparent; &:hover:not(:disabled) { background: var(--err-soft); border-color: var(--err); }"),
    };
    let small = css!("padding: 3px 10px; font-size: 12.5px; border-radius: 6px;");
    html! {
        <button type={if p.submit { "submit" } else { "button" }} class={classes!(base, variant, p.small.then_some(small))}
            disabled={p.disabled || p.busy} title={p.title.clone()} onclick={p.onclick.clone()}>
            { if let Some(i) = p.icon { html! { <Icon name={i} size={if p.small { 14 } else { 16 }} /> } } else { html! {} } }
            { if p.busy { html! { <Spinner small=true /> } } else { p.children.clone() } }
        </button>
    }
}

// ------------------------------------------------------------------ feedback

#[derive(Properties, PartialEq)]
pub struct SpinnerProps {
    #[prop_or_default]
    pub small: bool,
}

#[styled_component(Spinner)]
pub fn spinner(p: &SpinnerProps) -> Html {
    let ring = css!(
        r#"
        @keyframes spin { to { transform: rotate(360deg); } }
        display: inline-block; width: 18px; height: 18px; border-radius: 50%;
        border: 2px solid var(--line-strong); border-top-color: var(--brand); animation: spin .7s linear infinite;
        "#
    );
    let big = css!("margin: 28px auto; display: block; width: 26px; height: 26px;");
    html! { <span class={classes!(ring, (!p.small).then_some(big))} role="status" aria-label="Loading" /> }
}

#[derive(Properties, PartialEq)]
pub struct AlertProps {
    #[prop_or("info")]
    pub tone: &'static str,
    pub children: Html,
}

#[styled_component(Alert)]
pub fn alert(p: &AlertProps) -> Html {
    let tone = match p.tone {
        "err" => css!("background: var(--err-soft); color: var(--err); border-color: var(--err);"),
        "warn" => css!("background: var(--warn-soft); color: var(--warn); border-color: var(--warn);"),
        "ok" => css!("background: var(--ok-soft); color: var(--ok); border-color: var(--ok);"),
        _ => css!("background: var(--info-soft); color: var(--info); border-color: var(--info);"),
    };
    let base = css!("padding: 10px 14px; border-radius: 8px; border: 1px solid; margin: 8px 0; font-size: 13px; word-break: break-word;");
    html! { <div class={classes!(base, tone)} role="alert">{ p.children.clone() }</div> }
}

#[derive(Properties, PartialEq)]
pub struct BadgeProps {
    pub text: AttrValue,
    #[prop_or_default]
    pub tone: Option<&'static str>,
}

#[styled_component(Badge)]
pub fn badge(p: &BadgeProps) -> Html {
    let tone = p.tone.unwrap_or_else(|| crate::util::tone_for(&p.text));
    let base = css!("display: inline-block; padding: 1px 9px; border-radius: 99px; font-size: 11.5px; font-weight: 600; border: 1px solid var(--line-strong); color: var(--ink-2); background: var(--surface-2); white-space: nowrap;");
    let t = match tone {
        "ok" => css!("color: var(--ok); background: var(--ok-soft); border-color: transparent;"),
        "warn" => css!("color: var(--warn); background: var(--warn-soft); border-color: transparent;"),
        "err" => css!("color: var(--err); background: var(--err-soft); border-color: transparent;"),
        "info" => css!("color: var(--info); background: var(--info-soft); border-color: transparent;"),
        _ => css!(""),
    };
    html! { <span class={classes!(base, t)}>{ crate::util::title_case(&p.text) }</span> }
}

// ------------------------------------------------------------------ layout blocks

#[derive(Properties, PartialEq)]
pub struct PageHeaderProps {
    pub title: AttrValue,
    #[prop_or_default]
    pub subtitle: AttrValue,
    #[prop_or_default]
    pub children: Html,
}

#[styled_component(PageHeader)]
pub fn page_header(p: &PageHeaderProps) -> Html {
    let wrap = css!("display: flex; align-items: flex-end; justify-content: space-between; gap: 16px; flex-wrap: wrap; margin-bottom: 18px; h1 { font-size: 22px; } p { margin: 3px 0 0; color: var(--muted); } .actions { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }");
    html! {
        <div class={wrap}>
            <div><h1>{ p.title.clone() }</h1>{ if p.subtitle.is_empty() { html!{} } else { html!{ <p>{ p.subtitle.clone() }</p> } } }</div>
            <div class="actions">{ p.children.clone() }</div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct CardProps {
    #[prop_or_default]
    pub title: AttrValue,
    #[prop_or_default]
    pub actions: Html,
    #[prop_or(true)]
    pub pad: bool,
    pub children: Html,
}

#[styled_component(Card)]
pub fn card(p: &CardProps) -> Html {
    let wrap = css!("background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); box-shadow: var(--shadow); margin-bottom: 16px; overflow: hidden; > header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 16px; border-bottom: 1px solid var(--line); } > header h3 { font-size: 14.5px; }");
    let body = css!("padding: 16px;");
    html! {
        <section class={wrap}>
            { if p.title.is_empty() { html!{} } else { html! { <header><h3>{ p.title.clone() }</h3><div>{ p.actions.clone() }</div></header> } } }
            <div class={p.pad.then_some(body)}>{ p.children.clone() }</div>
        </section>
    }
}

#[derive(Properties, PartialEq)]
pub struct StatProps {
    pub label: AttrValue,
    pub value: AttrValue,
    #[prop_or_default]
    pub tone: &'static str,
    #[prop_or_default]
    pub hint: AttrValue,
}

#[styled_component(StatTile)]
pub fn stat_tile(p: &StatProps) -> Html {
    let wrap = css!("background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; min-width: 0; .l { color: var(--muted); font-size: 12px; } .v { font-size: 20px; font-weight: 650; font-variant-numeric: tabular-nums; overflow-wrap: anywhere; } .h { color: var(--muted); font-size: 11.5px; }");
    let tone = match p.tone {
        "err" => css!(".v { color: var(--err); } border-color: var(--err);"),
        "warn" => css!(".v { color: var(--warn); } border-color: var(--warn);"),
        "ok" => css!(".v { color: var(--ok); }"),
        _ => css!(""),
    };
    html! {
        <div class={classes!(wrap, tone)}>
            <div class="l">{ p.label.clone() }</div>
            <div class="v">{ p.value.clone() }</div>
            { if p.hint.is_empty() { html!{} } else { html!{ <div class="h">{ p.hint.clone() }</div> } } }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct GridProps {
    #[prop_or(180)]
    pub min: u32,
    pub children: Html,
}

#[function_component(Grid)]
pub fn grid(p: &GridProps) -> Html {
    html! { <div style={format!("display:grid;grid-template-columns:repeat(auto-fill,minmax({}px,1fr));gap:12px", p.min)}>{ p.children.clone() }</div> }
}

#[derive(Properties, PartialEq)]
pub struct ModalProps {
    pub title: AttrValue,
    pub on_close: Callback<()>,
    #[prop_or_default]
    pub wide: bool,
    pub children: Html,
}

#[styled_component(Modal)]
pub fn modal(p: &ModalProps) -> Html {
    let overlay = css!("position: fixed; inset: 0; background: rgba(8,14,23,.55); display: flex; align-items: flex-start; justify-content: center; padding: 6vh 16px; overflow-y: auto; z-index: 50;");
    let panel = css!("background: var(--surface); border: 1px solid var(--line); border-radius: 12px; box-shadow: 0 24px 60px rgba(0,0,0,.35); width: 100%; max-width: 560px; > header { display: flex; justify-content: space-between; align-items: center; padding: 14px 18px; border-bottom: 1px solid var(--line); } > header h3 { font-size: 16px; } > .body { padding: 18px; }");
    let wide = css!("max-width: 860px;");
    let close = { let c = p.on_close.clone(); Callback::from(move |_| c.emit(())) };
    html! {
        <div class={overlay} onclick={close.clone()}>
            <div class={classes!(panel, p.wide.then_some(wide))} role="dialog" aria-modal="true" aria-label={p.title.clone()} onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                <header><h3>{ p.title.clone() }</h3><Button variant={Variant::Ghost} small=true icon="x" title="Close" onclick={close} /></header>
                <div class="body">{ p.children.clone() }</div>
            </div>
        </div>
    }
}

// ------------------------------------------------------------------ navigation widgets

#[derive(Properties, PartialEq)]
pub struct TabsProps {
    pub labels: Vec<AttrValue>,
    pub active: usize,
    pub on_select: Callback<usize>,
}

#[styled_component(Tabs)]
pub fn tabs(p: &TabsProps) -> Html {
    let wrap = css!("display: flex; gap: 2px; flex-wrap: wrap; border-bottom: 1px solid var(--line); margin-bottom: 16px; button { background: none; border: 0; border-bottom: 2px solid transparent; padding: 9px 13px; cursor: pointer; color: var(--muted); font-weight: 550; margin-bottom: -1px; } button:hover { color: var(--ink); } button.on { color: var(--brand); border-bottom-color: var(--brand); }");
    html! {
        <div class={wrap} role="tablist">
            { for p.labels.iter().enumerate().map(|(i, l)| {
                let cb = p.on_select.clone();
                html! { <button type="button" role="tab" class={(i == p.active).then_some("on")} aria-selected={(i == p.active).to_string()} onclick={Callback::from(move |_| cb.emit(i))}>{ l.clone() }</button> }
            }) }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct PagerProps {
    pub page: u32,
    pub per_page: u32,
    pub total: Option<u64>,
    pub shown: usize,
    pub on_page: Callback<u32>,
}

#[styled_component(Pager)]
pub fn pager(p: &PagerProps) -> Html {
    let wrap = css!("display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 4px 0; color: var(--muted); font-size: 12.5px;");
    let first = (p.page as u64 - 1) * p.per_page as u64 + 1;
    let last = first + p.shown as u64 - 1;
    let has_next = match p.total {
        Some(t) => last < t,
        None => p.shown as u32 >= p.per_page,
    };
    let label = if p.shown == 0 { "No rows".to_string() } else { match p.total { Some(t) => format!("{first}–{last} of {t}"), None => format!("{first}–{last}") } };
    let (prev, next) = (p.page.saturating_sub(1).max(1), p.page + 1);
    let on = p.on_page.clone();
    let on2 = p.on_page.clone();
    html! {
        <div class={wrap}>
            <span>{ label }</span>
            <span style="display:flex;gap:8px;align-items:center">
                <Button small=true disabled={p.page <= 1} onclick={Callback::from(move |_| on.emit(prev))}>{ "Previous" }</Button>
                <span>{ format!("Page {}", p.page) }</span>
                <Button small=true disabled={!has_next} onclick={Callback::from(move |_| on2.emit(next))}>{ "Next" }</Button>
            </span>
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct EmptyProps {
    pub children: Html,
}

#[styled_component(Empty)]
pub fn empty(p: &EmptyProps) -> Html {
    let wrap = css!("text-align: center; color: var(--muted); padding: 36px 12px;");
    html! { <div class={wrap}>{ p.children.clone() }</div> }
}
