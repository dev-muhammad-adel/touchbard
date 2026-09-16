//! Rendering backends and shared boundary types for Touchbard.
//!
//! This crate owns:
//! - the concrete CPU rendering pipeline ([`cpu`]) used by all display backends,
//! - the framebuffer ([`Frame`]) and pointer-input ([`PointerEvent`]) types that
//!   form the backend boundary (consumed by the WebSocket preview and DRM/KMS),
//! - the [`Viewport`] type backends produce at initialization and the runtime
//!   creates the UI at,
//! - the [`FrameSource`] trait backends use to drive a runtime without knowing
//!   about Dioxus or Blitz,
//! - the [`Backend`] trait that concrete backends (`touchbard-preview`,
//!   `touchbard-drm`) implement, so the core crate is independent of them.

pub mod backend;
pub mod cpu;
pub mod diag;
pub mod events;
pub mod frame;
pub mod frame_source;

pub use backend::*;
pub use cpu::*;
pub use events::*;
pub use frame::*;
pub use frame_source::*;
