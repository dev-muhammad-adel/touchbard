//! Top-level configuration passed to [`crate::run`].

use touch_ui_renderer::Backend;

/// Top-level configuration passed to [`crate::run`].
///
/// The backend is chosen up front and owns the configuration relevant to it
/// (the browser preview keeps its own `PreviewConfig` in `touch-ui-preview`;
/// DRM/KMS keeps `DrmConfig` in `touch-ui-drm`), so there is no flat "mega"
/// config with fields that only apply to one backend - and the core crate has
/// no dependency on either backend crate.
pub struct TouchUiConfig {
    /// Which display backend to drive the UI on.
    pub backend: Box<dyn Backend>,
}