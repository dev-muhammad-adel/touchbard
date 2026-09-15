//! `/showcase/panels` — layout demos: flex sizing, gaps, justify/align and
//! padding. All standard Servo flexbox properties, no custom layout code.

use dioxus::prelude::*;

#[component]
pub fn Page() -> Element {
    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: row; align-items: stretch; gap: 5px; padding: 1px 4px; box-sizing: border-box;",

            Cell {
                caption: "flex grow",
                sample: rsx! {
                    Blox { w: "24px" }
                    Blox { w: "flex 1" }
                    Blox { w: "flex 1" }
                },
            }
            Cell {
                caption: "gap 2 vs 8",
                sample: rsx! {
                    div {
                        style: "display: flex; flex-direction: row; align-items: center; gap: 2px;",
                        Blox { w: "8px" }
                        Blox { w: "8px" }
                        Blox { w: "8px" }
                    }
                    div {
                        style: "display: flex; flex-direction: row; align-items: center; gap: 8px;",
                        Blox { w: "6px" }
                        Blox { w: "6px" }
                        Blox { w: "6px" }
                    }
                },
            }
            Cell {
                caption: "space-between",
                sample: rsx! {
                    div {
                        style: "width: 60px; display: flex; flex-direction: row; align-items: center; justify-content: space-between; background: #16161e; border: 1px solid #414868; border-radius: 3px; padding: 1px 2px;",
                        Chip {}
                        Chip {}
                        Chip {}
                    },
                },
            }
            Cell {
                caption: "align-items",
                sample: rsx! {
                    div {
                        style: "display: flex; flex-direction: row; align-items: center; gap: 2px;",
                        div { style: "width: 5px; height: 9px; background: #bb9af7; border-radius: 2px;" }
                        div { style: "width: 5px; height: 5px; background: #7aa2f7; border-radius: 2px;" }
                        div { style: "width: 5px; height: 7px; background: #9ece6a; border-radius: 2px;" }
                    },
                },
            }
            Cell {
                caption: "padding",
                sample: rsx! {
                    div {
                        style: "display: flex; flex-direction: row; align-items: center; gap: 8px;",
                        div {
                            style: "display: flex; align-items: center; justify-content: center; width: 20px; height: 9px; background: #24283b; border: 1px solid #7aa2f7; border-radius: 3px;",
                            span { style: "font-size: 7px; line-height: 8px; color: #7aa2f7;", "2" }
                        }
                        div {
                            style: "display: flex; align-items: center; justify-content: center; width: 20px; height: 9px; padding: 3px; background: #24283b; border: 1px solid #7aa2f7; border-radius: 3px; box-sizing: border-box;",
                            span { style: "font-size: 7px; line-height: 8px; color: #7aa2f7;", "3" }
                        }
                    },
                },
            }
        }
    }
}

#[component]
fn Cell(caption: String, sample: Element) -> Element {
    rsx! {
        div {
            style: "flex: 1; min-width: 0; width: 100%; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; background: #24283b; border: 1px solid #414868; border-radius: 5px; padding: 1px 3px; box-sizing: border-box; overflow: hidden;",
            div {
                style: "display: flex; flex-direction: row; align-items: center; justify-content: center; gap: 3px; width: 100%; height: 100%; overflow: hidden;",
                {sample}
            }
            span { style: "font-size: 8px; line-height: 9px; color: #565f89;", "{caption}" }
        }
    }
}

/// A small flex box whose width is passed through verbatim.
#[component]
fn Blox(w: String) -> Element {
    rsx! {
        div {
            style: "height: 7px; width: {w}; background: #3b4261; border: 1px solid #565f89; border-radius: 2px; box-sizing: border-box;",
        }
    }
}

#[component]
fn Chip() -> Element {
    rsx! {
        div { style: "width: 5px; height: 5px; background: #f7768e; border-radius: 100%;" }
    }
}