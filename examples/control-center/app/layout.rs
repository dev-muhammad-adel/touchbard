//! Root layout: the Touch Bar chrome strip + a content area.
//!
//! Layouts are plain Dioxus components that take `children`. Every page under
//! this node is rendered inside them, so the strip persists across navigation
//! (Dioxus keeps the layout scope alive because it stays at the same position
//! in the component tree).

use dioxus::prelude::*;
use touchbard::routing::{use_navigate, use_route};

#[component]
pub fn Layout(children: Element) -> Element {
    let route = use_route();
    let mut clicks = use_signal(|| 0u32);

    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: column; background: #1a1b26; color: #c0caf5; font-family: system-ui, sans-serif; font-size: 12px;",

            // Navigation strip.
            div {
                style: "flex-shrink: 0; height: 14px; display: flex; flex-direction: row; align-items: center; gap: 4px; padding: 0 6px; background: #16161e; border-bottom: 1px solid #24283b;",

                span { style: "color: #7aa2f7; font-weight: 600; margin-right: 4px; font-size: 10px;", "Control Center" }

                NavBtn { to: "/", label: "Home" }
                NavBtn { to: "/about", label: "About" }
                NavBtn { to: "/settings", label: "Settings" }
                NavBtn { to: "/settings/general", label: "General" }
                NavBtn { to: "/settings/audio", label: "Audio" }
                NavBtn { to: "/docs/intro/guide", label: "Docs" }
                NavBtn { to: "/boom", label: "Boom" }
                NavBtn { to: "/dashboard", label: "Dash" }
                NavBtn { to: "/no/such/route", label: "Bad" }

                button {
                    style: "height: 12px; line-height: 12px; font-size: 10px; color: #c0caf5; background: #24283b; border: 1px solid #414868; border-radius: 3px; padding: 0 8px; cursor: pointer;",
                    onclick: move |_| { clicks += 1; },
                    "Kick"
                }

                span {
                    style: "margin-left: auto; color: #565f89; font-size: 10px;",
                    "clicks: {clicks} · route: {route}"
                }
            }

            // Page content.
            div {
                style: "flex: 1; width: 100%; display: flex; align-items: center; justify-content: center; overflow: hidden; min-height: 0;",
                {children}
            }
        }
    }
}

/// A simple push-button that navigates to `to` when clicked.
#[component]
fn NavBtn(to: String, label: String) -> Element {
    let navigate = use_navigate();
    rsx! {
        button {
            style: "height: 12px; line-height: 12px; font-size: 10px; color: #c0caf5; background: #24283b; border: 1px solid #414868; border-radius: 3px; padding: 0 8px; cursor: pointer;",
            onclick: move |_| navigate.push(&to),
            "{label}"
        }
    }
}