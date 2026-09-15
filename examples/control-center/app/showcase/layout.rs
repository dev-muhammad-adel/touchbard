//! Nested layout for the `/showcase` subtree.
//!
//! A second-level layout (same `layout.rs` convention as the root one): a fixed
//! label on the left and the page content filling the rest of the row. This
//! demonstrates nested layouts while keeping the strip-shaped viewport fully
//! usable.

use dioxus::prelude::*;

#[component]
pub fn Layout(children: Element) -> Element {
    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: row; align-items: center; gap: 6px; padding: 0 6px; box-sizing: border-box; background: #16161e;",

            span {
                style: "flex-shrink: 0; color: #565f89; font-size: 8px; line-height: 9px; letter-spacing: 1px; text-transform: uppercase;",
                "Showcase"
            }

            div {
                style: "flex: 1; min-width: 0; align-self: stretch; display: flex; align-items: center; justify-content: center; overflow: hidden;",
                {children}
            }
        }
    }
}