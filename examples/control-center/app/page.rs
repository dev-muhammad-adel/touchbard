//! Home page (`/`): the classic Touchbard counter demo.

use dioxus::prelude::*;

#[component]
pub fn Page() -> Element {
    let mut counter = use_signal(|| 0i32);

    rsx! {
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
    }
}