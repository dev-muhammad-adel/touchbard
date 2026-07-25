//! DRM/KMS backend for the Touchbard framework.
//!
//! Placeholder: real DRM work (connector discovery, modesetting, page flips,
//! GBM/EGL allocator) is a later milestone and is deliberately not implemented
//! here. [`DrmBackend`] still implements the shared [`Backend`] boundary so the
//! integration/entry crate can construct and select it; its
//! [`Backend::initialize`] fails with a clean "not implemented" error, which
//! the runtime reports as a backend-initialization error.
//!
//! The future lifecycle (not implemented): open/probe the DRM device →
//! discover the connector → obtain its dimensions → return the real
//! [`Viewport`] from `initialize` → create the UI system at that viewport →
//! `run` with page flips and input. Framebuffer dimensions will come from the
//! connected display's connector, so `DrmConfig` carries no size or scale - and
//! no bind address.

use std::cell::RefCell;
use std::rc::Rc;

use touchbard_renderer::{Backend, FrameSource, Viewport};

/// Configuration for the DRM backend.
///
/// Minimal by design: there are no tunables yet. When real DRM lands, fields
/// such as a `--drm=<card>` device path can be added here while the preview
/// fields remain in the touchbard-preview crate's `PreviewConfig`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrmConfig;

/// The DRM/KMS [`Backend`] scaffold.
///
/// Not yet implemented: [`Backend::run`] always errors, so selecting it with
/// `--drm` fails fast instead of producing a broken display.
pub struct DrmBackend {
    config: DrmConfig,
}

impl DrmBackend {
    /// Build the DRM backend from its configuration.
    pub fn new(config: DrmConfig) -> Self {
        Self { config }
    }

    /// The configuration backing this backend.
    pub fn config(&self) -> &DrmConfig {
        &self.config
    }
}

impl Backend for DrmBackend {
    fn initialize(&mut self) -> Result<Viewport, Box<dyn std::error::Error + Send + Sync>> {
        // Real DRM will open/probe the device and discover the connector here,
        // then return the connected mode's dimensions as the authoritative
        // viewport. For now this fails cleanly, so `--drm` aborts with a clear
        // error instead of creating a UI at a fake size.
        Err("the DRM/KMS backend is not implemented yet (use the browser preview backend)".into())
    }

    fn run(
        &mut self,
        _source: Rc<RefCell<dyn FrameSource>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Unreachable today (initialize always fails); kept as a clean error so
        // a mis-sequenced caller still cannot double-run or panic.
        Err("the DRM/KMS backend is not implemented yet (use the browser preview backend)".into())
    }
}