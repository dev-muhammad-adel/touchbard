//! Home page (`/`): a pure animation smoothness test.
//!
//! The whole screen — 2008×60 logical pixels, rendered 1:1 on the bar (the
//! default scale factor of both backends is 1.0) — is an empty, near-black
//! background with exactly one thing on it: a single small circle.
//!
//! The circle slides continuously from the left edge of the bar to the right
//! edge and back. The motion is one infinite CSS `transform: translateX(...)`
//! animation with a linear timing function, driven entirely by the runtime's
//! CSS animation pipeline:
//!
//! ```text
//! Dioxus → Blitz → Stylo CSS animation → document.resolve(now)
//!         → Vello → Frame → Preview/DRM
//! ```
//!
//! There is no application state, no timer, no manual per-frame update — the
//! only thing advancing the circle is the CSS animation itself, so this page
//! validates that the event/animation-driven scheduler can smoothly advance a
//! continuous CSS transform animation without any input.
//!
//! The circle is 22px in diameter and keeps a 16px margin from both edges, so
//! it travels `2008 − 2·16 − 22 = 1954px` per half-cycle: `translateX(0)` →
//! `translateX(1954px)` → `translateX(0)`, one complete left-to-right-to-left
//! cycle every 20s, `linear` and `infinite`.
//!
//! This page is intentionally the only thing at `/` — no cards, buttons, text,
//! navigation or challenge. The demo/showcase pages keep their existing routes
//! and remain reachable through the shared layout's navigation strip; this page
//! draws no navigation of its own.

use dioxus::prelude::*;

#[rustfmt::skip]
const HOME_STYLE: &str = r#"
    @keyframes circleSlide {
        from { transform: translateX(0px); }
        50%  { transform: translateX(1954px); }
        to   { transform: translateX(0px); }
    }

    .stage {
        width: 100%; height: 100%;
        display: flex; flex-direction: column;
        justify-content: center; align-items: flex-start;
        background: #0b0e14; overflow: hidden;
    }
    .ball {
        width: 22px; height: 22px; margin-left: 16px;
        background: #7aa2f7; border-radius: 50%;
        animation: circleSlide 60s linear infinite;
    }
"#;

#[component]
pub fn Page() -> Element {
    rsx! {
        div {
            class: "stage",
            style { {HOME_STYLE} }
            div { class: "ball" }
        }
    }
}
