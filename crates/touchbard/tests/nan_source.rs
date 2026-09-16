//! Regression test: blitz-paint must never emit non-finite geometry to the
//! paint scene, even for elements with `border-radius` and no border.
//!
//! Root cause (blitz-paint 0.2.1): `draw_border_edge` built and filled a border
//! edge shape for every element regardless of border width. With
//! `border-radius` set and `border-width: 0`, the corner math in
//! `start_angle` divides 0/0 and produces NaN, which Vello rejects with
//! `A path contains NaN, ignoring it.` — once per edge, per frame.
//!
//! Fixed in the vended fork (crates/blitz-paint) by skipping zero-width border
//! edges. This test records every fill/stroke at the PaintScene boundary and
//! asserts the geometry is finite across several animation frames, and that the
//! ball (the visible background circle) is still painted.

use anyrender::{PaintRef, PaintScene};
use dioxus::prelude::*;
use kurbo::{Affine, Rect, Shape, Stroke};
use peniko::{BlendMode, Color, Fill, FontData, StyleRef};
use touchbard::{TouchbardSystem, Viewport};

/// Records every fill/stroke/clip at the PaintScene boundary.
#[derive(Default, Debug)]
struct Rec {
    draws: usize,
    ball_circles: usize,
    ball_x: Vec<f64>,
    non_finite: Vec<(Affine, Vec<(String, f64, f64)>)>,
}

fn elt(e: kurbo::PathEl) -> (String, f64, f64) {
    match e {
        kurbo::PathEl::MoveTo(p) => ("M".into(), p.x, p.y),
        kurbo::PathEl::LineTo(p) => ("L".into(), p.x, p.y),
        kurbo::PathEl::QuadTo(p1, _p2) => ("Q".into(), p1.x, p1.y),
        kurbo::PathEl::CurveTo(p1, _p2, _p3) => ("C".into(), p1.x, p1.y),
        kurbo::PathEl::ClosePath => ("Z".into(), 0.0, 0.0),
    }
}

fn is_bad(xs: &[(String, f64, f64)]) -> bool {
    xs.iter().any(|(_, x, y)| !x.is_finite() || !y.is_finite())
}

fn is_circle(shape: &impl Shape) -> bool {
    let b = shape.bounding_box();
    let w = b.width();
    let h = b.height();
    w > 10.0 && (w - h).abs() < 0.5 && b.x0.abs() < 0.5 && b.y0.abs() < 0.5
}

impl Rec {
    fn record(&mut self, transform: Affine, shape: &impl Shape) {
        self.draws += 1;
        if !transform.as_coeffs().iter().all(|v| v.is_finite()) {
            self.non_finite
                .push((transform, vec![("TRANSFORM".into(), f64::NAN, 0.0)]));
            return;
        }
        if is_circle(shape) {
            self.ball_circles += 1;
            let c = transform.as_coeffs();
            self.ball_x.push(c[4] + c[0] * shape.bounding_box().x0);
        }
        let path = shape.to_path(0.1);
        let els: Vec<_> = path.elements().iter().map(|&e| elt(e)).collect();
        if is_bad(&els) {
            self.non_finite.push((transform, els));
        }
    }
}

impl PaintScene for Rec {
    fn reset(&mut self) {}
    fn push_layer(
        &mut self,
        _blend: impl Into<BlendMode>,
        _alpha: f32,
        t: Affine,
        clip: &impl Shape,
    ) {
        self.record(t, clip);
    }
    fn pop_layer(&mut self) {}
    fn stroke<'a>(
        &mut self,
        _style: &Stroke,
        t: Affine,
        _brush: impl Into<PaintRef<'a>>,
        _bt: Option<Affine>,
        shape: &impl Shape,
    ) {
        self.record(t, shape);
    }
    fn fill<'a>(
        &mut self,
        _style: Fill,
        t: Affine,
        _brush: impl Into<PaintRef<'a>>,
        _bt: Option<Affine>,
        shape: &impl Shape,
    ) {
        self.record(t, shape);
    }
    fn draw_glyphs<'a, 's: 'a>(
        &'s mut self,
        _font: &'a FontData,
        _font_size: f32,
        _hint: bool,
        _normalized_coords: &'a [anyrender::NormalizedCoord],
        _style: impl Into<StyleRef<'a>>,
        _brush: impl Into<PaintRef<'a>>,
        _brush_alpha: f32,
        _transform: Affine,
        _glyph_transform: Option<Affine>,
        _glyphs: impl Iterator<Item = anyrender::Glyph>,
    ) {
    }
    fn draw_box_shadow(
        &mut self,
        _transform: Affine,
        _rect: Rect,
        _brush: Color,
        _radius: f64,
        _std_dev: f64,
    ) {
    }
}

#[rustfmt::skip]
const STYLE: &str = r#"
    @keyframes circleSlide {
        from { transform: translateX(0px); }
        50%  { transform: translateX(1954px); }
        to   { transform: translateX(0px); }
    }
    .stage { width: 100%; height: 100%; display: flex; justify-content: center; align-items: flex-start; background: #0b0e14; overflow: hidden; }
    .ball { width: 22px; height: 22px; margin-left: 16px; background: #7aa2f7; border-radius: 50%; animation: circleSlide 3.6s linear infinite; }
"#;

fn ball_app() -> Element {
    rsx! {
        div { class: "stage", style { {STYLE} } div { class: "ball" } }
    }
}

fn system() -> TouchbardSystem {
    TouchbardSystem::new(
        ball_app,
        Viewport {
            width: 2008,
            height: 60,
            scale_factor: 1.0,
        },
    )
}

fn paint(sys: &mut TouchbardSystem, now: f64, scale: f64) -> Rec {
    let mut rec = Rec::default();
    sys.document.resolve(now);
    blitz_paint::paint_scene(&mut rec, &sys.document, scale, 2008, 60);
    rec
}

#[test]
fn ball_animation_emits_only_finite_geometry_and_still_paints() {
    let mut sys = system();
    sys.poll();

    // Sweep a full animation cycle (out and back) across several frames.
    let mut ball_x = Vec::new();
    for i in 0..12 {
        let now = (i as f64) / 12.0 * 3.6;
        let rec = paint(&mut sys, now, 1.0);
        assert!(
            rec.non_finite.is_empty(),
            "frame {i}: non-finite geometry emitted to the paint scene: {:?}",
            rec.non_finite
        );
        // The ball background circle must still be visible in every frame.
        assert!(rec.ball_circles > 0, "frame {i}: ball circle missing");
        assert!(rec.draws > 0, "frame {i}: scene empty");
        ball_x.push(rec.ball_x[0]);
    }
    // The animation must still play: the ball sweeps the full track (out and
    // back), so the observed positions must span roughly the travel distance.
    let travel = ball_x.iter().cloned().fold(f64::MIN, f64::max)
        - ball_x.iter().cloned().fold(f64::MAX, f64::min);
    assert!(
        travel > 1900.0,
        "animation no longer travels: max-min span {travel:.0}px in {ball_x:?}"
    );
}

#[test]
fn ball_animation_stays_clean_at_scale_two() {
    let mut sys = system();
    sys.poll();
    for i in 0..6 {
        let now = (i as f64) / 6.0 * (3.6 / 2.0);
        let rec = paint(&mut sys, now, 2.0);
        assert!(rec.non_finite.is_empty(), "frame {i} @scale 2: {rec:?}");
        assert!(rec.ball_circles > 0, "frame {i} @scale 2: circle missing");
    }
}

#[test]
fn border_edges_with_real_widths_stay_finite() {
    // A bordered, rounded box must still draw its border and be finite.
    fn bordered() -> Element {
        rsx! {
            div { style: "width:100%; height:100%; background:#0b0e14;",
                div { style: "width:120px; height:40px; border:2px solid #fff; border-radius:8px; background:#123;" }
            }
        }
    }
    let mut sys = TouchbardSystem::new(
        bordered,
        Viewport {
            width: 2008,
            height: 60,
            scale_factor: 1.0,
        },
    );
    sys.poll();
    let rec = paint(&mut sys, 0.0, 1.0);
    assert!(rec.non_finite.is_empty(), "bordered box: {rec:?}");
    assert!(rec.draws > 0);
}
