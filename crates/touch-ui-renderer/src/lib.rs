//! Rendering backends and shared boundary types for Touch UI.
//!
//! This crate owns:
//! - the concrete CPU rendering pipeline ([`cpu`]) used by all display backends,
//! - the framebuffer ([`Frame`]) and pointer-input ([`PointerEvent`]) types that
//!   form the backend boundary (the WebSocket preview consumes them now; DRM/KMS
//!   will consume the same),
//! - the [`Viewport`] type backends produce at initialization and the runtime
//!   creates the UI at,
//! - the [`FrameSource`] trait backends use to drive a runtime without knowing
//!   about Dioxus or Blitz,
//! - the [`Backend`] trait that concrete backends (`touch-ui-preview`,
//!   `touch-ui-drm`) implement, so the core crate is independent of them.

pub mod backend;
pub mod cpu;
pub mod events;
pub mod frame;
pub mod frame_source;

pub use backend::*;
pub use cpu::*;
pub use events::*;
pub use frame::*;
pub use frame_source::*;