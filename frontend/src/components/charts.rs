//! Lightweight charts (plain HTML / inline SVG) that follow the theme tokens.
use stylist::yew::styled_component;
use yew::prelude::*;

use crate::util;

#[derive(Properties, PartialEq)]
pub struct BarProps {
    pub items: Vec<(String, f64)>,
    #[prop_or_default]
    pub money: bool,
}

/// Horizontal bars; negative values are drawn in the error colour.
#[styled_component(BarChart)]
pub fn bar_chart(p: &BarProps) -> Html {
    let wrap = css!(
        r#"
        display: grid; grid-template-columns: minmax(80px, 200px) 1fr auto; gap: 6px 12px; align-items: center; font-size: 12.5px;
        .lab { color: var(--ink-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .track { background: var(--surface-2); border-radius: 4px; height: 14px; overflow: hidden; border: 1px solid var(--line); }
        .fill { height: 100%; background: var(--c1); border-radius: 3px; min-width: 2px; }
        .fill.neg { background: var(--err); }
        .val { font-variant-numeric: tabular-nums; color: var(--ink); font-weight: 600; text-align: right; }
        "#
    );
    let max = p.items.iter().map(|(_, v)| v.abs()).fold(0.0_f64, f64::max).max(f64::MIN_POSITIVE);
    html! {
        <div class={wrap} role="img" aria-label="Bar chart">
            { for p.items.iter().map(|(l, v)| {
                let w = (v.abs() / max * 100.0).clamp(0.0, 100.0);
                html! { <>
                    <div class="lab" title={l.clone()}>{ l.clone() }</div>
                    <div class="track"><div class={classes!("fill", (*v < 0.0).then_some("neg"))} style={format!("width:{w:.1}%")} /></div>
                    <div class="val">{ if p.money { util::compact(*v) } else { util::compact(*v) } }</div>
                </> }
            }) }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct LineProps {
    pub points: Vec<(String, f64)>,
}

/// A single-series area/line chart for time series.
#[styled_component(LineChart)]
pub fn line_chart(p: &LineProps) -> Html {
    let wrap = css!("svg { width: 100%; height: auto; display: block; } .ax { fill: var(--muted); font-size: 10px; } .grid { stroke: var(--line); stroke-width: 1; } .ln { fill: none; stroke: var(--c1); stroke-width: 2; stroke-linejoin: round; } .ar { fill: var(--c1); opacity: .12; } .dot { fill: var(--c1); }");
    let n = p.points.len();
    if n < 2 {
        return html! {};
    }
    let (w, h, pl, pr, pt, pb) = (640.0, 190.0, 46.0, 12.0, 12.0, 26.0);
    let min = p.points.iter().map(|(_, v)| *v).fold(f64::INFINITY, f64::min).min(0.0);
    let max = p.points.iter().map(|(_, v)| *v).fold(f64::NEG_INFINITY, f64::max);
    let span = if (max - min).abs() < f64::EPSILON { 1.0 } else { max - min };
    let x = |i: usize| pl + (w - pl - pr) * i as f64 / (n - 1) as f64;
    let y = |v: f64| pt + (h - pt - pb) * (1.0 - (v - min) / span);
    let line: String = p.points.iter().enumerate().map(|(i, (_, v))| format!("{}{:.1},{:.1}", if i == 0 { "M" } else { " L" }, x(i), y(*v))).collect();
    let area = format!("{line} L{:.1},{:.1} L{:.1},{:.1} Z", x(n - 1), y(min.max(0.0).min(max)), x(0), y(min.max(0.0).min(max)));
    let step = (n / 6).max(1);
    html! {
        <div class={wrap}>
            <svg viewBox={format!("0 0 {w} {h}")} role="img" aria-label="Line chart">
                { for [0.0, 0.5, 1.0].iter().map(|f| { let v = min + span * f; html! { <g>
                    <line class="grid" x1={pl.to_string()} x2={(w - pr).to_string()} y1={format!("{:.1}", y(v))} y2={format!("{:.1}", y(v))} />
                    <text class="ax" x={(pl - 6.0).to_string()} y={format!("{:.1}", y(v) + 3.0)} text-anchor="end">{ util::compact(v) }</text>
                </g> } }) }
                <path class="ar" d={area} />
                <path class="ln" d={line} />
                { for p.points.iter().enumerate().map(|(i, (l, v))| html! {
                    <g>
                        <circle class="dot" cx={format!("{:.1}", x(i))} cy={format!("{:.1}", y(*v))} r="2.6"><title>{ format!("{l}: {}", util::compact(*v)) }</title></circle>
                        { if i % step == 0 || i == n - 1 { html! { <text class="ax" x={format!("{:.1}", x(i))} y={(h - 8.0).to_string()} text-anchor="middle">{ l.chars().take(10).collect::<String>() }</text> } } else { html! {} } }
                    </g>
                }) }
            </svg>
        </div>
    }
}
