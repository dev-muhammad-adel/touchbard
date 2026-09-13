//! Nested settings layout. Because it renders `children` and stays at the same
//! position in the tree while a section changes, its scope (and any state it
//! holds) is preserved across `/settings/general` → `/settings/audio`, etc.

use dioxus::prelude::*;

#[component]
pub fn Layout(children: Element) -> Element {
    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: column; padding: 2px 6px; box-sizing: border-box; gap: 2px;",

            div {
                style: "height: 12px; color: #7aa2f7; font-weight: 600; display: flex; align-items: center; font-size: 11px;",
                "Settings (nested layout)"
            }

            div {
                style: "flex: 1; display: flex; align-items: center; justify-content: center; overflow: hidden; min-height: 0;",
                {children}
            }
        }
    }
}