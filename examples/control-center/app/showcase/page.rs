//! Showcase index (`/showcase`): categories of what the stack can paint and
//! react to. Each entry is a plain Dioxus button that routes to its page.

use dioxus::prelude::*;
use touchbard::routing::use_navigate;

#[component]
pub fn Page() -> Element {
    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: row; align-items: center; gap: 5px; padding: 0 4px; box-sizing: border-box;",

            Cat {
                to: "/showcase/text",
                title: "Text & color",
                hint: "font sizes · weights · colors · alignment · inverted · radius",
            }
            Cat {
                to: "/showcase/widgets",
                title: "Widgets",
                hint: "counter · toggle · press & hold · live events · inert",
            }
            Cat {
                to: "/showcase/panels",
                title: "Panels",
                hint: "flex grow · gaps · justify · align · padding",
            }
            Cat {
                to: "/showcase/motion",
                title: "Motion",
                hint: "CSS animation: opacity · width · transform · color",
            }
            Cat {
                to: "/showcase/counter",
                title: "Counter",
                hint: "the preserved original Control Center home demo",
            }
            Cat {
                to: "/showcase/system",
                title: "System",
                hint: "pipeline · viewport · frame format · input events",
            }
        }
    }
}

#[component]
fn Cat(to: String, title: String, hint: String) -> Element {
    let navigate = use_navigate();
    rsx! {
        button {
            style: "flex: 1; min-width: 0; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 1px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 0 4px; cursor: pointer;",
            onclick: move |_| navigate.push(&to),

            span { style: "font-size: 9px; line-height: 10px; color: #c0caf5;", "{title}" }
            span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "{hint}" }
        }
    }
}