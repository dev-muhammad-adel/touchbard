//! Dynamic section page (`/settings/:section`).
//!
//! The `[section]` directory is a Next.js-style dynamic segment; the param
//! value is read reactively with `use_route_param`.

use dioxus::prelude::*;
use touch_ui::routing::use_route_param;

#[component]
pub fn Page() -> Element {
    let section = use_route_param::<String>("section").unwrap_or_default();

    rsx! {
        div {
            style: "display: flex; flex-direction: column; align-items: center; gap: 2px;",

            span { style: "color: #9ece6a; font-weight: 600;", "Section: {section}" }
            span { style: "color: #565f89; font-size: 10px;", "Dynamic route [section] via use_route_param" }
        }
    }
}