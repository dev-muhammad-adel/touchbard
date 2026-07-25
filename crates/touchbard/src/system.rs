//! Core `TouchbardSystem`: owns the Dioxus+Blitz document and renders frames.

use blitz_dom::Document as _;
use blitz_paint::paint_scene;
use blitz_traits::events::UiEvent;
use blitz_traits::shell::{ColorScheme, Viewport as BlitzViewport};
use dioxus_core::VirtualDom;
use dioxus_native_dom::{DioxusDocument, DocumentConfig};
use touchbard_renderer::{CpuRenderer, Frame, FrameSource, PointerEvent, PointerEventKind};
use tracing::trace;

/// Viewport (framebuffer) configuration for a [`TouchbardSystem`].
///
/// This is the authoritative physical-pixel framebuffer size plus a scale
/// factor, produced by the selected backend at initialization (the preview's
/// `PreviewConfig` defaults/env today, future DRM connector discovery later).
/// The runtime creates the system at exactly this viewport; there is no
/// independent copy anywhere else.
pub use touchbard_renderer::Viewport;

/// The central runtime: a Dioxus `VirtualDom` integrated with a Blitz
/// `BaseDocument`, plus a CPU (Vello) render pipeline.
///
/// Operations:
///  1. [`TouchbardSystem::new`] creates the document from a Dioxus app function.
///  2. [`TouchbardSystem::poll`] flushes Dioxus mutations into Blitz.
///  3. [`TouchbardSystem::render`] rasterizes the Blitz document into an RGBA
///     [`Frame`] via AnyRender + Vello CPU (through `touchbard-renderer`).
///
/// Input arrives via [`TouchbardSystem::handle_pointer_event`] (converted to
/// Blitz `UiEvent`s) and is dispatched into the Dioxus runtime.
pub struct TouchbardSystem {
    /// The document, exposed so render backends and the shell can drive it.
    pub document: DioxusDocument,
    renderer: CpuRenderer,
    config: Viewport,
}

impl TouchbardSystem {
    /// Create a new system from a Dioxus app function.
    pub fn new(app: fn() -> dioxus_core::Element, config: Viewport) -> Self {
        let vdom = VirtualDom::new(app);

        let doc_config = DocumentConfig {
            viewport: Some(BlitzViewport::new(
                config.width,
                config.height,
                config.scale_factor as f32,
                ColorScheme::Dark,
            )),
            ..Default::default()
        };

        let mut document = DioxusDocument::new(vdom, doc_config);

        // Blitz's DEFAULT_CSS leaves the html/body chain content-sized; make the
        // root fill the viewport (a hyper-strip runtime always fills its screen).
        document.add_user_agent_stylesheet(
            "html, body, main { margin: 0; width: 100vw; height: 100vh; }",
        );

        let mut system = Self {
            document,
            renderer: CpuRenderer::new(config.width, config.height),
            config,
        };
        system.document.initial_build();
        system
    }

    /// Poll the VirtualDom for pending work and flush mutations to the Blitz
    /// document. Returns `true` if there was any work to flush.
    pub fn poll(&mut self) -> bool {
        self.document.poll(None)
    }

    /// Whether the document needs a redraw (CSS animations, `<canvas>`, etc).
    pub fn needs_redraw(&self) -> bool {
        self.document.is_animating()
    }

    /// Handle a pointer event from any backend (preview WebSocket, touch device).
    ///
    /// Coordinates are in logical (CSS) pixels.
    pub fn handle_pointer_event(&mut self, event: PointerEvent) {
        // Blitz synthesizes `click` against the *hovered* node, and it only
        // updates hover on MouseMove. Some real input streams (browser pointer
        // coalescing, touch release) deliver down/up without a preceding move at
        // the release point, leaving a stale hover that mis-routes the click.
        // Make the release position authoritative: re-hover there before the up.
        if event.kind == PointerEventKind::Up {
            let mut hover = event.clone();
            hover.kind = PointerEventKind::Move;
            let ui: UiEvent = hover.to_ui_event();
            trace!(?ui, "re-hover before up");
            self.document.handle_ui_event(ui);
        }
        let ui_event: UiEvent = event.to_ui_event();
        trace!(?ui_event, "handle_pointer_event");
        self.document.handle_ui_event(ui_event);
    }

    /// Render the current document state to an RGBA framebuffer.
    ///
    /// This runs the full pipeline: Blitz paint → AnyRender commands →
    /// Vello CPU rasterization into the framebuffer.
    pub fn render(&mut self) -> Frame {
        let width = self.config.width;
        let height = self.config.height;
        let scale = self.config.scale_factor;

        // blitz-paint requires styles and layout to be resolved. `resolve()`
        // restyles the tree and relayouts it (the timestamp drives CSS
        // animations; 0.0 is fine for static UIs).
        self.document.resolve(0.0);

        self.renderer.render(
            |scene| {
                paint_scene(scene, &self.document, scale, width, height);
            },
            width,
            height,
        )
    }

    /// Resize the viewport. The Dioxus document relayouts to the new size.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.renderer.resize(width, height);
        self.document.set_viewport(BlitzViewport::new(
            width,
            height,
            self.config.scale_factor as f32,
            ColorScheme::Dark,
        ));
    }

    /// Get the current viewport configuration.
    pub fn config(&self) -> &Viewport {
        &self.config
    }
}

/// The shell is itself a [`FrameSource`], so backends (preview WebSocket now,
/// DRM later) can drive it without knowing about Dioxus or Blitz. The physical
/// viewport is owned by the backend (see [`Backend::initialize`](touchbard_renderer::Backend::initialize));
/// the shell only renders frames and accepts input.
impl FrameSource for TouchbardSystem {
    fn handle_pointer_event(&mut self, event: PointerEvent) {
        TouchbardSystem::handle_pointer_event(self, event);
    }

    fn poll_and_render(&mut self) -> Frame {
        self.poll();
        self.render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::prelude::*;
    use std::sync::atomic::{AtomicI32, Ordering};
    use touchbard_renderer::PointerButton;
    use touchbard_renderer::PointerEvent as UiPointerEvent;

    /// Mirror of the `counter` signal used to assert click direction.
    static COUNTER: AtomicI32 = AtomicI32::new(0);

    fn test_app() -> Element {
        rsx! {
            div {
                "Hello Touchbard"
            }
        }
    }

    /// The control-center widget set. Layout must match the real app so the
    /// hard-coded button centers in `test_click_direction` stay valid.
    fn counter_app() -> Element {
        let mut counter = use_signal(|| 0i32);
        rsx! {
            div {
                style: "width: 100%; height: 100%; display: flex; flex-direction: row; align-items: center; justify-content: center; gap: 8px; background: #1a1b26; color: #c0caf5; font-family: system-ui, sans-serif; font-size: 12px; overflow: hidden;",
                div { "Touchbard Demo" }
                div {
                    style: "width: 100px; text-align: center; background: #24283b; border-radius: 3px; border: 1px solid #414868;",
                    "Count: {counter}"
                }
                button {
                    style: "color: #f7768e; padding: 0 12px; background: #24283b; border: 1px solid #414868; border-radius: 3px; cursor: pointer;",
                    onclick: move |_| { counter -= 1; COUNTER.fetch_sub(1, Ordering::SeqCst); },
                    "-"
                }
                button {
                    style: "color: #9ece6a; padding: 0 12px; background: #24283b; border: 1px solid #414868; border-radius: 3px; cursor: pointer;",
                    onclick: move |_| { counter += 1; COUNTER.fetch_add(1, Ordering::SeqCst); },
                    "+"
                }
            }
        }
    }

    fn preview_config() -> Viewport {
        Viewport {
            width: 2008,
            height: 60,
            scale_factor: 2.0,
        }
    }

    /// Full down/up with a preceding move (normal click).
    fn click(sys: &mut TouchbardSystem, x: f32, y: f32) {
        for kind in [
            PointerEventKind::Move,
            PointerEventKind::Down,
            PointerEventKind::Up,
        ] {
            sys.handle_pointer_event(UiPointerEvent {
                x,
                y,
                button: PointerButton::Main,
                buttons: if kind == PointerEventKind::Up { 0 } else { 1 },
                kind,
            });
        }
        sys.poll();
    }

    /// Release without a preceding move (stale hover): the click must still
    /// route to the node under the release position.
    fn click_stale(sys: &mut TouchbardSystem, x: f32, y: f32) {
        for kind in [PointerEventKind::Down, PointerEventKind::Up] {
            sys.handle_pointer_event(UiPointerEvent {
                x,
                y,
                button: PointerButton::Main,
                buttons: if kind == PointerEventKind::Up { 0 } else { 1 },
                kind,
            });
        }
        sys.poll();
    }

    #[test]
    fn test_system_creation() {
        let config = Viewport {
            width: 100,
            height: 30,
            scale_factor: 1.0,
        };
        let system = TouchbardSystem::new(test_app, config);
        assert_eq!(system.config().width, 100);
        assert_eq!(system.config().height, 30);
        assert_eq!(system.config().logical_width(), 100.0);
        assert_eq!(system.config().logical_height(), 30.0);
    }

    #[test]
    fn test_render_produces_frame() {
        let config = Viewport {
            width: 100,
            height: 30,
            scale_factor: 1.0,
        };
        let mut system = TouchbardSystem::new(test_app, config);
        system.poll();
        let frame = system.render();
        assert_eq!(frame.width, 100);
        assert_eq!(frame.height, 30);
        assert_eq!(frame.data.len(), 100 * 30 * 4);
    }

    #[test]
    fn test_resize_affects_render() {
        let config = Viewport {
            width: 100,
            height: 30,
            scale_factor: 1.0,
        };
        let mut system = TouchbardSystem::new(test_app, config);
        system.resize(200, 60);
        system.poll();
        let frame = system.render();
        assert_eq!(frame.width, 200);
        assert_eq!(frame.height, 60);
        assert_eq!(frame.data.len(), 200 * 60 * 4);
    }

    /// Regression suite relocated from the old playground snapshot example:
    /// exercises both buttons through the real input pipeline (Blitz hit-test →
    /// Dioxus handlers) and asserts each moves the counter in the correct
    /// direction, then checks the framebuffer changed and that a release
    /// without a preceding move still targets the node under it.
    #[test]
    fn test_click_direction_and_stale_hover() {
        COUNTER.store(0, Ordering::SeqCst);

        let mut sys = TouchbardSystem::new(counter_app, preview_config());
        sys.poll();
        let before = sys.render();

        // "-"-button center and the gap to "+" (logical px, measured earlier).
        let (b_minus, b_plus) = (581.5_f32, 621.7_f32);

        for &(x, expect) in &[
            (b_minus, -1),
            (b_minus, -2),
            (b_minus, -3),
            (b_plus, -2),
            (b_plus, -1),
            (b_plus, 0),
            (b_plus, 1),
        ] {
            click(&mut sys, x, 15.0);
            let got = COUNTER.load(Ordering::SeqCst);
            assert_eq!(got, expect, "button at x={x:.1} must move counter to {expect}");
        }

        let after = sys.render();
        let diff = before
            .data
            .iter()
            .zip(after.data.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(diff > 0, "clicking buttons must change the framebuffer");

        // Stale hover: release at "-" while hover is still on "+" from the
        // previous click in the loop; the re-hover-on-up must route it to "-".
        let start = COUNTER.load(Ordering::SeqCst);
        click_stale(&mut sys, b_minus, 15.0);
        let got = COUNTER.load(Ordering::SeqCst);
        assert_eq!(
            got,
            start - 1,
            "release must target the node under it (stale-hover regression)"
        );
    }
}