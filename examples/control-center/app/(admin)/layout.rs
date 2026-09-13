//! Route group layout for `(admin)`.
//!
//! `(admin)` adds a directory but no URL segment, so this layout wraps only the
//! routes under the group (`/dashboard`) while never appearing in the URL. Its
//! state stays alive while navigation stays inside the group, exactly like a
//! normal nested layout.

use dioxus::prelude::*;

#[component]
pub fn Layout(children: Element) -> Element {
    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: column; padding: 2px 6px; box-sizing: border-box; gap: 2px; background: #16161e;",

            div {
                style: "height: 12px; color: #bb9af7; font-weight: 600; display: flex; align-items: center; font-size: 11px;",
                "Admin (route group layout)"
            }

            div {
                style: "flex: 1; display: flex; align-items: center; justify-content: center; overflow: hidden; min-height: 0;",
                {children}
            }
        }
    }
}