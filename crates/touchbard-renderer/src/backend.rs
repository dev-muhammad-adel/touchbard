//! The backend boundary shared by all display backends.

use std::cell::RefCell;
use std::rc::Rc;

use crate::FrameSource;

/// Physical display/viewport properties of a display backend.
///
/// Produced once by [`Backend::initialize`] and used by the runtime to create
/// the UI system at the correct size/scale. This is the single authoritative
/// viewport for a runtime: it comes from backend initialization, and backends
/// must not expose their own independent size afterwards.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// Scale factor (physical pixels per logical pixel).
    pub scale_factor: f64,
}

impl Viewport {
    /// Logical (CSS pixel) width.
    pub fn logical_width(&self) -> f32 {
        self.width as f32 / self.scale_factor as f32
    }

    /// Logical (CSS pixel) height.
    pub fn logical_height(&self) -> f32 {
        self.height as f32 / self.scale_factor as f32
    }
}

/// A display backend that drives a [`FrameSource`] (the app runtime).
///
/// A backend owns everything specific to its output medium:
/// - [`Backend::initialize`] discovers/probes its display (for the preview that
///   is the configured/default framebuffer size; for DRM it will be the
///   connected connector's mode) and returns the physical [`Viewport`] the UI
///   system must be created at,
/// - [`Backend::run`] owns the backend's event loop and thread (e.g. a
///   current-thread Tokio runtime for the WebSocket preview), which feeds
///   pointer input into the runtime and presents the frames it produces.
///
/// The two sides meet only at this boundary: the runtime
/// ([`crate::FrameSource`] impl, in practice `TouchbardSystem`) knows nothing
/// about a backend, and a backend knows nothing about Dioxus or Blitz.
///
/// Lifecycle: `initialize` → create the UI system at the returned
/// [`Viewport`] → `run`. Initialization failures (e.g. DRM not implemented)
/// are propagated as errors, never panics and never fake viewports.
///
/// Implemented by the concrete backend crates:
/// - `touchbard-preview`: browser canvas over the WebSocket preview protocol,
/// - `touchbard-drm`: DRM/KMS output (scaffolded, not yet implemented).
pub trait Backend {
    /// Initialize the backend, returning the physical [`Viewport`] the UI must
    /// target.
    ///
    /// Called once, before the UI system is built. A backend that needs to
    /// discover its display (a connector, a window, a configured size) does so
    /// here and retains its own context for [`Backend::run`]. On failure the
    /// runtime aborts cleanly with the returned error.
    fn initialize(&mut self) -> Result<Viewport, Box<dyn std::error::Error + Send + Sync>>;

    /// Drive `source` for the lifetime of the UI.
    ///
    /// `source` is the shared [`FrameSource`] (`Rc<RefCell<_>>`) and must stay
    /// on the calling thread: Blitz documents are not `Send`, so the whole
    /// pipeline runs single-threaded. The backend is responsible for presenting
    /// frames from `source.poll_and_render()` and dispatching its input events
    /// (converted to logical-pixel [`PointerEvent`](crate::PointerEvent)s) via
    /// `source.handle_pointer_event()`.
    fn run(
        &mut self,
        source: Rc<RefCell<dyn FrameSource>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummySource;

    impl FrameSource for DummySource {
        fn handle_pointer_event(&mut self, _event: crate::PointerEvent) {}

        fn poll_and_render(&mut self) -> crate::Frame {
            crate::Frame::new(0, 0, crate::PixelFormat::Rgba8)
        }
    }

    /// Records the lifecycle order: initialize must happen before run, and the
    /// viewport discovered at initialize must be the viewport used by the
    /// running backend.
    struct ProbeBackend {
        log: Vec<&'static str>,
        viewport: Viewport,
    }

    impl Default for ProbeBackend {
        fn default() -> Self {
            Self {
                log: Vec::new(),
                viewport: Viewport {
                    width: 0,
                    height: 0,
                    scale_factor: 1.0,
                },
            }
        }
    }

    impl Backend for ProbeBackend {
        fn initialize(&mut self) -> Result<Viewport, Box<dyn std::error::Error + Send + Sync>> {
            self.log.push("initialize");
            self.viewport = Viewport {
                width: 2008,
                height: 60,
                scale_factor: 2.0,
            };
            Ok(self.viewport)
        }

        fn run(
            &mut self,
            _source: Rc<RefCell<dyn FrameSource>>,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.log.push("run");
            Ok(())
        }
    }

    /// The boundary must work behind a `Box<dyn Backend>`: core owns the trait
    /// object, calls `initialize` for the authoritative viewport, then hands the
    /// runtime to `run`.
    #[test]
    fn backend_initialize_feed_viewport_then_run() {
        let mut backend: Box<dyn Backend> = Box::new(ProbeBackend::default());

        let viewport = backend.initialize().expect("probe initializes");
        assert_eq!(viewport.width, 2008);
        assert_eq!(viewport.height, 60);
        assert_eq!(viewport.scale_factor, 2.0);

        let _source: Rc<RefCell<dyn FrameSource>> = Rc::new(RefCell::new(DummySource));
        backend.run(_source).expect("probe run succeeds");
    }

    /// A backend initializes exactly once, before it runs, and keeps its own
    /// state behind the trait.
    #[test]
    fn backend_lifecycle_is_initialize_before_run() {
        let mut backend = ProbeBackend::default();
        let source: Rc<RefCell<dyn FrameSource>> = Rc::new(RefCell::new(DummySource));
        let viewport = backend.initialize().expect("probe initializes");
        assert_eq!(backend.log, ["initialize"]);
        assert_eq!(viewport, backend.viewport);

        backend.run(Rc::clone(&source)).expect("probe run succeeds");
        assert_eq!(backend.log, ["initialize", "run"]);
    }

    #[test]
    fn viewport_logical_helpers() {
        let viewport = Viewport {
            width: 2008,
            height: 60,
            scale_factor: 2.0,
        };
        assert_eq!(viewport.logical_width(), 1004.0);
        assert_eq!(viewport.logical_height(), 30.0);
    }
}