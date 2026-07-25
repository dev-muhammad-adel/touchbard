//! Docs catch-all (`/docs/*`).
//!
//! `[...chapter]` captures all remaining path segments; the joined value is
//! stored under the `chapter` parameter.

use dioxus::prelude::*;
use touchbard::routing::use_route_param;

#[component]
pub fn Page() -> Element {
    let chapter = use_route_param::<String>("chapter").unwrap_or_default();

    rsx! {
        div {
            style: "display: flex; flex-direction: column; align-items: center; gap: 2px;",

            span { style: "color: #bb9af7; font-weight: 600;", "Docs: {chapter}" }
            span { style: "color: #565f89; font-size: 10px;", "Catch-all [...chapter] — remaining segments joined with /" }
        }
    }
}