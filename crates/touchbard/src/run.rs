//! Entry point: [`run`] performs the backend lifecycle around an app component.

use std::cell::RefCell;
use std::rc::Rc;

use crate::system::TouchbardSystem;
use crate::TouchbardConfig;
use touchbard_renderer::{FrameSource, Viewport};
use tracing::info;

/// A Dioxus app component: `fn() -> Element`.
pub type AppFn = fn() -> dioxus_core::Element;

/// Errors produced while starting or running a backend.
#[derive(Debug)]
pub enum RunError {
    /// The backend failed to initialize (e.g. DRM not implemented yet).
    Initialize(Box<dyn std::error::Error + Send + Sync>),
    /// The backend failed while running.
    Run(Box<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Initialize(e) => write!(f, "backend initialization failed: {e}"),
            RunError::Run(e) => write!(f, "backend error: {e}"),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunError::Initialize(e) | RunError::Run(e) => Some(&**e),
        }
    }
}

/// Run an app on the configured backend.
///
/// The whole pipeline (Dioxus → Blitz → AnyRender → Vello CPU) is bound to a
/// single thread because Blitz documents are not `Send`. `run` performs the
/// backend lifecycle in order:
///
/// ```text
/// backend.initialize()  →  authoritative Viewport  →  TouchbardSystem  →  backend.run(source)
/// ```
///
/// The backend discovers its display first (for the preview, its configured
/// size/scale; for DRM, eventually the connected connector's mode) and only
/// then is the UI system created at exactly that viewport. The backend then
/// owns its event loop and presentation until the UI exits. Initialization
/// failures (e.g. DRM not implemented) abort cleanly with [`RunError::Initialize`].
pub fn run(app: AppFn, config: TouchbardConfig) -> Result<(), RunError> {
    // Best-effort: init if the application has not already configured logging.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let mut backend = config.backend;

    // Phase 1: backend initialization produces the authoritative viewport.
    let viewport: Viewport = backend.initialize().map_err(RunError::Initialize)?;

    // Phase 2: create the UI system at exactly that viewport.
    let system = Rc::new(RefCell::new(TouchbardSystem::new(app, viewport)));

    // Render once to prove the pipeline works, before ceding control to the
    // backend's own event loop.
    {
        let mut sys = system.borrow_mut();
        let frame = sys.poll_and_render();
        info!(
            "initial render: {}x{} ({} bytes), non-zero bytes: {}",
            frame.width,
            frame.height,
            frame.byte_len(),
            frame.data.iter().filter(|&&b| b != 0).count()
        );
    }

    // Phase 3: the backend drives the runtime.
    backend.run(system).map_err(RunError::Run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::prelude::*;
    use touchbard_renderer::{Backend, FrameSource, Viewport};

    fn empty_app() -> Element {
        rsx! { div {} }
    }

    /// Records the lifecycle and asserts that, at `run`, the UI system already
    /// exists at the viewport discovered during `initialize`.
    struct LifecycleBackend {
        log: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Backend for LifecycleBackend {
        fn initialize(&mut self) -> Result<Viewport, Box<dyn std::error::Error + Send + Sync>> {
            self.log.borrow_mut().push("initialize");
            Ok(Viewport {
                width: 100,
                height: 30,
                scale_factor: 1.0,
            })
        }

        fn run(
            &mut self,
            source: Rc<RefCell<dyn FrameSource>>,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.log.borrow_mut().push("run");
            let frame = source.borrow_mut().poll_and_render();
            assert_eq!(frame.width, 100);
            assert_eq!(frame.height, 30);
            assert_eq!(frame.data.len(), 100 * 30 * 4);
            Ok(())
        }
    }

    /// `run` must perform: backend initialize → create TouchbardSystem at the
    /// returned viewport → hand it to `backend.run`, in that order, with no
    /// backend lifecycle details leaked to the caller.
    #[test]
    fn run_initializes_backend_then_system_then_hands_over() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let backend = LifecycleBackend { log: Rc::clone(&log) };
        let config = TouchbardConfig {
            backend: Box::new(backend),
        };
        run(empty_app, config).expect("run succeeds");
        assert_eq!(*log.borrow(), ["initialize", "run"]);
    }

    /// Initialization errors must surface cleanly (no panic), preserving the
    /// not-implemented DRM pattern.
    #[test]
    fn run_propagates_backend_initialization_errors() {
        struct FailingBackend;

        impl Backend for FailingBackend {
            fn initialize(
                &mut self,
            ) -> Result<Viewport, Box<dyn std::error::Error + Send + Sync>> {
                Err("the DRM/KMS backend is not implemented yet".into())
            }

            fn run(
                &mut self,
                _source: Rc<RefCell<dyn FrameSource>>,
            ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                unreachable!("run is never reached when initialize fails")
            }
        }

        let config = TouchbardConfig {
            backend: Box::new(FailingBackend),
        };
        match run(empty_app, config) {
            Err(RunError::Initialize(e)) => {
                assert!(e.to_string().contains("not implemented"));
            }
            other => panic!("expected Initialize error, got {other:?}"),
        }
    }
}