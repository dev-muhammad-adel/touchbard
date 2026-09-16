//! Characterization test for animated GIFs through the existing `<img>` path.
//! This intentionally does not implement GIF animation; it records what the
//! current DOM/resource/render pipeline actually does.

use dioxus::prelude::*;
use std::collections::BTreeMap;
use std::time::Duration;
use touchbard::{TouchbardSystem, Viewport};

const TWO_FRAME_GIF: &str = "data:image/gif;base64,R0lGODlhBAAEAIEAAP8AAAAAAAAAAAAAACH/C05FVFNDQVBFMi4wAwEAAAAh+QQACgAAACwAAAAABAAEAAAICQABCBxIsCCAgAAh+QQBCgABACwAAAAABAAEAIEA/wAAAAAAAAAAAAAICQABCBxIsCCAgAA7";

fn gif_app() -> Element {
    rsx! {
        div {
            style: "width: 16px; height: 8px; background: #000;",
            img {
                src: TWO_FRAME_GIF,
                style: "width: 4px; height: 4px;",
            }
        }
    }
}

fn count_color(data: &[u8], channel: usize) -> usize {
    data.chunks_exact(4)
        .filter(|pixel| pixel[channel] > 200 && pixel[(channel + 1) % 3] < 50)
        .count()
}

fn color_summary(data: &[u8]) -> String {
    let mut colors = BTreeMap::new();
    for pixel in data.chunks_exact(4) {
        *colors
            .entry([pixel[0], pixel[1], pixel[2], pixel[3]])
            .or_insert(0usize) += 1;
    }
    colors
        .into_iter()
        .rev()
        .take(8)
        .map(|(color, count)| format!("{color:?}:{count}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn report_animated_gif_behavior_through_img_pipeline() {
    let mut system = TouchbardSystem::new(
        gif_app,
        Viewport {
            width: 16,
            height: 8,
            scale_factor: 1.0,
        },
    );
    system.poll();
    let first = system.render();
    let redraw_after_first = system.needs_redraw();

    std::thread::sleep(Duration::from_millis(150));
    system.poll();
    let second = system.render();

    let differing_bytes = first
        .data
        .iter()
        .zip(&second.data)
        .filter(|(a, b)| a != b)
        .count();
    println!(
        "animated GIF: first_frame_bytes={} differing_bytes={} redraw_after_first={} red_pixels={} green_pixels={}",
        first.data.len(),
        differing_bytes,
        redraw_after_first,
        count_color(&first.data, 0),
        count_color(&second.data, 1),
    );
    println!("first_colors={}", color_summary(&first.data));
    println!("second_colors={}", color_summary(&second.data));

    assert!(
        first.data.iter().any(|&byte| byte != 0),
        "the GIF test must produce a non-empty rendered framebuffer"
    );
}
