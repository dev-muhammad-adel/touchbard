//! `/showcase/widgets` — what the stack can actually do with input.
//!
//! Blitz only synthesizes `click`, `mousedown`, `mouseup` and `mousemove` DOM
//! events (dioxus-native-dom maps exactly those into Dioxus handlers) — there
//! is no `pointer*`, hover or scroll support. So everything here is built on
//! `onclick` / `onmousedown` / `onmouseup` / `onmousemove` only, and the
//! event-readout cell shows that honestly.

use dioxus::prelude::*;
use dioxus::html::InteractionLocation;
use touchbard::routing::use_navigate;

#[component]
pub fn Page() -> Element {
    let mut counter = use_signal(|| 0i32);
    let mut on = use_signal(|| false);
    let mut held = use_signal(|| false);
    let mut last = use_signal(|| String::from("idle"));

    // Reactive renders: read the signals once, then only identifiers are
    // interpolated into style strings (Dioxus string segments cannot hold
    // arbitrary `if` expressions).
    let on_bg = if *on.read() { "#9ece6a" } else { "#24283b" };
    let on_fg = if *on.read() { "#1a1b26" } else { "#f7768e" };
    let toggle_label = if *on.read() { "ON" } else { "OFF" };
    let held_bg = if *held.read() { "#bb9af7" } else { "#16161e" };

    let navigate = use_navigate();

    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: row; align-items: stretch; gap: 5px; padding: 1px 4px; box-sizing: border-box;",

            // Counter: the simplest interactive widget, `onclick` only.
            div {
                style: "flex: 1; min-width: 0; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 1px 3px; box-sizing: border-box; overflow: hidden;",
                div {
                    style: "display: flex; flex-direction: row; align-items: center; gap: 4px;",
                    button {
                        style: "height: 10px; line-height: 10px; font-size: 9px; color: #f7768e; background: #16161e; border: 1px solid #414868; border-radius: 3px; padding: 0 6px; cursor: pointer;",
                        onclick: move |_| { counter -= 1; },
                        "-"
                    }
                    span { style: "min-width: 22px; text-align: center; font-size: 9px; line-height: 10px; color: #9ece6a;", "{counter}" }
                    button {
                        style: "height: 10px; line-height: 10px; font-size: 9px; color: #9ece6a; background: #16161e; border: 1px solid #414868; border-radius: 3px; padding: 0 6px; cursor: pointer;",
                        onclick: move |_| { counter += 1; },
                        "+"
                    }
                }
                span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "counter · onclick" }
            }

            // Toggle pill: flips on every tap.
            div {
                style: "flex: 1; min-width: 0; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 1px 3px; box-sizing: border-box; overflow: hidden;",
                div {
                    style: "border-radius: 5px; border: 1px solid #414868; padding: 1px 8px; font-size: 9px; line-height: 10px; cursor: pointer; background: {on_bg}; color: {on_fg};",
                    onclick: move |_| { let next = !*on.read(); on.set(next); },
                    {toggle_label}
                }
                span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "toggle · onclick" }
            }

            // Press & hold: lights up while the button is held down.
            div {
                style: "flex: 1; min-width: 0; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 1px 3px; box-sizing: border-box; overflow: hidden;",
                div {
                    style: "width: 12px; height: 10px; border-radius: 3px; border: 1px solid #414868; background: {held_bg};",
                    onmousedown: move |_| { held.set(true); },
                    onmouseup: move |_| { held.set(false); },
                }
                // Note: Blitz only synthesizes click/mousedown/mouseup/mousemove, so a
                // press released *outside* the box leaves it lit — there is no
                // mouseleave event to clear it. Accepted as a documented quirk.
                span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "hold · mousedown/up" }
            }

            // Live event readout: shows the raw event the last interaction produced.
            div {
                style: "flex: 1.3; min-width: 0; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 1px 3px; box-sizing: border-box; overflow: hidden;",
                onmousedown: move |e: MouseEvent| last.set(describe("down", &e)),
                onmouseup: move |e: MouseEvent| last.set(describe("up", &e)),
                onmousemove: move |e: MouseEvent| last.set(describe("move", &e)),
                span { style: "font-size: 8px; line-height: 9px; color: #9ece6a; width: 100%; text-align: center; overflow: hidden;", "events: {last}" }
                span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "readout · mousedown/up/move" }
            }

            // Inert: enabled-looking but deliberately does nothing.
            div {
                style: "flex: 1; min-width: 0; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 1px 3px; box-sizing: border-box; overflow: hidden;",
                div {
                    style: "height: 10px; line-height: 10px; font-size: 9px; color: #565f89; background: #1a1b26; border: 1px solid #2f334d; border-radius: 3px; padding: 0 6px;",
                    "Grey"
                }
                span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "inert · no handler" }
            }

            // Jump back to the playground.
            div {
                style: "flex: 0 0 34px; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 1px; background: #16161e; border: 1px solid #414868; border-radius: 5px; cursor: pointer;",
                onclick: move |_| navigate.push("/"),
                span { style: "font-size: 10px; line-height: 11px; color: #7aa2f7;", "Home" }
                span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "back" }
            }
        }
    }
}

/// Turn a mouse event into a short "name x,y" description using the client
/// (viewport) coordinates Blitz reported. `x,y` are logical pixels.
fn describe(name: &str, e: &MouseEvent) -> String {
    let p = e.data.client_coordinates();
    format!("{name} {},{}", p.x.round() as i32, p.y.round() as i32)
}