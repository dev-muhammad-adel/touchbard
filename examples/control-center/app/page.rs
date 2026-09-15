//! Home page (`/`): the UI capabilities playground.
//!
//! Every tile is a *live* sample of one capability of the current rendering
//! stack and jumps to the showcasing page it stands for on tap. Only things the
//! stack has actually been proven to do (see the `showcase/*` pages and the
//! CSS-animation regression test in `touchbard/tests`) are used here: no icons,
//! no `<svg>`, no `onpointer*`, no invented APIs. The pulsing dot below is a
//! real CSS animation driven by the render loop.

use dioxus::prelude::*;
use touchbard::routing::use_navigate;

#[rustfmt::skip]
const HOME_STYLE: &str = r#"
    @keyframes tilePulse { 0% { opacity: 1; } 50% { opacity: 0.25; } 100% { opacity: 1; } }
    .pulse-dot { animation: tilePulse 1.2s ease-in-out infinite; }
"#;

#[component]
pub fn Page() -> Element {
    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: row; align-items: stretch; gap: 5px; padding: 1px 5px; box-sizing: border-box;",
            style { {HOME_STYLE} }

            Tile {
                to: "/showcase/text",
                title: "Text & color",
                caption: "sizes · aligns · borders",
                sample: rsx! {
                    div { style: "width: 6px; height: 6px; background: #7aa2f7; border-radius: 2px;" }
                    div { style: "width: 6px; height: 6px; background: #9ece6a; border-radius: 2px;" }
                    div { style: "width: 6px; height: 6px; background: #f7768e; border-radius: 2px;" }
                },
            }
            Tile {
                to: "/showcase/widgets",
                title: "Widgets",
                caption: "tap · hold · events",
                sample: rsx! {
                    div {
                        style: "display: flex; flex-direction: row; align-items: center; gap: 3px; background: #24283b; border: 1px solid #414868; border-radius: 3px; padding: 1px 4px;",
                        span { style: "font-size: 8px; line-height: 8px; color: #f7768e;", "-" }
                        span { style: "font-size: 9px; line-height: 9px; color: #9ece6a;", "0" }
                        span { style: "font-size: 8px; line-height: 8px; color: #7aa2f7;", "+" }
                    }
                },
            }
            Tile {
                to: "/showcase/panels",
                title: "Panels",
                caption: "flex · gaps · padding",
                sample: rsx! {
                    div {
                        style: "width: 11px; height: 8px; background: #24283b; border: 1px solid #565f89; border-radius: 3px; display: flex; align-items: center; justify-content: center;",
                        div { style: "width: 3px; height: 3px; background: #bb9af7; border-radius: 1.5px;" }
                    },
                },
            }
            Tile {
                to: "/showcase/motion",
                title: "Motion",
                caption: "live CSS animation",
                sample: rsx! {
                    div {
                        class: "pulse-dot",
                        style: "width: 8px; height: 8px; background: #f7768e; border-radius: 100%;",
                    },
                },
            }
            Tile {
                to: "/showcase/counter",
                title: "Counter",
                caption: "classic demo",
                sample: rsx! {
                    span { style: "font-size: 10px; line-height: 11px; color: #7aa2f7; font-weight: 600;", "Count: 0" },
                },
            }
            Tile {
                to: "/showcase/system",
                title: "System",
                caption: "render pipeline",
                sample: rsx! {
                    div {
                        style: "width: 8px; height: 8px; border: 1px solid #9ece6a; border-radius: 100%; display: flex; align-items: center; justify-content: center;",
                        span { style: "font-size: 6px; line-height: 7px; color: #9ece6a;", "i" }
                    },
                },
            }
        }
    }
}

/// A full-height tile: a live sample on top, a title and caption below, and a
/// tap anywhere that pushes to the showcasing page.
#[component]
fn Tile(to: String, title: String, caption: String, sample: Element) -> Element {
    let navigate = use_navigate();
    rsx! {
        div {
            style: "flex: 1; min-width: 0; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 1px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 0 3px; cursor: pointer;",
            onclick: move |_| navigate.push(&to),

            div { style: "display: flex; flex-direction: row; align-items: center; gap: 3px;", {sample} }
            span { style: "font-size: 9px; line-height: 10px; color: #c0caf5;", "{title}" }
            span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "{caption}" }
        }
    }
}