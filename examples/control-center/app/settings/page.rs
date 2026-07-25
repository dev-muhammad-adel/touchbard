//! Settings index (`/settings`).

use dioxus::prelude::*;
use touchbard::routing::use_navigate;

#[component]
pub fn Page() -> Element {
    let navigate = use_navigate();

    rsx! {
        div {
            style: "display: flex; flex-direction: column; align-items: center; gap: 4px;",

            span { "Settings index" }

            div {
                style: "display: flex; flex-direction: row; gap: 6px;",
                button {
                    style: "height: 12px; line-height: 12px; font-size: 10px; color: #c0caf5; background: #24283b; border: 1px solid #414868; border-radius: 3px; padding: 0 8px; cursor: pointer;",
                    onclick: move |_| navigate.push("/settings/general"),
                    "General"
                }
                button {
                    style: "height: 12px; line-height: 12px; font-size: 10px; color: #c0caf5; background: #24283b; border: 1px solid #414868; border-radius: 3px; padding: 0 8px; cursor: pointer;",
                    onclick: move |_| navigate.push("/settings/audio"),
                    "Audio"
                }
            }
        }
    }
}