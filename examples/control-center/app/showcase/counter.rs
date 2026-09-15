//! `/showcase/counter` — the original Control Center home page, preserved.
//!
//! This was the content of `app/page.rs` before the home page became the
//! capabilities playground. It is kept as its own route so nothing was lost.

use dioxus::prelude::*;
use touchbard::routing::use_navigate;

#[component]
pub fn Page() -> Element {
    let mut counter = use_signal(|| 0i32);
    let navigate = use_navigate();

    rsx! {
        div {
            style: "display: flex; flex-direction: column; align-items: center; gap: 3px;",

            div {
                style: "display: flex; flex-direction: row; align-items: center; gap: 6px;",

                span { "Touchbard Demo" }

                div {
                    style: "width: 64px; text-align: center; background: #3b4261; border: 1px solid #565f89; border-radius: 3px; color: #c0caf5; line-height: 12px;",
                    "Count: {counter}"
                }

                button {
                    style: "height: 12px; line-height: 12px; color: #f7768e; padding: 0 10px; background: #24283b; border: 1px solid #414868; border-radius: 3px; cursor: pointer;",
                    onclick: move |_| { counter -= 1; },
                    "-"
                }

                button {
                    style: "height: 12px; line-height: 12px; color: #9ece6a; padding: 0 10px; background: #24283b; border: 1px solid #414868; border-radius: 3px; cursor: pointer;",
                    onclick: move |_| { counter += 1; },
                    "+"
                }
            }

            div {
                style: "display: flex; flex-direction: row; align-items: center; gap: 8px;",
                span { style: "font-size: 9px; line-height: 10px; color: #565f89;", "original Control Center home preserved here" }
                button {
                    style: "height: 9px; line-height: 9px; font-size: 9px; color: #7aa2f7; background: #24283b; border: 1px solid #414868; border-radius: 3px; padding: 0 6px; cursor: pointer;",
                    onclick: move |_| navigate.push("/"),
                    "back to playground"
                }
            }
        }
    }
}