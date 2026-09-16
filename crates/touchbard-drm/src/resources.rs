//! A safe, owned view over the KMS resources of an opened DRM card.
//!
//! [`DrmResources::query`] issues the read-only KMS resource queries through
//! the `drm` crate's safe API and snapshots what the card exposes: connectors,
//! their connection status and supported modes, the encoders, and the CRTCs.
//! No modesetting happens here; this is pure discovery. Modeset and
//! framebuffer operations are built on top in other modules.

use std::error::Error;

use drm::control::{connector, Device as ControlDevice};

use crate::device::DrmDevice;

/// The connection state of a connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrmConnectionStatus {
    /// The sink is connected and usable.
    Connected,
    /// The sink is disconnected.
    Disconnected,
    /// The kernel did not report a state.
    Unknown,
}

impl From<connector::State> for DrmConnectionStatus {
    fn from(state: connector::State) -> Self {
        match state {
            connector::State::Connected => DrmConnectionStatus::Connected,
            connector::State::Disconnected => DrmConnectionStatus::Disconnected,
            connector::State::Unknown => DrmConnectionStatus::Unknown,
        }
    }
}

/// A display mode a connector reports as supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmMode {
    name: String,
    clock: u32,
    width: u16,
    height: u16,
    vrefresh: u32,
}

impl DrmMode {
    /// The kernel name of this mode (e.g. `800x128`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Pixel clock in kHz.
    pub fn clock(&self) -> u32 {
        self.clock
    }

    /// Horizontal display size in pixels.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Vertical display size in pixels.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Vertical refresh rate in Hz.
    pub fn vrefresh(&self) -> u32 {
        self.vrefresh
    }
}

impl From<&drm::control::Mode> for DrmMode {
    fn from(mode: &drm::control::Mode) -> Self {
        DrmMode {
            name: mode.name().to_string_lossy().into_owned(),
            clock: mode.clock(),
            width: mode.size().0,
            height: mode.size().1,
            vrefresh: mode.vrefresh(),
        }
    }
}

/// A physical output of the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmConnector {
    id: u32,
    interface: String,
    interface_id: u32,
    status: DrmConnectionStatus,
    modes: Vec<DrmMode>,
    encoders: Vec<u32>,
    current_encoder: Option<u32>,
}

impl DrmConnector {
    /// The kernel handle (ID) of this connector.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The physical interface type, e.g. `eDP`, `DisplayPort`, `HDMI-A`.
    pub fn interface(&self) -> &str {
        &self.interface
    }

    /// The zero-based interface index among connectors of the same type.
    pub fn interface_id(&self) -> u32 {
        self.interface_id
    }

    /// Whether a display is currently connected to this connector.
    pub fn connection_status(&self) -> DrmConnectionStatus {
        self.status
    }

    /// The modes this connector reports as supported, in kernel order.
    pub fn modes(&self) -> &[DrmMode] {
        &self.modes
    }

    /// Handles of the encoders that can drive this connector.
    pub fn encoders(&self) -> &[u32] {
        &self.encoders
    }

    /// The encoder currently attached to this connector, if any.
    pub fn current_encoder(&self) -> Option<u32> {
        self.current_encoder
    }
}

/// A device that encodes a video signal for output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrmEncoder {
    id: u32,
    crtc: Option<u32>,
}

impl DrmEncoder {
    /// The kernel handle (ID) of this encoder.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The CRTC this encoder is currently attached to, if any.
    pub fn crtc(&self) -> Option<u32> {
        self.crtc
    }
}

/// A CRTC: the scan-out engine that drives a connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrmCrtc {
    pub(crate) id: u32,
}

impl DrmCrtc {
    /// The kernel handle (ID) of this CRTC.
    pub fn id(&self) -> u32 {
        self.id
    }
}

/// The KMS resources of an opened card: its connectors, encoders, and CRTCs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmResources {
    connectors: Vec<DrmConnector>,
    encoders: Vec<DrmEncoder>,
    crtcs: Vec<DrmCrtc>,
}

impl DrmResources {
    /// The connectors of the card.
    pub fn connectors(&self) -> &[DrmConnector] {
        &self.connectors
    }

    /// The encoders of the card.
    pub fn encoders(&self) -> &[DrmEncoder] {
        &self.encoders
    }

    /// The CRTCs of the card.
    pub fn crtcs(&self) -> &[DrmCrtc] {
        &self.crtcs
    }

    /// The first connector that reports a connected display, if any.
    ///
    /// The card exposes a single output here, so the (first) connected
    /// connector is the one the modeset step will drive. No connector is
    /// special-cased by name or ID here: the card decides.
    pub fn connected_connector(&self) -> Option<&DrmConnector> {
        self.connectors
            .iter()
            .find(|c| c.connection_status() == DrmConnectionStatus::Connected)
    }

    /// Query the resources of an opened DRM card.
    ///
    /// Only read-only kernel queries are issued; the card is not modeset.
    pub fn query(device: &DrmDevice) -> Result<DrmResources, Box<dyn Error + Send + Sync>> {
        let handles = device
            .resource_handles()
            .map_err(|e| io_err("DRM_IOCTL_MODE_GETRESOURCES", e))?;

        let mut connectors = Vec::with_capacity(handles.connectors().len());
        for &handle in handles.connectors() {
            let info = device.get_connector(handle, true).map_err(|e| {
                io_err(
                    &format!("DRM_IOCTL_MODE_GETCONNECTOR({})", u32::from(handle)),
                    e,
                )
            })?;
            connectors.push(DrmConnector {
                id: u32::from(info.handle()),
                interface: info.interface().as_str().to_owned(),
                interface_id: info.interface_id(),
                status: info.state().into(),
                modes: info.modes().iter().map(DrmMode::from).collect(),
                encoders: info.encoders().iter().map(|&h| u32::from(h)).collect(),
                current_encoder: info.current_encoder().map(u32::from),
            });
        }

        let mut encoders = Vec::with_capacity(handles.encoders().len());
        for &handle in handles.encoders() {
            let info = device.get_encoder(handle).map_err(|e| {
                io_err(
                    &format!("DRM_IOCTL_MODE_GETENCODER({})", u32::from(handle)),
                    e,
                )
            })?;
            encoders.push(DrmEncoder {
                id: u32::from(info.handle()),
                crtc: info.crtc().map(u32::from),
            });
        }

        let crtcs = handles
            .crtcs()
            .iter()
            .map(|&handle| DrmCrtc {
                id: u32::from(handle),
            })
            .collect();

        Ok(DrmResources {
            connectors,
            encoders,
            crtcs,
        })
    }
}

fn io_err(action: &str, e: std::io::Error) -> Box<dyn Error + Send + Sync> {
    format!("{action} failed: {e}").into()
}

#[cfg(test)]
mod tests {
    use super::{DrmConnectionStatus, DrmMode};
    use std::ffi::CStr;

    #[test]
    fn connection_status_maps_drm_states() {
        assert_eq!(
            DrmConnectionStatus::from(drm::control::connector::State::Connected),
            DrmConnectionStatus::Connected,
        );
        assert_eq!(
            DrmConnectionStatus::from(drm::control::connector::State::Disconnected),
            DrmConnectionStatus::Disconnected,
        );
        assert_eq!(
            DrmConnectionStatus::from(drm::control::connector::State::Unknown),
            DrmConnectionStatus::Unknown,
        );
    }

    #[test]
    fn mode_name_is_read_from_c_string() {
        let name = CStr::from_bytes_with_nul(b"800x128\0").unwrap();
        let mode = DrmMode {
            name: name.to_string_lossy().into_owned(),
            clock: 50000,
            width: 800,
            height: 128,
            vrefresh: 60,
        };
        assert_eq!(mode.name(), "800x128");
        assert_eq!(mode.width(), 800);
        assert_eq!(mode.height(), 128);
        assert_eq!(mode.clock(), 50000);
        assert_eq!(mode.vrefresh(), 60);
    }
}
