//! Legacy DRM/KMS mode selection and initial modeset.
//!
//! Picks a concrete display configuration (connector + encoder + CRTC + full
//! mode) from the resources discovered in [`crate::resources`], then performs
//! the *initial* legacy modeset: bind the chosen CRTC to a framebuffer and
//! connector with `DRM_IOCTL_MODE_SETCRTC`.
//!
//! This is deliberately the smallest modeset that can put a framebuffer on the
//! panel. There is no page flipping, no double buffering, no DirtyFB, no
//! atomic modesetting, and no scanning/refresh loop.

use std::error::Error;

use drm::control::{self, connector, crtc, encoder, framebuffer, Device as ControlDevice};

use crate::device::DrmDevice;
use crate::resources::{DrmCrtc, DrmResources};

/// The position a CRTC is programmed with for a full-screen modeset.
const POSITION: (u32, u32) = (0, 0);

/// A concrete display configuration chosen from a card's resources.
///
/// Aggregates the handles selected by [`select_display_config`]: the connector
/// that will display, the encoder driving it, the CRTC the encoder is attached
/// to, and the full DRM mode the CRTC will be programmed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmDisplayConfig {
    connector: connector::Handle,
    encoder: encoder::Handle,
    crtc: crtc::Handle,
    mode: drm::control::Mode,
}

impl DrmDisplayConfig {
    /// The connector chosen to display.
    pub fn connector(&self) -> connector::Handle {
        self.connector
    }

    /// The encoder chosen to drive [`Self::connector`].
    pub fn encoder(&self) -> encoder::Handle {
        self.encoder
    }

    /// The CRTC chosen to drive [`Self::encoder`].
    pub fn crtc(&self) -> crtc::Handle {
        self.crtc
    }

    /// The full DRM mode the CRTC will be programmed with.
    pub fn mode(&self) -> drm::control::Mode {
        self.mode
    }
}

/// Select a display configuration for the card.
///
/// Reuses the already-discovered `resources`: the first connected connector
/// with at least one mode is chosen; its first (usually best) mode is used; the
/// encoder currently attached to that connector is chosen; and the CRTC that
/// encoder is currently attached to is used, falling back to the card's first
/// CRTC if the encoder reports none. Nothing is hardcoded: all IDs come from
/// the live device query.
pub fn select_display_config(
    device: &DrmDevice,
    resources: &DrmResources,
) -> Result<DrmDisplayConfig, Box<dyn Error + Send + Sync>> {
    let connector_info = resources
        .connected_connector()
        .ok_or_else(|| "no connected connector on the card".to_string())
        .map_err(Box::<dyn Error + Send + Sync>::from)?;

    let connector_id = connector_info.id();
    let connector_handle = control::from_u32(connector_id)
        .ok_or_else(|| format!("invalid connector handle {connector_id}"))
        .map_err(Box::<dyn Error + Send + Sync>::from)?;
    let connector = device
        .get_connector(connector_handle, true)
        .map_err(|e| io_err(&format!("get_connector({connector_id})"), e))?;

    let mode = connector
        .modes()
        .first()
        .copied()
        .ok_or_else(|| format!("connector {connector_id} has no modes; nothing to display"))
        .map_err(Box::<dyn Error + Send + Sync>::from)?;

    let encoder = choose_encoder(connector.current_encoder(), connector.encoders())
        .ok_or_else(|| format!("connector {connector_id} has no encoders"))
        .map_err(Box::<dyn Error + Send + Sync>::from)?;
    let encoder_id = u32::from(encoder);
    let encoder_info = device
        .get_encoder(encoder)
        .map_err(|e| io_err(&format!("get_encoder({encoder_id})"), e))?;

    let crtc = choose_crtc(encoder_info.crtc(), resources.crtcs())
        .ok_or_else(|| format!("no CRTC compatible with encoder {encoder_id}"))
        .map_err(Box::<dyn Error + Send + Sync>::from)?;

    Ok(DrmDisplayConfig {
        connector: connector_handle,
        encoder,
        crtc,
        mode,
    })
}

/// Choose the encoder that will drive a connector.
///
/// Prefers the connector's currently attached encoder (the live binding) and
/// falls back to the first encoder the connector can be driven by.
fn choose_encoder(
    current: Option<encoder::Handle>,
    possible: &[encoder::Handle],
) -> Option<encoder::Handle> {
    current.or_else(|| possible.first().copied())
}

/// Choose the CRTC that an encoder can drive.
///
/// Prefers the CRTC the encoder is currently attached to, falling back to the
/// first CRTC present on the card.
fn choose_crtc(current: Option<crtc::Handle>, card: &[DrmCrtc]) -> Option<crtc::Handle> {
    current.or_else(|| card.first().and_then(|c| control::from_u32(c.id())))
}

/// Perform the initial legacy modeset.
///
/// Programs `config.crtc` to display `framebuffer` full-screen on
/// `config.connector` at `config.mode` (the `DRM_MODE_SETCRTC` ioctl). The
/// caller must hold DRM master; call [`drm::Device::acquire_master_lock`]
/// before this and release it when done.
pub fn initial_modeset(
    device: &DrmDevice,
    framebuffer: framebuffer::Handle,
    config: &DrmDisplayConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    device
        .set_crtc(
            config.crtc,
            Some(framebuffer),
            POSITION,
            core::slice::from_ref(&config.connector),
            Some(config.mode),
        )
        .map_err(|e| {
            io_err(
                &format!(
                    "set_crtc(crtc {}, fb {}, connector {})",
                    u32::from(config.crtc),
                    u32::from(framebuffer),
                    u32::from(config.connector),
                ),
                e,
            )
        })
}

/// Release a CRTC: unbind its framebuffer so the kernel no longer scans it.
///
/// Called as the first step of teardown, before the framebuffer/dumb buffer
/// are destroyed. Safe to call even when nothing is bound.
pub fn disable_crtc(
    device: &DrmDevice,
    config: &DrmDisplayConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    device
        .set_crtc(config.crtc, None, POSITION, &[], None)
        .map_err(|e| {
            io_err(
                &format!("set_crtc(crtc {}, off)", u32::from(config.crtc)),
                e,
            )
        })
}

fn io_err(action: &str, e: std::io::Error) -> Box<dyn Error + Send + Sync> {
    format!("{action} failed: {e}").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    use drm::control::from_u32;

    fn crtc(id: u32) -> crtc::Handle {
        from_u32(id).unwrap()
    }

    fn encoder(id: u32) -> encoder::Handle {
        from_u32(id).unwrap()
    }

    #[test]
    fn choose_encoder_prefers_current_over_list() {
        assert_eq!(
            choose_encoder(Some(encoder(7)), &[encoder(1), encoder(2)]),
            Some(encoder(7)),
        );
        assert_eq!(choose_encoder(None, &[]), None);
        assert_eq!(choose_encoder(None, &[encoder(1)]), Some(encoder(1)));
    }

    #[test]
    fn choose_crtc_prefers_current_over_first_card_crtc() {
        let card = [DrmCrtc { id: 3 }];
        assert_eq!(choose_crtc(Some(crtc(2)), &card), Some(crtc(2)));
        assert_eq!(choose_crtc(None, &card), Some(crtc(3)));
        assert_eq!(choose_crtc(None, &[]), None);
    }
}
