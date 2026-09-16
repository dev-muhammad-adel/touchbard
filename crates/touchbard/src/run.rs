//! Entry point: [`run`] performs the backend lifecycle around an app component.

use std::cell::RefCell;
use std::rc::Rc;

use crate::system::TouchbardSystem;
use crate::TouchbardConfig;
use touchbard_renderer::Viewport;
use tracing::info;

/// A Dioxus app component: `fn() -> Element`.
pub type AppFn = fn() -> dioxus_core::Element;

/// Errors produced while starting or running a backend.
#[derive(Debug)]
pub enum RunError {
    /// The backend failed to initialize.
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
/// size/scale; for DRM, the connected connector's mode) and only then is the
/// UI system created at exactly that viewport. The backend then owns its event
/// loop and presentation until the UI exits. Initialization failures abort
/// cleanly with [`RunError::Initialize`].
pub fn run(app: AppFn, config: TouchbardConfig) -> Result<(), RunError> {
    // Best-effort: init if the application has not already configured logging.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let mut backend = config.backend;

    // Backend initialization produces the authoritative viewport.
    let viewport: Viewport = backend.initialize().map_err(RunError::Initialize)?;

    // Create the UI system at exactly that viewport.
    let system = Rc::new(RefCell::new(TouchbardSystem::new(app, viewport)));

    // Render one frame up front so the initial render log carries real data,
    // before the backend takes over its own event loop. No host wake is armed
    // here: the backend registers its own waker when it starts driving. This
    // pre-render does not consume the backend's initial present — the first
    // host-armed `frame` still renders.
    {
        let mut sys = system.borrow_mut();
        if let Some(frame) = sys.frame(None) {
            info!(
                "initial render: {}x{} ({} bytes), non-zero bytes: {}",
                frame.width,
                frame.height,
                frame.byte_len(),
                frame.data.iter().filter(|&&b| b != 0).count()
            );
        }
    }

    // The backend drives the runtime.
    backend.run(system).map_err(RunError::Run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::prelude::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Wake, Waker};
    use touchbard_renderer::{Backend, FrameSource, Viewport};

    fn empty_app() -> Element {
        rsx! { div {} }
    }

    /// Records the lifecycle and asserts that, at `run`, the runtime handed
    /// over is consistent: `run()` already rendered the initial frame, so the
    /// empty app reports no animation and no further scheduling step produces
    /// a frame.
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
            // `run()` rendered the initial frame before handing the runtime
            // over, so the backend finds the empty app idle here - and a
            // further scheduling step must not produce a frame.
            assert!(
                !source.borrow().needs_redraw(),
                "empty app is not animating"
            );
            assert!(
                source.borrow_mut().frame(None).is_none(),
                "no new frame after the initial render"
            );
            Ok(())
        }
    }

    /// A host waker that just records every wake in an `AtomicBool` (mirrors
    /// `crate::system::tests::TestWake`).
    struct TestWake(Arc<AtomicBool>);

    impl Wake for TestWake {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Performs the real DRM/preview flow: `run()` has already pre-rendered a
    /// frame with no waker armed, so the backend must arm its own waker and
    /// still receive its first present immediately — a static app must not stay
    /// black until the first event.
    struct PresentInitialBackend;

    impl Backend for PresentInitialBackend {
        fn initialize(&mut self) -> Result<Viewport, Box<dyn std::error::Error + Send + Sync>> {
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
            let flag = Arc::new(AtomicBool::new(false));
            let waker: &'static Waker =
                Box::leak(Box::new(Waker::from(Arc::new(TestWake(flag.clone())))));
            assert!(
                source.borrow_mut().frame(Some(waker)).is_some(),
                "first host-armed frame must present after run()'s pre-render"
            );
            assert!(
                source.borrow_mut().frame(Some(waker)).is_none(),
                "static app idles again after its initial present"
            );
            Ok(())
        }
    }

    /// `run` must perform: backend initialize → create TouchbardSystem at the
    /// returned viewport → hand it to `backend.run`, in that order, with no
    /// backend lifecycle details leaked to the caller.
    #[test]
    fn run_initializes_backend_then_system_then_hands_over() {
        let _guard = crate::testing::RENDER.lock().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let backend = LifecycleBackend {
            log: Rc::clone(&log),
        };
        let config = TouchbardConfig {
            backend: Box::new(backend),
        };
        run(empty_app, config).expect("run succeeds");
        assert_eq!(*log.borrow(), ["initialize", "run"]);
    }

    /// Regression: `run()`'s pre-render must not consume the backend's initial
    /// present — the first host-armed `frame` still produces a frame.
    #[test]
    fn run_first_host_armed_frame_presents_after_pre_render() {
        let _guard = crate::testing::RENDER.lock().unwrap();
        let config = TouchbardConfig {
            backend: Box::new(PresentInitialBackend),
        };
        run(empty_app, config).expect("run succeeds");
    }

    /// Initialization errors must surface cleanly (no panic): the caller gets
    /// the backend's own error back.
    #[test]
    fn run_propagates_backend_initialization_errors() {
        struct FailingBackend;

        impl Backend for FailingBackend {
            fn initialize(&mut self) -> Result<Viewport, Box<dyn std::error::Error + Send + Sync>> {
                Err("backend init failed".into())
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
                assert!(e.to_string().contains("backend init failed"));
            }
            other => panic!("expected Initialize error, got {other:?}"),
        }
    }
}
