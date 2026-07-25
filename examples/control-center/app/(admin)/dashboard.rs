//! Dashboard (`/dashboard`) — served from the `(admin)` route group.
//!
//! Route groups add a directory but no path segment, so `(admin)` does not
//! appear in the URL. The `Page` still lives in its own module namespace.

use dioxus::prelude::*;
use touchbard::routing::use_route;

#[component]
pub fn Page() -> Element {
    let route = use_route();

    rsx! {
        div {
            style: "display: flex; flex-direction: column; align-items: center; gap: 2px;",

            span { style: "color: #7dcfff; font-weight: 600;", "Dashboard" }
            span { style: "color: #565f89; font-size: 10px;", "Route group (admin) — current: {route}" }
        }
    }
}