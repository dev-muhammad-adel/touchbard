//! `/showcase/motion` — real CSS animations.
//!
//! Each animation animates a property that was empirically verified to advance
//! through the full production pipeline (Dioxus → Blitz → AnyRender → Vello)
//! in `touchbard/tests/css_animations.rs`: `opacity`, `transform:
//! translateX(..)`, `width` and `background-color`. No `setInterval`, no
//! blocking loops: the experiment is driven purely by the render loop advancing
//! the document's animation clock.
//!
//! Caveat shown honestly below: the browser/preview backend only re-renders on
//! input, so these animations are only continuously visible on the DRM backend
//! (or while the pointer is over an animated area in the preview).

use dioxus::prelude::*;

#[rustfmt::skip]
const MOTION_STYLE: &str = r#"
    @keyframes demoPulse { 0% { opacity: 1; } 50% { opacity: 0.2; } 100% { opacity: 1; } }
    @keyframes demoMarquee { from { transform: translateX(0px); } to { transform: translateX(-30px); } }
    @keyframes demoGrow { from { width: 8px; } to { width: 46px; } }
    @keyframes demoSweep { from { background-color: #f7768e; } to { background-color: #7aa2f7; } }
    .c-pulse { animation: demoPulse 1.4s ease-in-out infinite; }
    .c-marquee { animation: demoMarquee 1.6s linear infinite; }
    .c-grow { animation: demoGrow 1.8s ease-in-out infinite; }
    .c-sweep { animation: demoSweep 1.2s linear infinite; }
"#;

#[component]
pub fn Page() -> Element {
    rsx! {
        div {
            style: "width: 100%; height: 100%; display: flex; flex-direction: row; align-items: stretch; gap: 5px; padding: 1px 4px; box-sizing: border-box;",
            style { {MOTION_STYLE} }

            Cell {
                caption: "opacity pulse",
                sample: rsx! {
                    div { class: "c-pulse", style: "width: 9px; height: 9px; background: #f7768e; border-radius: 100%;" }
                },
            }
            Cell {
                caption: "marquee · translateX",
                sample: rsx! {
                    div {
                        style: "width: 52px; height: 9px; overflow: hidden; border: 1px solid #414868; border-radius: 3px; background: #16161e; display: flex; flex-direction: row; align-items: center;",
                        div {
                            class: "c-marquee",
                            style: "white-space: nowrap; font-size: 8px; line-height: 9px; color: #9ece6a;",
                            "TOUCH BAR BAR BAR "
                        },
                    },
                },
            }
            Cell {
                caption: "width grows",
                sample: rsx! {
                    div {
                        style: "width: 50px; height: 8px; padding: 1px; border: 1px solid #414868; border-radius: 3px; background: #16161e; display: flex; flex-direction: row; align-items: center; box-sizing: border-box;",
                        div { class: "c-grow", style: "height: 100%; background: #7aa2f7; border-radius: 2px;" }
                    },
                },
            }
            Cell {
                caption: "color sweep",
                sample: rsx! {
                    div { class: "c-sweep", style: "width: 26px; height: 8px; background-color: #f7768e; border-radius: 3px;" },
                },
            }
            Cell {
                caption: "verified",
                sample: rsx! {
                    span { style: "font-size: 8px; line-height: 9px; color: #565f89; text-align: center;", "opacity · translateX · width · background-color (see css_animations.rs)" }
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