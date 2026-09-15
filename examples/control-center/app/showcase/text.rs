//! `/showcase/text` — text, color and border rendering demos.
//!
//! All of these are plain CSS/Servo layout + Vello rasterization: no fonts were
//! embedded, no SVG/icon infra, nothing beyond the pipeline the stack ships.

use dioxus::prelude::*;

#[component]
pub fn Page() -> Element {
    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: row; align-items: stretch; gap: 5px; padding: 1px 4px; box-sizing: border-box;",

            Cell {
                caption: "font-size",
                sample: rsx! {
                    span { style: "font-size: 8px; line-height: 9px; color: #c0caf5;", "8" }
                    span { style: "font-size: 10px; line-height: 12px; color: #c0caf5;", "10" }
                    span { style: "font-size: 12px; line-height: 13px; color: #c0caf5;", "12" }
                    span { style: "font-size: 9px; line-height: 10px; color: #565f89;", "ppx" }
                },
            }
            Cell {
                caption: "font-weight",
                sample: rsx! {
                    span { style: "font-weight: 400; font-size: 9px; line-height: 10px; color: #a9b1d6;", "Aa" }
                    span { style: "font-weight: 650; font-size: 9px; line-height: 10px; color: #a9b1d6;", "Aa" }
                    span { style: "font-weight: 800; font-size: 9px; line-height: 10px; color: #a9b1d6;", "Aa" }
                },
            }
            Cell {
                caption: "accent colors",
                sample: rsx! {
                    Chip { color: "#7aa2f7" }
                    Chip { color: "#9ece6a" }
                    Chip { color: "#f7768e" }
                    Chip { color: "#bb9af7" }
                    Chip { color: "#e0af68" }
                },
            }
            Cell {
                caption: "text-align",
                sample: rsx! {
                    div {
                        style: "display: flex; flex-direction: column; gap: 1px; width: 100%;",
                        div { style: "width: 100%; text-align: left; font-size: 8px; line-height: 8px; color: #c0caf5;", "l" }
                        div { style: "width: 100%; text-align: center; font-size: 8px; line-height: 8px; color: #c0caf5;", "c" }
                        div { style: "width: 100%; text-align: right; font-size: 8px; line-height: 8px; color: #c0caf5;", "r" }
                    },
                },
            }
            Cell {
                caption: "inverted",
                sample: rsx! {
                    div {
                        style: "background: #c0caf5; color: #1a1b26; border-radius: 3px; padding: 1px 5px; font-size: 9px; line-height: 10px; font-weight: 600;",
                        "Touch"
                    }
                },
            }
            Cell {
                caption: "border-radius",
                sample: rsx! {
                    Corner { r: "2px" }
                    Corner { r: "4px" }
                    Corner { r: "7px" }
                    Corner { r: "100%" }
                },
            }
        }
    }
}

#[component]
fn Cell(caption: String, sample: Element) -> Element {
    rsx! {
        div {
            style: "flex: 1; min-width: 0; width: 100%; max-width: 100%; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 1px 3px; box-sizing: border-box; overflow: hidden;",
            div {
                style: "display: flex; flex-direction: row; align-items: center; justify-content: center; gap: 3px; width: 100%; overflow: hidden;",
                {sample}
            }
            span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "{caption}" }
        }
    }
}

#[component]
fn Chip(color: String) -> Element {
    rsx! {
        div { style: "width: 6px; height: 6px; background: {color}; border-radius: 2px;" }
    }
}

#[component]
fn Corner(r: String) -> Element {
    rsx! {
        div { style: "width: 7px; height: 7px; background: #9ece6a; border-radius: {r};" }
    }
}