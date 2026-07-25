//! The frame-source boundary shared by all display backends.

use crate::{Frame, PointerEvent};

/// A viewport-agnostic runtime shell that produces frames and accepts pointer
/// input.
///
/// Implemented by the runtime shell ([`crate`]-agnostic: in practice
/// `TouchbardSystem`) and consumed by every backend - the WebSocket preview now,
/// DRM/KMS later - so a backend never needs to know about Dioxus or Blitz.
/// The shared object is `Rc<RefCell<dyn FrameSource>>` and is driven on a
/// single thread (the underlying document is not `Send`).
///
/// The physical viewport is not exposed here: it is discovered by the backend
/// at [`Backend::initialize`](crate::Backend::initialize) and handed to the
/// runtime when the UI system is created, so [`FrameSource`] stays focused on
/// the rendered UI/frame side only.
pub trait FrameSource {
    /// Dispatch a pointer event given in logical (CSS) pixels.
    fn handle_pointer_event(&mut self, event: PointerEvent);

    /// Flush pending host work and rasterize the current document into a [`Frame`].
    fn poll_and_render(&mut self) -> Frame;
}