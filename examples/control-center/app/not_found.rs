//! 404 fallback (`app/not_found.rs`), rendered inside the root layout whenever
//! no route matches.

use dioxus::prelude::*;

#[component]
pub fn Page() -> Element {
    rsx! {
        div {
            style: "display: flex; flex-direction: column; align-items: center; gap: 2px;",

            span { style: "color: #e5534b; font-weight: 600;", "404 — page not found" }
            span { style: "color: #565f89; font-size: 10px;", "app_router! fallback (app/not_found.rs)" }
        }
    }
}