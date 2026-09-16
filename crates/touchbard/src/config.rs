//! Top-level configuration passed to [`crate::run()`].

use touchbard_renderer::Backend;

/// Top-level configuration passed to [`crate::run()`].
///
/// The backend is chosen up front and owns the configuration relevant to it
/// (the browser preview keeps its own `PreviewConfig` in `touchbard-preview`;
/// DRM/KMS keeps `DrmConfig` in `touchbard-drm`), so there is no flat "mega"
/// config with fields that only apply to one backend - and the core crate has
/// no dependency on either backend crate.
pub struct TouchbardConfig {
    /// Which display backend to drive the UI on.
    pub backend: Box<dyn Backend>,
}
