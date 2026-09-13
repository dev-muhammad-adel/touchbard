//! About page (`/about`) — a plain static route from `about.rs`.

use dioxus::prelude::*;

#[component]
pub fn Page() -> Element {
    rsx! {
        div {
            style: "display: flex; flex-direction: column; align-items: center; gap: 2px;",
            span { style: "font-weight: 600;", "About Touch UI" }
            span { style: "color: #565f89; font-size: 10px;", "A file-based routing demo — app_router!()" }
        }
    }
}