//! Regression test: real CSS animations advance through the production render
//! pipeline (Dioxus → Blitz → AnyRender → Vello CPU) and the system reports
//! that redraws are needed while they run.
//!
//! This documents the animation capability the Control Center "Motion" demo
//! relies on. The animated properties below (`background-color`, `opacity`,
//! `width`, `transform: translateX`) are the only ones the showcase claims.
//!
//! It exercises [`TouchbardSystem`] directly — the same object the DRM and
//! preview backends drive through `frame()` on an event-driven loop — so a
//! change that breaks animation (e.g. the document's animation clock no longer
//! advancing) fails here first.

use dioxus::prelude::*;
use std::time::Duration;
use touchbard::{TouchbardSystem, Viewport};

#[rustfmt::skip]
const ANIM_STYLE: &str = r#"
    @keyframes demoPulse { from { background-color: #ff0000; } to { background-color: #00ffff; } }
    @keyframes demoMove { from { transform: translateX(0px); } to { transform: translateX(24px); } }
    @keyframes demoGrow { from { width: 10px; } to { width: 60px; } }
    @keyframes demoFade { from { opacity: 1.0; } to { opacity: 0.2; } }
    .c-pulse { animation: demoPulse 1s linear infinite; }
    .c-move { animation: demoMove 1s linear infinite; }
    .c-grow { animation: demoGrow 1s linear infinite; }
    .c-fade { animation: demoFade 1s linear infinite; }
"#;

#[rustfmt::skip]
fn animated_app() -> Element {
    rsx! {
        div {
            style: "width:100%; height:100%; background:#000;",
            style { {ANIM_STYLE} }
            div { class: "c-pulse", style: "width: 60px; height: 8px; background-color: #ff0000;" }
            div { class: "c-move", style: "width: 8px; height: 8px; background-color: #00ff00;" }
            div { class: "c-grow", style: "height: 8px; background-color: #0000ff;" }
            div { class: "c-fade", style: "width: 30px; height: 8px; background-color: #ffff00;" }
        }
    }
}

#[rustfmt::skip]
fn static_app() -> Element {
    rsx! {
        div { style: "width:100%; height:100%; background:#000;", div { style: "width: 40px; height: 8px; background-color: #888888;" } }
    }
}

fn system(app: fn() -> Element) -> TouchbardSystem {
    TouchbardSystem::new(
        app,
        Viewport {
            width: 2008,
            height: 60,
            scale_factor: 2.0,
        },
    )
}

fn diff(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

#[test]
fn css_animations_advance_frames_and_flag_redraw() {
    let mut sys = system(animated_app);
    sys.poll();

    let f0 = sys.render();
    assert!(
        sys.needs_redraw(),
        "a CSS animation element must make the document report it is animating"
    );

    std::thread::sleep(Duration::from_millis(250));
    sys.poll();
    let f1 = sys.render();

    std::thread::sleep(Duration::from_millis(250));
    sys.poll();
    let f2 = sys.render();

    let d01 = diff(&f0.data, &f1.data);
    let d12 = diff(&f1.data, &f2.data);
    let d02 = diff(&f0.data, &f2.data);
    assert!(
        d01 > 0,
        "frames 0 and 1 must differ (animation progress): {d01}"
    );
    assert!(
        d12 > 0,
        "frames 1 and 2 must differ (animation progress): {d12}"
    );
    assert!(
        d02 > 0,
        "frames 0 and 2 must differ (animation really moved): {d02}"
    );
}

#[test]
fn static_page_needs_no_redraw() {
    let mut sys = system(static_app);
    sys.poll();
    sys.render();

    assert!(
        !sys.needs_redraw(),
        "a page without CSS animation must not request redraws"
    );
}
