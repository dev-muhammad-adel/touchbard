//! `/showcase/system` — facts about the stack, rendered through the stack
//! itself. No wall of text: each fact is a short strip on the single-row
//! layout.

use dioxus::prelude::*;
use touchbard::routing::use_route;

#[component]
pub fn Page() -> Element {
    let route = use_route();

    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: row; align-items: center; gap: 5px; padding: 1px 4px; box-sizing: border-box;",

            Fact {
                title: "Render pipeline",
                value: "Dioxus → Blitz → AnyRender → Vello (CPU)",
            }
            Fact {
                title: "Frame",
                value: "premultiplied RGBA8, flush via DRM or preview ws",
            }
            Fact {
                title: "Viewport",
                value: "2008×60 physical @ 2.0 → 1004×30 logical",
            }
            Fact {
                title: "Input",
                value: "onclick · onmousedown · onmouseup · onmousemove (no pointer* / hover)",
            }
            Fact {
                title: "Route",
                value: "{route}",
            }
        }
    }
}

#[component]
fn Fact(title: String, value: String) -> Element {
    rsx! {
        div {
            style: "flex: 1; min-width: 0; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 1px 3px; box-sizing: border-box; overflow: hidden;",
            span { style: "font-size: 8px; line-height: 9px; color: #7aa2f7;", "{title}" }
            span { style: "font-size: 8px; line-height: 9px; color: #c0caf5; text-align: center;", "{value}" }
        }
    }
}