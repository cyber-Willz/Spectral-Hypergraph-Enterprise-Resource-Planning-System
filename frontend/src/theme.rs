//! Design tokens + global styles (Stylist) and the light / dark / system theme switch.
use gloo_storage::{LocalStorage, Storage};
use stylist::yew::Global;
use yew::prelude::*;

const KEY: &str = "suite.theme";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    System,
    Light,
    Dark,
}
impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::System => "system",
            Mode::Light => "light",
            Mode::Dark => "dark",
        }
    }
    pub fn parse(s: &str) -> Mode {
        match s {
            "light" => Mode::Light,
            "dark" => Mode::Dark,
            _ => Mode::System,
        }
    }
    pub fn next(self) -> Mode {
        match self {
            Mode::System => Mode::Light,
            Mode::Light => Mode::Dark,
            Mode::Dark => Mode::System,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct ThemeCtx {
    pub mode: Mode,
    pub cycle: Callback<()>,
}

const LIGHT: &str = r#"
  color-scheme: light;
  --bg:#f3f5f8; --surface:#ffffff; --surface-2:#f7f9fb; --ink:#0f1b2d; --ink-2:#3b4a5e; --muted:#66758a;
  --line:#e1e6ed; --line-strong:#c9d2de;
  --brand:#0f766e; --brand-hover:#0b5f58; --brand-soft:#e0f2f0; --brand-ink:#ffffff;
  --ok:#15803d; --ok-soft:#dcfce7; --warn:#b45309; --warn-soft:#fef3c7; --err:#b91c1c; --err-soft:#fee2e2; --info:#1d4ed8; --info-soft:#dbeafe;
  --side-bg:#0f1b2d; --side-ink:#c3cfdf; --side-muted:#7d8da3; --side-active:#183250; --side-line:#1d2c42;
  --shadow:0 1px 2px rgba(15,27,45,.06),0 6px 18px rgba(15,27,45,.06);
  --c1:#0f766e; --c2:#2563eb; --c3:#d97706; --c4:#9333ea; --c5:#dc2626; --c6:#475569;
"#;

const DARK: &str = r#"
  color-scheme: dark;
  --bg:#0a111b; --surface:#111b29; --surface-2:#0e1723; --ink:#e6edf5; --ink-2:#b6c2d1; --muted:#8a9aae;
  --line:#1f2d3f; --line-strong:#2d3e54;
  --brand:#2dd4bf; --brand-hover:#5eead4; --brand-soft:#0d2f2d; --brand-ink:#04211e;
  --ok:#4ade80; --ok-soft:#0f2a1b; --warn:#fbbf24; --warn-soft:#2f2410; --err:#f87171; --err-soft:#321516; --info:#7cb2ff; --info-soft:#14233f;
  --side-bg:#080e17; --side-ink:#b9c6d8; --side-muted:#71829a; --side-active:#14233a; --side-line:#16222f;
  --shadow:0 1px 2px rgba(0,0,0,.4),0 6px 18px rgba(0,0,0,.3);
  --c1:#2dd4bf; --c2:#60a5fa; --c3:#fbbf24; --c4:#c084fc; --c5:#f87171; --c6:#94a3b8;
"#;

/// Everything global: tokens for both themes, reset, typography, focus ring, scrollbars.
pub fn global_css() -> String {
    format!(
        r#"
:root {{ {LIGHT} --radius:10px; --font:"Inter","Segoe UI",system-ui,-apple-system,Roboto,"Helvetica Neue",sans-serif; --mono:ui-monospace,"SF Mono",Menlo,Consolas,monospace; }}
@media (prefers-color-scheme: dark) {{ :root[data-theme="system"] {{ {DARK} }} }}
:root[data-theme="dark"] {{ {DARK} }}
:root[data-theme="light"] {{ {LIGHT} }}
* {{ box-sizing: border-box; }}
html, body {{ margin: 0; min-height: 100%; }}
body {{ background: var(--bg); color: var(--ink); font: 14px/1.5 var(--font); -webkit-font-smoothing: antialiased; }}
h1, h2, h3, h4 {{ margin: 0; font-weight: 650; letter-spacing: -0.01em; }}
a {{ color: var(--brand); text-decoration: none; }}
a:hover {{ text-decoration: underline; }}
button, input, select, textarea {{ font: inherit; color: inherit; }}
:focus-visible {{ outline: 2px solid var(--brand); outline-offset: 2px; }}
::selection {{ background: var(--brand-soft); }}
::-webkit-scrollbar {{ width: 10px; height: 10px; }}
::-webkit-scrollbar-thumb {{ background: var(--line-strong); border-radius: 8px; border: 2px solid var(--bg); }}
code, pre {{ font-family: var(--mono); }}
"#
    )
}

#[derive(Properties, PartialEq)]
pub struct ThemeProps {
    pub children: Html,
}

#[function_component(ThemeProvider)]
pub fn theme_provider(p: &ThemeProps) -> Html {
    let mode = use_state(|| Mode::parse(&LocalStorage::get::<String>(KEY).unwrap_or_default()));
    {
        let mode = *mode;
        use_effect_with(mode, move |m| {
            if let Some(el) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.document_element()) {
                let _ = el.set_attribute("data-theme", m.as_str());
            }
            let _ = LocalStorage::set(KEY, m.as_str());
            || ()
        });
    }
    let cycle = {
        let mode = mode.clone();
        Callback::from(move |_| mode.set(mode.next()))
    };
    let ctx = ThemeCtx { mode: *mode, cycle };
    html! {
        <ContextProvider<ThemeCtx> context={ctx}>
            <Global css={global_css()} />
            { p.children.clone() }
        </ContextProvider<ThemeCtx>>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn global_css_parses_with_stylist() {
        let sheet = stylist::ast::Sheet::from_str(&global_css());
        assert!(sheet.is_ok(), "{:?}", sheet.err());
    }

    #[test]
    fn mode_round_trip() {
        for m in [Mode::System, Mode::Light, Mode::Dark] {
            assert_eq!(Mode::parse(m.as_str()), m);
        }
        assert_eq!(Mode::System.next().next().next(), Mode::System);
    }
}
