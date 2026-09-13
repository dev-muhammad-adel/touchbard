//! WebSocket preview backend for Touch UI.
//!
//! Renders the real RGBA framebuffer produced by the Touch UI pipeline and
//! streams it to a browser over WebSocket, where it is displayed on an HTML
//! canvas. The browser's pointer events travel back over the same connection
//! and are dispatched into the Dioxus runtime.
//!
//! The crate is a concrete [`Backend`]
//! (see [`PreviewBackend`] and [`PreviewConfig`]): it owns everything specific
//! to the browser preview - framebuffer size/scale, bind address, opening the
//! browser, and the WebSocket protocol - and only depends on
//! `touch-ui-renderer` for the shared backend boundary.

pub mod protocol;
pub mod server;

pub use protocol::*;
pub use server::*;