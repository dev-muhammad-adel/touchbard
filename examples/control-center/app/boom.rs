//! Error demo page (`/boom`): always throws while rendering, so the router's
//! ErrorBoundary catches it and falls back to `app/error.rs`.

use dioxus::prelude::*;

#[component]
pub fn Page() -> Element {
    dioxus::core::bail!("intentional error thrown by /boom");
}