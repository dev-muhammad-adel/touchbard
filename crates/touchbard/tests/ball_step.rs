//! Regression: the sliding ball must advance by *constant* pixel steps per
//! presented frame, and each step must track the real time between presents.
//!
//! The visible shake comes from consecutive presents moving the animation by
//! unequal amounts (double-cadence deferrals, drifting wake grids). These
//! tests pin the three runtime invariants that keep motion uniform:
//!
//! - the presentation cadence is *exactly* 1/60 s (a 16 ms period drifts
//!   0.66 ms per frame against a 60 Hz refresh and beats against it);
//! - every cadence wait yields a present while a document animates (a
//!   deferral drops a present and makes the next step jump);
//! - the animation clock equals real elapsed time at each present (a stepped
//!   or quantized clock would warp the painted steps).

use dioxus::prelude::*;
use touchbard::{TouchbardSystem, Viewport};
use touchbard_renderer::frame_source::FRAME_CADENCE;

#[rustfmt::skip]
const STYLE: &str = r#"
    @keyframes circleSlide {
        from { transform: translateX(0px); }
        50%  { transform: translateX(1954px); }
        to   { transform: translateX(0px); }
    }
    .stage { width: 100%; height: 100%; display: flex; justify-content: center; align-items: flex-start; background: #0b0e14; overflow: hidden; }
    .ball { width: 22px; height: 22px; margin-left: 16px; background: #7aa2f7; border-radius: 50%; animation: circleSlide 60s linear infinite; }
"#;

fn ball_app() -> Element {
    rsx! {
        div { class: "stage", style { {STYLE} } div { class: "ball" } }
    }
}

/// The ball's linear speed from the keyframes: 1954 px over 30 s.
const BALL_SPEED_PX_PER_S: f64 = 1954.0 / 30.0; // 65.13

/// The ball's horizontal center in a frame: the centroid of ball-colored
/// pixels, so the measure tracks the ball's subpixel position smoothly instead
/// of quantizing to whole columns. The 22 px ball sits at the top of the
/// flex-start stage (rows 0..=21); it is opaque `#7aa2f7` on a dark
/// background, so any blue-dominant pixel there is inside it. Returns `None`
/// when the ball is missing from the frame.
fn ball_center_x(frame: &touchbard::Frame) -> Option<f64> {
    let width = frame.width;
    let rows = frame.height.min(22);
    if rows < 8 {
        return None;
    }
    let mut sum = 0.0;
    let mut count = 0.0;
    for row in 0..rows {
        let row_off = row as usize * frame.stride;
        for x in 0..width {
            let p = row_off + x as usize * 4;
            let (r, g, b) =
                (frame.data[p] as i32, frame.data[p + 1] as i32, frame.data[p + 2] as i32);
            // Blue-dominant ball color (background is near-black, blends included).
            if b - r > 30 && b - g > 30 && b > 90 {
                sum += x as f64;
                count += 1.0;
            }
        }
    }
    if count > 0.0 {
        Some(sum / count)
    } else {
        None
    }
}

/// The cadence must be the exact 60 Hz refresh period. A rounded `16 ms`
/// presents 0.66 ms early each frame, sliding the present phase across the
/// scan and making moving content shake intermittently.
#[test]
fn frame_cadence_is_exactly_one_sixtieth_of_a_second() {
    assert_eq!(FRAME_CADENCE.as_nanos(), 1_000_000_000 / 60);
}

/// Mirror the DRM loop: wait exactly the cadence deadline, ask for a frame,
/// present it. Measure the ball's painted X and the wall time per present, then
/// check that (a) every wait produces a present while the document animates,
/// (b) the painted step matches the real elapsed time at the ball's linear
/// speed — the animation clock must track the wall, never warp — and (c) the
/// steps are uniform, because render starts land one cadence apart.
#[test]
fn the_ball_steps_in_lockstep_with_elapsed_time() {
    let mut sys = TouchbardSystem::new(
        ball_app,
        Viewport {
            width: 2008,
            height: 60,
            scale_factor: 1.0,
        },
    );
    sys.frame(None); // warm the document

    let mut xs = Vec::new();
    let mut walls = Vec::new();
    let mut t_prev = std::time::Instant::now();
    let mut deferred = 0usize;
    for _ in 0..60 {
        std::thread::sleep(
            sys.frame_deadline().unwrap_or(FRAME_CADENCE), // like a backend's wait
        );
        let t0 = std::time::Instant::now();
        let frame = sys.frame(None);
        let wall = t0.duration_since(t_prev).as_secs_f64();
        t_prev = t0;
        let frame = match frame {
            Some(f) => f,
            None => {
                deferred += 1;
                continue;
            }
        };
        if let Some(x) = ball_center_x(&frame) {
            xs.push(x);
            walls.push(wall);
        }
    }
    assert_eq!(
        deferred, 0,
        "every cadence wait must present while animating; a deferral drops a \
         frame and makes the next step jump"
    );
    assert!(xs.len() >= 55, "expected ~a present per cadence wait, got {}", xs.len());

    // Skip warm-up presents (compile/paint cache settles after a few frames);
    // `walls` and `deltas` align so deltas[i] was painted over walls[i].
    let xs = &xs[4..];
    let walls = &walls[5..];
    let deltas: Vec<f64> = xs.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(deltas.len(), walls.len());
    assert!(deltas.iter().all(|d| *d > 0.0), "the ball must only move forward");

    for (i, (d, wall)) in deltas.iter().zip(walls).enumerate() {
        let expected = BALL_SPEED_PX_PER_S * wall;
        let ratio = d / expected;
        assert!(
            (0.7..=1.3).contains(&ratio),
            "present {i}: painted step {d:.3}px must track {expected:.3}px from \
             wall time (ratio {ratio:.2}); the animation clock warped"
        );
    }

    // Tight step uniformity is a presentation-quality check; the debug renderer
    // rasterizes slowly and unevenly (wall-time jitter), so only enforce the
    // exact-step bound in release where the cadence actually governs.
    if cfg!(not(debug_assertions)) {
        let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
        let max_dev = deltas.iter().map(|d| (d - mean).abs()).fold(0.0, f64::max);
        eprintln!(
            "BALL_STEP (release) n={} mean={mean:.3}px max_dev={max_dev:.3}px ({:.1}% of mean)",
            deltas.len(),
            max_dev / mean * 100.0
        );
        assert!(
            max_dev <= 0.25 * mean,
            "steps must be uniform: mean {mean:.3}px, worst deviation {max_dev:.3}px"
        );
    } else {
        let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
        let max_dev = deltas.iter().map(|d| (d - mean).abs()).fold(0.0, f64::max);
        eprintln!(
            "BALL_STEP (debug, diagnostic) n={} mean={mean:.3}px max_dev={max_dev:.3}px \
             ({:.1}%); debug raster cost variance, see release for the cadence check",
            deltas.len(),
            max_dev / mean * 100.0
        );
    }
}