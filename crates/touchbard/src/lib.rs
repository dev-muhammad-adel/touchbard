//! Core integration layer for the Touchbard framework.
//!
//! Pipeline:
//!
//! ```text
//! Dioxus VirtualDom
//!     ↓ mutations
//! DioxusDocument (dioxus-native-dom wrapping blitz_dom::BaseDocument)
//!     ↓ styles/layout resolved
//! blitz_paint::paint_scene (pushes into anyrender::PaintScene)
//!     ↓ draw commands
//! anyrender_vello_cpu::VelloCpuScenePainter
//!     ↓ rasterization
//! vello_cpu::render_to_buffer (premultiplied RGBA8)
//!     ↓ raw bytes
//! Frame (premultiplied RGBA framebuffer)
//! ```
//!
//! #[`run`] performs the backend lifecycle: it initializes the backend (which
//! discovers/provides the authoritative physical viewport), creates the
//! [`TouchbardSystem`](system::TouchbardSystem) at that viewport, then hands
//! control to the backend, which owns its own event loop and presentation.
//! The backend boundary types ([`Frame`], [`PointerEvent`], [`Viewport`],
//! [`FrameSource`], [`Backend`]) live in `touchbard-renderer` and are
//! re-exported here for convenience. The concrete backends
//! (`touchbard-preview`, `touchbard-drm`) depend on that boundary; this crate
//! does not depend on them.

pub mod config;
pub mod routing;
pub mod run;
pub mod system;

pub use config::*;
pub use routing::*;
pub use run::*;
pub use system::*;
pub use touchbard_renderer::{
    Backend, Frame, FrameSource, PixelFormat, PointerButton, PointerEvent, PointerEventKind,
    Viewport,
};