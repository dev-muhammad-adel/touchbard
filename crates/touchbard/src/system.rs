//! Core `TouchbardSystem`: owns the Dioxus+Blitz document and renders frames.

use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Waker};

use blitz_dom::Document as _;
use blitz_paint::paint_scene_at;
use blitz_traits::events::UiEvent;
use blitz_traits::shell::{ColorScheme, ShellProvider, Viewport as BlitzViewport};
use dioxus_core::VirtualDom;
use dioxus_native_dom::{DioxusDocument, DocumentConfig};
use touchbard_renderer::{CpuRenderer, Frame, FrameSource, PointerEvent, PointerEventKind};
use tracing::trace;

/// Viewport (framebuffer) configuration for a [`TouchbardSystem`].
///
/// This is the authoritative physical-pixel framebuffer size plus a scale
/// factor, produced by the selected backend at initialization (the preview's
/// configured/default framebuffer size, or the DRM connector's mode). The
/// runtime creates the system at exactly this viewport; there is no
/// independent copy anywhere else.
pub use touchbard_renderer::Viewport;

/// The central runtime: a Dioxus `VirtualDom` integrated with a Blitz
/// `BaseDocument`, plus a CPU (Vello) render pipeline.
///
/// The runtime is driven through its [`FrameSource`] half: backends call
/// [`frame`](FrameSource::frame) whenever any wake signal says a frame might be
/// wanted, and the runtime produces one only when there is something new to
/// show:
///
///  * Dioxus state changed (the scheduler wakes the host waker registered on
///    the previous call),
///  * a redraw was requested through the shell provider (e.g. the hovered
///    element changed as the cursor moved),
///  * the document is animating (CSS animations/transitions, `<canvas>`), so a
///    bounded animation cadence keeps it being re-rendered until it stops.
///
/// When the runtime has no work it returns `None` and a backend blocks until
/// one of those signals fires again — the presentation rate is event-driven,
/// and bounded at ≈60 Hz by the shared presentation cadence
/// ([`FRAME_CADENCE`](touchbard_renderer::frame_source::FRAME_CADENCE)): even a
/// burst of scheduler wakeups (a high-frequency timer updating state) coalesces
/// into at most one frame per cadence, so the display never renders at the wake
/// rate, only at the display's rate.
///
/// Operations:
///  1. [`TouchbardSystem::new`] creates the document from a Dioxus app function.
///  2. [`TouchbardSystem::frame`] polls Dioxus and renders only when the
///     document changed, requested a redraw, or is animating.
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
    /// When the system was created; rendered frames advance CSS animations
    /// (transitions/keyframes) against this clock.
    animation_started: std::time::Instant,
    /// Shared host-wake state read/written by `frame`, `poll`, and the shell
    /// redraw bridge (which also holds a clone of the [`Arc`]).
    redraw: Arc<Mutex<RedrawSlot>>,
    /// Whether at least one frame has been rasterized since creation. The
    /// initial build mutates the Blitz document even though the Dioxus
    /// scheduler then reports no pending work, so the first `frame` must render
    /// unconditionally.
    rendered: bool,
    /// Whether at least one host-armed (`wake: Some`) frame has been produced.
    ///
    /// Kept separate from `rendered` so that a pre-render with no armed waker
    /// (e.g. [`run`](crate::run::run)'s initial frame) does not steal the
    /// backend's guaranteed first present: a backend that arms its waker must
    /// always receive an initial frame to show.
    presented_once: bool,
    /// When the last frame was rasterized. Change-driven renders are coalesced
    /// to at most one per [`FRAME_CADENCE`] so a burst of scheduler wakeups
    /// (e.g. a high-frequency timer updating state) cannot force a rasterize
    /// per wake. `None` before the first frame.
    last_present: Option<std::time::Instant>,
    /// A change arrived within the cadence and was coalesced: present it on the
    /// next [`frame`](Self::frame) call. A backend queries this via
    /// [`frame_pending`](Self::frame_pending) to bound its wait, so the
    /// deferred frame is presented within one cadence rather than lost while
    /// the backend blocks.
    render_pending: bool,
}

/// Host-wake state shared between the runtime and backends.
///
/// `requested` records that a redraw was requested since the last frame (for
/// example the shell provider firing after the hovered element changed). The
/// host's waker is re-armed on every `frame` call, and every wake signal —
/// shell redraw bridge, Dioxus scheduler, or the runtime itself — fires it so
/// a blocked backend wakes up and asks for a frame.
#[derive(Default)]
struct RedrawSlot {
    waker: Option<Waker>,
    requested: bool,
}

/// A [`ShellProvider`] that turns Blitz redraw requests into a runtime wake:
/// `request_redraw()` marks the redraw-needed flag and notifies the armed host
/// waker. Replaces the crate-default `DummyShellProvider`, which silently
/// drops redraw requests.
struct WakingShellProvider {
    slot: Arc<Mutex<RedrawSlot>>,
}

impl ShellProvider for WakingShellProvider {
    fn request_redraw(&self) {
        let mut slot = self.slot.lock().unwrap();
        slot.requested = true;
        if let Some(waker) = slot.waker.as_ref() {
            waker.wake_by_ref();
        }
    }
}

impl TouchbardSystem {
    /// Create a new system from a Dioxus app function.
    pub fn new(app: fn() -> dioxus_core::Element, config: Viewport) -> Self {
        let redraw = Arc::new(Mutex::new(RedrawSlot::default()));

        // Shell redraw requests (hover changes, canvas invalidation, ...) must
        // reach the host's wake loop, so the system installs its own provider.
        let shell_provider = WakingShellProvider {
            slot: Arc::clone(&redraw),
        };

        let vdom = VirtualDom::new(app);

        let doc_config = DocumentConfig {
            viewport: Some(BlitzViewport::new(
                config.width,
                config.height,
                config.scale_factor as f32,
                ColorScheme::Dark,
            )),
            shell_provider: Some(Arc::new(shell_provider)),
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
            animation_started: std::time::Instant::now(),
            redraw,
            rendered: false,
            presented_once: false,
            last_present: None,
            render_pending: false,
        };
        system.document.initial_build();
        system
    }

    /// Poll the VirtualDom for pending work and flush mutations to the Blitz
    /// document. Returns `true` if there was any work to flush.
    pub fn poll(&mut self) -> bool {
        self.document.poll(None)
    }

    /// Poll the VirtualDom with an optional waker for the Dioxus scheduler, and
    /// flush mutations to the Blitz document. Returns `true` if there was work.
    ///
    /// Dioxus needs a `&'static` waker: backends hand the runtime a long-lived
    /// waker they leak once per run (see `frame`).
    fn poll_with(&mut self, wake: Option<&'static Waker>) -> bool {
        match wake {
            Some(waker) => self.document.poll(Some(TaskContext::from_waker(waker))),
            None => self.document.poll(None),
        }
    }

    /// Whether the document needs animation ticks (CSS animations, `<canvas>`).
    ///
    /// Note: a `<canvas>` whose layout never changes keeps `is_animating()`
    /// true indefinitely (its invalidation is tracked statically by Blitz), so
    /// `needs_redraw()` can stay true for an idle canvas. The runtime treats
    /// that as an animated document and keeps producing frames; this is the
    /// documented Blitz behaviour and is not detected here (no internals are
    /// inspected).
    pub fn needs_redraw(&self) -> bool {
        self.document.is_animating()
    }

    /// The shared runtime scheduling step used by backends.
    ///
    /// `wake` is the host's long-lived waker (backends leak one per run). The
    /// previous value — if any — is re-armed for this wait; the Dioxus
    /// scheduler and the shell redraw bridge both fire it when a frame is
    /// wanted. Passing `None` keeps the last armed waker in place (useful for a
    /// one-shot frame that will not block).
    ///
    /// Returns a [`Frame`] to present when the document changed, a redraw was
    /// requested, or the document is animating; `None` when there is nothing to
    /// present and the caller should block until its wake fires. When it is
    /// animating, the caller bounds its wait (≈60 Hz cadence) instead of
    /// blocking indefinitely, so the animation keeps advancing (see
    /// [`needs_redraw`](Self::needs_redraw)).
    ///
    /// Change-driven renders are coalesced to at most one per
    /// [`FRAME_CADENCE`](touchbard_renderer::frame_source::FRAME_CADENCE): a
    /// change arriving sooner than that after the last present is recorded as
    /// pending and returned as `None`, so a burst of scheduler wakeups (e.g. a
    /// high-frequency timer updating state) cannot force a rasterize per wake.
    /// The pending frame is presented within one cadence; a backend bounds its
    /// wait for it via [`frame_pending`](Self::frame_pending) exactly like it
    /// bounds for animations.
    ///
    /// A backend that arms its waker (`wake: Some`) is guaranteed at least one
    /// frame, its initial present: even if the runtime has already rasterized a
    /// frame with no waker armed (e.g. [`run`](crate::run::run)'s pre-render),
    /// the first host-armed frame still renders. After that, presentation is
    /// change-driven within the cadence.
    pub fn frame(&mut self, wake: Option<&'static Waker>) -> Option<Frame> {
        // Consume any redraw request and re-arm the host waker. This happens
        // *before* polling so a wake signalled by Dioxus during the poll is a
        // late answer to the frame we are about to produce, not a spurious one
        // still pending after it.
        let requested = {
            let mut slot = self.redraw.lock().unwrap();
            if let Some(waker) = wake {
                slot.waker = Some(waker.clone());
            }
            std::mem::take(&mut slot.requested)
        };
        let changed = self.poll_with(wake);
        let needs_initial_present = wake.is_some() && !self.presented_once;
        if !self.rendered || needs_initial_present || changed || requested || self.needs_redraw()
            || self.render_pending
        {
            // Coalesce change-driven renders to the presentation cadence: a
            // burst of scheduler wakeups (e.g. a high-frequency timer writing
            // state) must not each rasterize a frame — that would render at the
            // wake rate and flood the display. The first frame and each backend's
            // initial present are exempt (they never wait). A coalesced frame is
            // recorded as pending so a backend that bounds its wait on
            // `frame_pending()` comes back within one cadence to present it.
            let within_cadence = self
                .last_present
                .is_some_and(|t| t.elapsed() < touchbard_renderer::frame_source::FRAME_CADENCE);
            let exempt = !self.rendered || needs_initial_present;
            if within_cadence && !exempt {
                self.render_pending = true;
                return None;
            }
            self.render_pending = false;
            self.last_present = Some(std::time::Instant::now());
            touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::FrameStart {
                animating: self.needs_redraw(),
            });
            let frame = self.render();
            if wake.is_some() {
                self.presented_once = true;
            }
            Some(frame)
        } else {
            None
        }
    }

    /// Whether a change was coalesced into the presentation cadence and a frame
    /// is now due. A backend bounds its wait on this (like `needs_redraw`) so
    /// the deferred frame is presented within one cadence instead of being lost
    /// while the backend blocks.
    pub fn frame_pending(&self) -> bool {
        self.render_pending
    }

    /// The time remaining until a frame is due on the cadence boundary.
    ///
    /// A present is imminent when a coalesced frame is pending (it is due
    /// exactly on the boundary) or when the document is animating (a backend
    /// bounds its wait so the animation keeps advancing). In both cases a
    /// backend must wait this long — not a fresh, full [`FRAME_CADENCE`] —
    /// before calling [`frame`](Self::frame). Waiting a full cadence after the
    /// previous present returns *after* the render of that frame, so the next
    /// render starts one cadence *plus one render duration* later; with a
    /// variable render cost the present interval varies frame to frame and
    /// animation advances by unequal steps (jumping/shaking). Waiting exactly
    /// the remaining slice lands the next present on the cadence boundary
    /// relative to the last one, so the present rate stays uniform. `None` when
    /// nothing is due and the caller should block until it is woken.
    pub fn frame_deadline(&self) -> Option<std::time::Duration> {
        if !(self.render_pending || self.needs_redraw()) {
            return None;
        }
        self.last_present.map(|t| {
            touchbard_renderer::frame_source::FRAME_CADENCE
                .saturating_sub(t.elapsed())
        })
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
        // restyles the tree and relayouts it; the timestamp drives CSS
        // animations, so each rendered frame advances them against the system's
        // own clock (static UIs are unaffected by the value).
        let t0 = std::time::Instant::now();
        let now = self.animation_started.elapsed().as_secs_f64();
        self.document.resolve(now);
        touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Flush {
            now_ms: now * 1000.0,
            resolve_us: t0.elapsed().as_micros() as u64,
        });

        let frame = self.renderer.render(
            |scene| {
                let paint_start = std::time::Instant::now();
                paint_scene_at(scene, &self.document, scale, width, height, Some(now));
                touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::SceneDone {
                    paint_us: paint_start.elapsed().as_micros() as u64,
                });
            },
            width,
            height,
        );
        // `Raster` timing is recorded inside `CpuRenderer::render` (wraps the
        // whole publish + paint + rasterize step).
        self.rendered = true;
        frame
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

/// The shell is itself a [`FrameSource`], so any backend (preview WebSocket or
/// DRM) can drive it without knowing about Dioxus or Blitz. The physical
/// viewport is owned by the backend (see [`Backend::initialize`](touchbard_renderer::Backend::initialize));
/// the shell only renders frames, accepts input, and reports whether it is
/// animating.
impl FrameSource for TouchbardSystem {
    fn handle_pointer_event(&mut self, event: PointerEvent) {
        TouchbardSystem::handle_pointer_event(self, event);
    }

    fn frame(&mut self, wake: Option<&'static Waker>) -> Option<Frame> {
        TouchbardSystem::frame(self, wake)
    }

    fn needs_redraw(&self) -> bool {
        self.needs_redraw()
    }

    fn frame_pending(&self) -> bool {
        self.frame_pending()
    }

    fn frame_deadline(&self) -> Option<std::time::Duration> {
        self.frame_deadline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::prelude::*;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::sync::Arc;
    use std::task::{Wake, Waker};
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

    /// A host waker that just records every wake in an `AtomicBool`.
    struct TestWake(Arc<AtomicBool>);

    impl Wake for TestWake {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// An armed `'static` waker plus its flag, to observe runtime wakes from
    /// the host side.
    fn armed_waker() -> (&'static Waker, Arc<AtomicBool>) {
        let flag = Arc::new(AtomicBool::new(false));
        let waker: &'static Waker =
            Box::leak(Box::new(Waker::from(Arc::new(TestWake(flag.clone())))));
        (waker, flag)
    }

    /// See `crate::testing::RENDER`: Blitz paints (and the shared `COUNTER`)
    /// must not run concurrently with other tests, so rendering tests hold this
    /// guard for their whole body.
    fn render_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::testing::RENDER.lock().unwrap()
    }

    const ANIM_CSS: &str = r#"
        @keyframes opacity-pulse {
            from { opacity: 1.0; }
            to   { opacity: 0.2; }
        }
        .demo {
            animation: opacity-pulse 1s linear infinite;
        }
    "#;

    fn animated_app() -> Element {
        rsx! {
            div {
                style: "width: 100%; height: 100%; background: #000;",
                style { {ANIM_CSS} }
                div {
                    class: "demo",
                    style: "width: 40px; height: 8px; background-color: #fff;",
                }
            }
        }
    }

    const FINITE_CSS: &str = r#"
        @keyframes fade-out {
            from { opacity: 1.0; }
            to   { opacity: 0.0; }
        }
        .fade {
            animation: fade-out 200ms linear 1;
        }
    "#;

    fn finite_animation_app() -> Element {
        rsx! {
            div {
                style: "width: 100%; height: 100%; background: #000;",
                style { {FINITE_CSS} }
                div {
                    class: "fade",
                    style: "width: 40px; height: 8px; background-color: #fff;",
                }
            }
        }
    }

    #[test]
    fn test_static_ui_renders_once_then_idles() {
        let _guard = render_lock();
        let config = Viewport {
            width: 100,
            height: 30,
            scale_factor: 1.0,
        };
        let mut sys = TouchbardSystem::new(test_app, config);
        assert!(sys.frame(None).is_some(), "initial state must render");
        assert!(!sys.needs_redraw(), "static UI is not animating");
        assert!(sys.frame(None).is_none(), "idle UI must not render frames");
        assert!(
            sys.frame(None).is_none(),
            "stays idle after further requests"
        );
    }

    /// A `run()`-style pre-render (no waker armed) must not steal the
    /// backend's guaranteed first present: the first host-armed frame still
    /// renders, and the app idles again afterwards.
    #[test]
    fn test_pre_render_does_not_steal_first_host_present() {
        let _guard = render_lock();
        let config = Viewport {
            width: 100,
            height: 30,
            scale_factor: 1.0,
        };
        let mut sys = TouchbardSystem::new(test_app, config);
        let (waker, _flag) = armed_waker();

        // Equivalent of `run()`'s pre-render with no waker armed.
        assert!(sys.frame(None).is_some(), "pre-render");
        assert!(sys.frame(None).is_none(), "idle after the pre-render");

        // The backend arms its waker: this must be its initial present.
        assert!(
            sys.frame(Some(waker)).is_some(),
            "first host-armed frame must present after a pre-render"
        );
        assert!(
            sys.frame(Some(waker)).is_none(),
            "static app idles again after its initial present"
        );
    }

    #[test]
    fn test_pointer_hover_redraw_wakes_the_host() {
        let _guard = render_lock();
        let config = Viewport {
            width: 100,
            height: 30,
            scale_factor: 1.0,
        };
        let mut sys = TouchbardSystem::new(test_app, config);
        let (waker, flag) = armed_waker();

        // Initial frame arms the host waker through the scheduler.
        assert!(sys.frame(Some(waker)).is_some());
        flag.store(false, Ordering::SeqCst);

        // A move that changes the hovered element goes through Blitz's shell
        // provider: the runtime must wake the host and remember the redraw.
        sys.handle_pointer_event(UiPointerEvent {
            x: 50.0,
            y: 15.0,
            button: PointerButton::Main,
            buttons: 0,
            kind: PointerEventKind::Move,
        });
        assert!(
            flag.load(Ordering::SeqCst),
            "hover change must wake the host"
        );

        // The requested redraw is coalesced to the presentation cadence: if it
        // arrives within one cadence of the initial present it is deferred and
        // reported pending, then presented on the backend's next bounded wait;
        // if it arrives after the cadence boundary it is presented immediately.
        // Either way at most one frame comes out of the request, then the UI
        // idles again.
        let deferred = sys.frame(Some(waker)).is_none();
        if deferred {
            assert!(
                sys.frame_pending(),
                "deferred redraw must be reported pending"
            );
            std::thread::sleep(touchbard_renderer::frame_source::FRAME_CADENCE * 2);
            assert!(
                sys.frame(Some(waker)).is_some(),
                "pending redraw presents after the cadence"
            );
        }
        assert!(
            sys.frame(Some(waker)).is_none(),
            "redraw consumed; idle again"
        );
    }

    #[test]
    fn test_dioxus_state_change_wakes_the_host_and_frames() {
        let _guard = render_lock();
        COUNTER.store(0, Ordering::SeqCst);
        let mut sys = TouchbardSystem::new(counter_app, preview_config());
        let (waker, flag) = armed_waker();
        assert!(sys.frame(Some(waker)).is_some(), "initial frame");
        flag.store(false, Ordering::SeqCst);

        // Real input through Blitz → Dioxus onclick → counter signal write.
        // The Dioxus scheduler must wake the registered host waker.
        for kind in [
            PointerEventKind::Move,
            PointerEventKind::Down,
            PointerEventKind::Up,
        ] {
            sys.handle_pointer_event(UiPointerEvent {
                x: 621.7,
                y: 15.0,
                button: PointerButton::Main,
                buttons: if kind == PointerEventKind::Up { 0 } else { 1 },
                kind,
            });
        }
        assert!(
            flag.load(Ordering::SeqCst),
            "Dioxus wake must fire the host waker"
        );
        assert_eq!(COUNTER.load(Ordering::SeqCst), 1, "click landed");

        // The next scheduling step poll pumps the mutation and renders.
        assert!(sys.frame(Some(waker)).is_some(), "changed document renders");
    }

    #[test]
    fn test_coalesced_frame_exposes_cadence_deadline() {
        let _guard = render_lock();
        let config = Viewport {
            width: 2008,
            height: 60,
            scale_factor: 1.0,
        };
        let mut sys = TouchbardSystem::new(animated_app, config);
        assert!(sys.frame(None).is_some(), "initial render");

        // A wake arriving inside the cadence is deferred and reported pending,
        // not silently dropped. Whether one can actually land inside the 16ms
        // window depends on how fast a single full-bar raster is on this
        // machine: when it is ≥ a cadence, every change is already ≥ a cadence
        // apart and the deferral never arises (the double-wait failure mode
        // cannot occur either), so the pending branch below is best-effort.
        let mut deferred = false;
        for _ in 0..8 {
            if sys.frame(None).is_none() {
                deferred = true;
                break;
            }
        }
        if deferred {
            assert!(sys.frame_pending(), "deferred frame is reported pending");
            let deadline = sys.frame_deadline().expect(
                "a pending frame exposes the remaining cadence deadline",
            );
            assert!(
                deadline > std::time::Duration::ZERO
                    && deadline <= touchbard_renderer::frame_source::FRAME_CADENCE,
                "deadline is the remaining slice to the cadence boundary: {deadline:?}"
            );

            // Honoring the deadline (a backend sleeps it) presents the deferred
            // frame on the cadence boundary — one period, not two.
            std::thread::sleep(deadline);
            assert!(
                sys.frame(None).is_some(),
                "the pending frame presents after the deadline"
            );
            assert!(
                !sys.frame_pending(),
                "pending flag clears once the frame presents"
            );
        }
    }

    #[test]
    fn test_active_css_animation_drives_frames() {
        let _guard = render_lock();
        let config = Viewport {
            width: 320,
            height: 60,
            scale_factor: 2.0,
        };
        let mut sys = TouchbardSystem::new(animated_app, config);
        assert!(sys.frame(None).is_some(), "initial render");

        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(sys.needs_redraw(), "animation is active");
        assert!(sys.frame(None).is_some(), "animation tick renders");

        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(sys.frame(None).is_some(), "animation keeps rendering");
    }

    #[test]
    fn test_finished_css_animation_stops_repeated_frames() {
        let _guard = render_lock();
        let config = Viewport {
            width: 320,
            height: 60,
            scale_factor: 2.0,
        };
        let mut sys = TouchbardSystem::new(finite_animation_app, config);
        assert!(sys.frame(None).is_some(), "initial render");
        assert!(sys.needs_redraw(), "one-shot animation is running");

        // 600ms ≫ the 200ms animation; the frame that crosses the end renders
        // the final state.
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(sys.frame(None).is_some(), "final tick at animation end");

        // The document is static again: no further frames.
        assert!(!sys.needs_redraw(), "finished animation is not animating");
        assert!(sys.frame(None).is_none(), "no frames after animation end");
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
        let _guard = render_lock();
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
        let _guard = render_lock();
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
        let _guard = render_lock();
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
            assert_eq!(
                got, expect,
                "button at x={x:.1} must move counter to {expect}"
            );
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
