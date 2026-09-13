//! Global ErrorBoundary fallback (`app/error.rs`).
//!
//! Used when a page throws while rendering; the generated Router wraps every
//! route in an `ErrorBoundary` whose `handle_error` renders this component.

use dioxus::prelude::*;

#[component]
pub fn Page() -> Element {
    rsx! {
        div {
            style: "display: flex; align-items: center; gap: 6px;",

            span { style: "color: #e5534b; font-weight: 600;", "An error was caught" }
            span { style: "color: #565f89; font-size: 10px;", "app/error.rs → ErrorBoundary fallback" }
        }
    }
}