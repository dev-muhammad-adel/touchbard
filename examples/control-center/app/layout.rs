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
    // Keyboard usage disabled (hidden). Uncomment these lines to re-enable the
    // physical key readout (`kbd: <key> / <gesture>`) via keyboard.rs.
    // let keyboard_event = crate::keyboard::use_key_events();
    let route = use_route();
    // Pointer activity readout: `moving` while the mouse / trackpad moves,
    // `idle` after 400 ms of stillness.
    let pointer_moving = crate::pointer::use_pointer_moving();
    // let keyboard_status = keyboard_event()
    //     .map(|event| format!("{:?} / {:?}", event.key, event.gesture))
    //     .unwrap_or_else(|| "waiting".to_string());

    // The home route is the full-bleed Touch Bar Playground: it owns the whole
    // viewport (2008×60) and draws its own chrome, so the generic strip is
    // skipped there. Every other route keeps the shared strip below.
    if route == "/" {
        return rsx! {
            div {
                style: "position: relative; width: 100%; height: 100%; background: #1a1b26; color: #c0caf5; font-family: system-ui, sans-serif;",
                {children}
     
                // Keyboard readout hidden (see header comment).
                span {
                    style: "position: absolute; right: 4px; bottom: 2px; color: #565f89; font-size: 18px;",
                    "mouse: {pointer_moving}"
                }
            }
        };
    }

    let mut clicks = use_signal(|| 0u32);

    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: column; background: #1a1b26; color: #c0caf5; font-family: system-ui, sans-serif; font-size: 12px;",

            // Navigation strip.
            div {
                style: "flex-shrink: 0; height: 12px; display: flex; flex-direction: row; align-items: center; gap: 2px; padding: 0 4px; background: #16161e; border-bottom: 1px solid #24283b;",

                span { style: "color: #7aa2f7; font-weight: 600; margin-right: 4px; font-size: 9px;", "Ctrl Center" }

                NavBtn { to: "/", label: "Home" }
                NavBtn { to: "/showcase/text", label: "Text" }
                NavBtn { to: "/showcase/widgets", label: "Widgets" }
                NavBtn { to: "/showcase/panels", label: "Panels" }
                NavBtn { to: "/showcase/motion", label: "Motion" }
                NavBtn { to: "/showcase/counter", label: "Counter" }
                NavBtn { to: "/showcase/system", label: "System" }
                NavBtn { to: "/showcase/tiles", label: "Tiles" }
                NavBtn { to: "/about", label: "About" }
                NavBtn { to: "/settings", label: "Settings" }
                NavBtn { to: "/settings/general", label: "General" }
                NavBtn { to: "/settings/audio", label: "Audio" }
                NavBtn { to: "/docs/intro/guide", label: "Docs" }
                NavBtn { to: "/boom", label: "Boom" }
                NavBtn { to: "/dashboard", label: "Dash" }
                NavBtn { to: "/no/such/route", label: "Bad" }

                button {
                    style: "height: 9px; line-height: 9px; font-size: 9px; color: #c0caf5; background: #24283b; border: 1px solid #414868; border-radius: 3px; padding: 0 6px; cursor: pointer;",
                    onclick: move |_| { clicks += 1; },
                    "Kick"
                }

                span {
                    style: "margin-left: auto; color: #565f89; font-size: 9px; white-space: nowrap; overflow: hidden;",
                    "clicks: {clicks} · {route}"
                }
                // Keyboard readout hidden (see header comment).
                // span {
                //     style: "margin-left: 6px; color: #565f89; font-size: 9px; white-space: nowrap;",
                //     "kbd: {keyboard_status}"
                // }
                span {
                    style: "margin-left: 6px; color: #565f89; font-size: 9px; white-space: nowrap;",
                    if pointer_moving() { "ptr: moving" } else { "ptr: idle" }
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
            style: "height: 9px; line-height: 9px; font-size: 9px; color: #c0caf5; background: #24283b; border: 1px solid #414868; border-radius: 3px; padding: 0 6px; cursor: pointer;",
            onclick: move |_| navigate.push(&to),
            "{label}"
        }
    }
}
