//! The frame-source boundary shared by all display backends.

use std::task::Waker;

use crate::{Frame, PointerEvent};

/// The presentation cadence: change-driven frames are coalesced to at most one
/// per period (exactly 60 Hz — one 16.66 ms cadence per vertical blank of a
/// 60 Hz panel). A burst of scheduler wakeups — for example a high-frequency
/// timer updating state — must not each force their own rasterize; the newest
/// state is presented instead, at most one period late. This is the same bound a
/// backend already applies while a document is animating; here it is the general
/// upper bound shared by every backend.
///
/// The cadence is exactly `1/60 s`, *not* a rounded `16 ms`: presentation must
/// land on the display's refresh grid. A 16 ms period drifts 0.66 ms against a
/// 16.66 ms refresh, so the present phase slides across the scan every few
/// frames — a beat that reads as shaking/jumping just after the motion starts.
pub const FRAME_CADENCE: std::time::Duration = std::time::Duration::from_nanos(1_000_000_000 / 60);

/// A viewport-agnostic runtime shell that produces frames and accepts pointer
/// input.
///
/// Implemented by the runtime shell ([`crate`]-agnostic: in practice
/// `TouchbardSystem`) and consumed by every backend - the WebSocket preview and
/// DRM/KMS - so a backend never needs to know about Dioxus or Blitz. The shared
/// object is `Rc<RefCell<dyn FrameSource>>` and is driven on a single thread
/// (the underlying document is not `Send`).
///
/// The physical viewport is not exposed here: it is discovered by the backend
/// at [`Backend::initialize`](crate::Backend::initialize) and handed to the
/// runtime when the UI system is created, so [`FrameSource`] stays focused on
/// the rendered UI/frame side only.
///
/// Frame production is event-driven: a backend registers exactly one wait
/// primitive (blocking OS eventfd/poll, or an async `select!`) and asks
/// [`frame`](FrameSource::frame) for a frame every time something might have
/// changed. The runtime answers `Some` only when the document changed, a
/// redraw was requested (shell provider / hover), or the document is animating;
/// otherwise it answers `None` and the backend keeps blocking, so nothing is
/// presented and no frames are rasterized while a UI is idle. Among change
/// events, presentation is coalesced to at most one frame per
/// [`FRAME_CADENCE`]: a burst of scheduler wakeups (e.g. a high-frequency timer
/// updating state) is deferred through [`frame_pending`](FrameSource::frame_pending)
/// and presented at the cadence, never per wake. A backend that arms its host
/// waker is guaranteed at least one `Some` answer, its initial present, even if
/// the runtime already pre-rendered a frame with no waker armed; after that,
/// presentation is change-driven within the cadence.
pub trait FrameSource {
    /// Dispatch a pointer event given in logical (CSS) pixels.
    fn handle_pointer_event(&mut self, event: PointerEvent);

    /// Produce a [`Frame`] when the document changed, requested a redraw, or is
    /// animating; `None` when there is nothing new to present.
    ///
    /// `wake` is the host's long-lived waker: backends leak one `'static` waker
    /// per run, because Dioxus' scheduler needs a `&'static Waker`. It is armed
    /// on the Dioxus scheduler and the shell redraw bridge so both can wake a
    /// blocked host. Re-arming it happens on every call (the previous value —
    /// if any — is replaced), and `None` leaves the last armed waker in place;
    /// either way any redraw requested since the last call is consumed here.
    ///
    /// A first call with `wake: Some` is guaranteed to produce a frame (the
    /// backend's initial present), even if a frame was already rendered with no
    /// waker armed (e.g. the runtime's pre-render); the initial present is what
    /// a static app shows until its first event.
    fn frame(&mut self, wake: Option<&'static Waker>) -> Option<Frame>;

    /// Whether the runtime currently needs repeated frame ticks because the
    /// document is animating (CSS animations/transitions, `<canvas>`). A
    /// backend uses this to bound its wait while such ticks are active.
    fn needs_redraw(&self) -> bool;

    /// Whether a frame is due that was coalesced into the presentation cadence.
    ///
    /// [`frame`](FrameSource::frame) coalesces change-driven renders to at most
    /// one per [`FRAME_CADENCE`], rather than every scheduler wake: when a burst
    /// of changes (e.g. a high-frequency timer) arrives sooner than the cadence,
    /// the render is deferred and `frame_pending()` becomes true for the next
    /// wait. A backend must bound its wait for it exactly as it does for
    /// [`needs_redraw`](Self::needs_redraw), so the deferred frame is presented
    /// within one cadence instead of being lost while the backend blocks.
    fn frame_pending(&self) -> bool {
        false
    }

    /// When a frame is due, the time remaining until it is due on the
    /// [`FRAME_CADENCE`] boundary — either because one was coalesced and is
    /// now pending, or because the document is animating (so the backend
    /// bounds its wait to keep the animation advancing).
    ///
    /// A backend about to wait must use this as its bound rather than a fresh,
    /// full [`FRAME_CADENCE`] — otherwise the period restarts after the
    /// previous present and its render, stretching the presented interval and
    /// making animation advance by unequal steps frame to frame (perceived as
    /// jumping/shaking). Waiting exactly the remaining slice lands the next
    /// frame on the cadence boundary, so the present rate stays uniform.
    /// `None` when nothing is due and the caller should block until it is
    /// woken.
    fn frame_deadline(&self) -> Option<std::time::Duration> {
        None
    }
}
