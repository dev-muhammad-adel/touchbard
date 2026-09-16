//! DRM/KMS backend for the Touchbard framework.
//!
//! The backend presents CPU-rendered frames through a single dumb buffer.
//!
//! # Orientation
//!
//! The physical panel is landscape (`2008×60`) but the DRM framebuffer the
//! driver exposes is its transpose (`60×2008`, `RIGHT_UP`). Content is
//! therefore stored rotated 90° clockwise by default (see [`ScanoutOrientation`]
//! and [`convert::convert_frame`]); the viewport handed to the UI uses the
//! physical, rotated-back dimensions.

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;
use std::thread;

use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::Device as ControlDevice;
use drm::Device as DrmDeviceTrait;
use touchbard_renderer::{Backend, Frame, FrameSource, Viewport};

pub mod convert;
pub mod device;
pub mod discovery;
pub mod dumbbuffer;
pub mod framebuffer;
mod lifecycle;
pub mod modeset;
pub mod pattern;
pub mod preparer;
pub mod resources;
pub mod sysfs;
pub mod touchbar;
pub mod wakefd;

pub use convert::*;
pub use device::*;
pub use discovery::*;
pub use dumbbuffer::*;
pub use framebuffer::*;
pub use modeset::*;
pub use pattern::*;
pub use preparer::*;
pub use resources::*;
pub use sysfs::*;
pub use touchbar::*;
pub use wakefd::*;

use crate::dumbbuffer::BPP;
use crate::framebuffer::DEPTH;

/// UI scale factor: physical pixels per logical pixel.
const SCALE_FACTOR: f64 = 1.0;

/// Configuration for the DRM backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrmConfig {
    /// Store frame content rotated 90° clockwise into the (transposed) DRM
    /// buffer. The Touch Bar's panel is landscape but its framebuffer is the
    /// transpose, so this is `true` by default. Set to `false` for panels that
    /// expose an upright buffer of the UI's size.
    pub transpose: bool,
}

impl Default for DrmConfig {
    fn default() -> Self {
        Self { transpose: true }
    }
}

impl DrmConfig {
    /// Read the configuration from the environment:
    ///
    /// - `TOUCHBARD_DRM_TRANSPOSE`: `0`/`false`/`no` disables the 90° clockwise
    ///   transpose described on [`Self::transpose`]; anything else enables it.
    pub fn from_env() -> Self {
        let transpose = match std::env::var("TOUCHBARD_DRM_TRANSPOSE") {
            Ok(v) => !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            ),
            Err(_) => true,
        };
        Self { transpose }
    }
}

/// An active DRM scanout and its kernel resources.
pub struct Scanout {
    device: DrmDevice,
    buffer: drm::control::dumbbuffer::DumbBuffer,
    framebuffer: drm::control::framebuffer::Handle,
    config: DrmDisplayConfig,
    orientation: ScanoutOrientation,
    width: u16,
    height: u16,
    pitch: u32,
}

impl Scanout {
    pub fn framebuffer(&self) -> drm::control::framebuffer::Handle {
        self.framebuffer
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn pitch(&self) -> u32 {
        self.pitch
    }

    pub fn orientation(&self) -> ScanoutOrientation {
        self.orientation
    }

    pub fn viewport_size(&self) -> (u16, u16) {
        self.orientation.viewport_size(self.width, self.height)
    }
}

impl Drop for Scanout {
    fn drop(&mut self) {
        // The CRTC must be disabled before its framebuffer is destroyed.
        if let Err(e) = modeset::disable_crtc(&self.device, &self.config) {
            eprintln!("teardown: disabling CRTC failed (ignored): {e}");
        }
        if let Err(e) = self.device.destroy_framebuffer(self.framebuffer) {
            eprintln!(
                "teardown: destroying framebuffer {} failed (ignored): {e}",
                u32::from(self.framebuffer)
            );
        }
        if let Err(e) = self.device.destroy_dumb_buffer(self.buffer) {
            eprintln!("teardown: destroying dumb buffer failed (ignored): {e}");
        }
    }
}

/// The DRM/KMS [`Backend`].
pub struct DrmBackend {
    config: DrmConfig,
    scanout: Option<Scanout>,
}

impl DrmBackend {
    pub fn new(config: DrmConfig) -> Self {
        Self {
            config,
            scanout: None,
        }
    }

    pub fn from_env() -> Self {
        Self::new(DrmConfig::from_env())
    }

    /// The configuration backing this backend.
    pub fn config(&self) -> &DrmConfig {
        &self.config
    }

    /// The active scanout, if [`Backend::initialize`] has succeeded.
    pub fn scanout(&self) -> Option<&Scanout> {
        self.scanout.as_ref()
    }

    /// Run the discovery → device → resources → modeset flow and return an
    /// active [`Scanout`] whose CRTC is already presenting the allocated
    /// (still black) framebuffer.
    fn create_scanout(config: DrmConfig) -> Result<Scanout, Box<dyn Error + Send + Sync>> {
        let orientation = if config.transpose {
            ScanoutOrientation::Transpose
        } else {
            ScanoutOrientation::Normal
        };

        let mut hardware_discovery = SysfsHardwareDiscovery::default();
        let mut card_discovery = SysfsDrmCardDiscovery::new();
        let card = discover_or_prepare(&mut hardware_discovery, &mut card_discovery, TOUCHBAR_ID)?;

        let device = DrmDevice::open(&card.device_path)?;
        let resources = DrmResources::query(&device)?;
        let display_config = select_display_config(&device, &resources)?;

        let (buffer_width, buffer_height) = (
            display_config.mode().size().0,
            display_config.mode().size().1,
        );
        let mut buffer = device
            .create_dumb_buffer(
                (u32::from(buffer_width), u32::from(buffer_height)),
                DrmFourcc::Abgr8888,
                BPP,
            )
            .map_err(|e| {
                io_err(
                    &format!("create_dumb_buffer({buffer_width}x{buffer_height})"),
                    e,
                )
            })?;
        let pitch = buffer.pitch();

        // Validate the CPU mapping covers the whole pixel area (`pitch ×
        // height`). The kernel may page-align the mapping upward; that extra
        // tail is left untouched by the converter.
        let pixel_bytes = usize::from(buffer_height) * pitch as usize;
        {
            let mapping = device.map_dumb_buffer(&mut buffer).map_err(|e| {
                io_err(
                    &format!("map_dumb_buffer({buffer_width}x{buffer_height})"),
                    e,
                )
            })?;
            if mapping.len() < pixel_bytes {
                return Err(format!(
                    "dumb buffer mapping too small: {} bytes, need {pixel_bytes} (pitch {pitch} × height {buffer_height})",
                    mapping.len()
                )
                .into());
            }
        }

        let framebuffer = device.add_framebuffer(&buffer, DEPTH, BPP).map_err(|e| {
            io_err(
                &format!("add_framebuffer({buffer_width}x{buffer_height})"),
                e,
            )
        })?;

        device
            .acquire_master_lock()
            .map_err(|e| io_err("acquire_master_lock (needs root or the drm group)", e))?;
        initial_modeset(&device, framebuffer, &display_config)?;
        Ok(Scanout {
            device,
            buffer,
            framebuffer,
            config: display_config,
            orientation,
            width: buffer_width,
            height: buffer_height,
            pitch,
        })
    }
}

impl Backend for DrmBackend {
    fn initialize(&mut self) -> Result<Viewport, Box<dyn Error + Send + Sync>> {
        let scanout = Self::create_scanout(self.config)?;
        let (width, height) = scanout.viewport_size();
        self.scanout = Some(scanout);
        Ok(Viewport {
            width: u32::from(width),
            height: u32::from(height),
            scale_factor: SCALE_FACTOR,
        })
    }

    fn run(
        &mut self,
        source: Rc<RefCell<dyn FrameSource>>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut scanout = Some(
            self.scanout
                .take()
                .ok_or_else(|| "the DRM backend is not initialized".to_string())?,
        );
        let wake = WakeFd::new()?;
        let waker = wake.waker();
        let mut lifecycle = match lifecycle::SleepWatcher::start(&wake) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                eprintln!("DRM lifecycle watcher unavailable: {error}");
                None
            }
        };
        let mut suspended = false;
        let mut repaint_after_resume = false;
        let mut last_frame: Option<Frame> = None;

        loop {
            let animating = source.borrow().needs_redraw();
            let budget = if animating {
                Some(wakefd::ANIM_TICK)
            } else {
                None
            };
            wake.wait(budget)?;

            while let Some(event) = lifecycle.as_ref().and_then(|watcher| watcher.try_recv()) {
                match event {
                    lifecycle::SleepEvent::PrepareForSleep(true) if !suspended => {
                        eprintln!("DRM suspend: releasing Touch Bar display resources");
                        drop(scanout.take());
                        if let Some(watcher) = lifecycle.as_mut() {
                            watcher.release_inhibitor();
                        }
                        suspended = true;
                    }
                    lifecycle::SleepEvent::PrepareForSleep(false) if suspended => {
                        eprintln!("DRM resume: rediscovering Touch Bar display");
                        if let Some(watcher) = lifecycle.as_mut() {
                            watcher.acquire_inhibitor()?;
                        }
                        loop {
                            match Self::create_scanout(self.config) {
                                Ok(new_scanout) => {
                                    scanout = Some(new_scanout);
                                    suspended = false;
                                    repaint_after_resume = true;
                                    break;
                                }
                                Err(error) => {
                                    eprintln!("DRM resume: display not ready, retrying: {error}");
                                    thread::sleep(std::time::Duration::from_millis(250));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            if suspended {
                continue;
            }

            let frame = if repaint_after_resume {
                repaint_after_resume = false;
                last_frame.clone()
            } else {
                source.borrow_mut().frame(Some(waker))
            };
            if let Some(frame) = frame {
                last_frame = Some(frame.clone());
                let scanout = scanout
                    .as_mut()
                    .ok_or_else(|| "DRM scanout unavailable outside suspend".to_string())?;
                let width = scanout.width;
                let height = scanout.height;
                let pitch = scanout.pitch;
                let orientation = scanout.orientation;
                let framebuffer = scanout.framebuffer;
                let clip = [drm::control::ClipRect::new(0, 0, width, height)];
                let conversion = {
                    let mut mapping = scanout.device.map_dumb_buffer(&mut scanout.buffer)?;
                    convert::convert_frame(&frame, &mut mapping, width, height, pitch, orientation)
                };
                if let Err(e) = conversion {
                    eprintln!("WARN: dropping frame (conversion failed): {e}");
                } else if let Err(e) = scanout.device.dirty_framebuffer(framebuffer, &clip) {
                    eprintln!("WARN: dirty_framebuffer failed (frame not refreshed): {e}");
                }
            }
        }
    }
}

fn io_err(action: &str, e: std::io::Error) -> Box<dyn Error + Send + Sync> {
    format!("{action} failed: {e}").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_transposes_for_the_touch_bar() {
        let config = DrmConfig::default();
        assert!(config.transpose, "Touch Bar content must be stored rotated");
    }

    /// `TOUCHBARD_DRM_TRANSPOSE` is read from the environment; false-like
    /// values disable the transpose.
    #[test]
    fn from_env_parses_transpose_flag() {
        for (value, expected) in [
            ("1", true),
            ("true", true),
            ("yes", true),
            ("on", true),
            ("0", false),
            ("false", false),
            ("no", false),
            ("off", false),
        ] {
            std::env::set_var("TOUCHBARD_DRM_TRANSPOSE", value);
            assert_eq!(
                DrmConfig::from_env().transpose,
                expected,
                "TOUCHBARD_DRM_TRANSPOSE={value}"
            );
        }
        std::env::remove_var("TOUCHBARD_DRM_TRANSPOSE");
        assert!(DrmConfig::from_env().transpose, "defaults to transpose");
    }

    #[test]
    fn scanout_viewport_size_applies_the_orientation() {
        assert_eq!(
            ScanoutOrientation::Transpose.viewport_size(60, 2008),
            (2008, 60)
        );
        assert_eq!(
            ScanoutOrientation::Normal.viewport_size(60, 2008),
            (60, 2008)
        );
    }
}
