//! DRM/KMS backend for the Touchbard framework.
//!
//! The backend is structured in three layers:
//!
//! ```text
//! DrmBackend
//!     |
//!     +--> hardware discovery   ([`discovery::HardwareDiscovery`]: does the
//!     |                          hardware identified by VID/PID exist?)
//!     +--> DRM card discovery   ([`discovery::DrmCardDiscovery`]: has the
//!     |                          kernel exposed a DRM card for it?)
//!     |
//!     +--> DevicePreparer       (opt-in hardware workaround, only when needed)
//!     |
//!     +--> DrmDevice            (opened/owned in [`device`]; usable with the
//!     |                          safe `drm` crate by implementing its
//!     |                          [`drm::Device`] trait)
//!     +--> KMS resources         ([`resources`]: connectors, their modes,
//!     |                          encoders, and CRTCs of the card)
//!     +--> display config        ([`modeset::select_display_config`]: the
//!     |                          connected connector + encoder + CRTC + mode)
//!     +--> Scanout               (this module: device + dumb buffer +
//!     |                          framebuffer + orientation, modeset, CPU
//!     |                          presentation loop)
//! ```
//!
//! The two discovery operations are separate. [`discovery::discover_or_prepare`]
//! first finds the target *hardware* by its [`HardwareId`]; only if the
//! hardware exists does it look for the hardware's DRM *card*. A missing DRM
//! card never automatically triggers a workaround: one runs only when the
//! hardware was found, its DRM card is missing, and that [`HardwareId`] has a
//! known workaround ([`preparer::workaround_for`]). Unknown hardware that
//! cannot be found, or unknown hardware whose DRM card is missing, yields an
//! unavailable error - the flow never guesses a workaround.
//!
//! The backend has no GBM/EGL allocator, no page flips, and no double
//! buffering. It presents CPU-rendered frames through a single dumb buffer and
//! a full-screen [`drm::control::Device::dirty_framebuffer`] refresh each
//! cycle, which is exactly what the t2bdrm/appletbdrm Touch Bar driver expects.
//!
//! # Lifecycle
//!
//! [`DrmBackend::initialize`] runs the full discovery flow, opens the device,
//! queries resources, selects a display configuration, allocates the dumb
//! buffer (validating its CPU mapping), creates the framebuffer, acquires DRM
//! master, performs the initial modeset, and returns the **physical** viewport
//! (landscape, e.g. `2008×60`) for the UI system. [`DrmBackend::run`] then
//! drives an event loop: block on a single wake source (see [`wakefd`]) →
//! poll the frame source → convert ([`ScanoutOrientation`]) → DirtyFB, with a
//! bounded wait (~60 Hz) only while the document is animating. Idle UI renders
//! nothing and takes no CPU.
//!
//! The mapping is remapped per `run` call and unmapped on drop; teardown
//! releases the CRTC, then destroys the framebuffer and dumb buffer - all RAII
//! through [`Scanout`]'s `Drop`.
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

use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::Device as ControlDevice;
use drm::Device as DrmDeviceTrait;
use touchbard_renderer::{Backend, FrameSource, Viewport};

pub mod convert;
pub mod device;
pub mod discovery;
pub mod dumbbuffer;
pub mod framebuffer;
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
///
/// The `control-center` pages are authored at 2008×60 logical pixels (1:1 with
/// the Touch Bar's native resolution), so scanout maps 1:1.
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

/// An active DRM scanout: the opened device plus the buffer/framebuffer bound
/// to it, ready to present frames.
///
/// Owned by [`DrmBackend`] after [`DrmBackend::initialize`] succeeds and
/// released by its `Drop` on teardown - the kernel-side resources (framebuffer,
/// dumb buffer) are destroyed exactly once, in order, before the device closes.
///
/// The device is stored by value (`DrmDevice`), so the dumb buffer and
/// framebuffer are stored as their raw kernel handles rather than the
/// borrow-lifetime wrappers in [`crate::framebuffer`]/[`crate::dumbbuffer`]:
/// the buffer geometry is what matters during `run`, and teardown only needs
/// the device plus handles. This avoids a self-referential struct.
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
    /// The DRM framebuffer handle being scanned out.
    pub fn framebuffer(&self) -> drm::control::framebuffer::Handle {
        self.framebuffer
    }

    /// The dumb-buffer width in pixels (the DRM framebuffer's width).
    pub fn width(&self) -> u16 {
        self.width
    }

    /// The dumb-buffer height in pixels (the DRM framebuffer's height).
    pub fn height(&self) -> u16 {
        self.height
    }

    /// The dumb buffer's row pitch in bytes.
    pub fn pitch(&self) -> u32 {
        self.pitch
    }

    /// The orientation frames are stored with.
    pub fn orientation(&self) -> ScanoutOrientation {
        self.orientation
    }

    /// The physical (panel) size the UI must be built at: the orientation's
    /// viewport size of the DRM framebuffer.
    pub fn viewport_size(&self) -> (u16, u16) {
        self.orientation.viewport_size(self.width, self.height)
    }
}

impl Drop for Scanout {
    fn drop(&mut self) {
        // Unbind the CRTC first, so the kernel is no longer scanning the
        // framebuffer by the time it is destroyed.
        if let Err(e) = modeset::disable_crtc(&self.device, &self.config) {
            eprintln!("teardown: disabling CRTC failed (ignored): {e}");
        }
        // Destroy the framebuffer.
        if let Err(e) = self.device.destroy_framebuffer(self.framebuffer) {
            eprintln!(
                "teardown: destroying framebuffer {} failed (ignored): {e}",
                u32::from(self.framebuffer)
            );
        }
        // Destroy the dumb buffer. The mapping unmapped when `run` returned,
        // so no buffer is left pinned.
        if let Err(e) = self.device.destroy_dumb_buffer(self.buffer) {
            eprintln!("teardown: destroying dumb buffer failed (ignored): {e}");
        }
        // The device's fd closes as this field drops, releasing DRM master.
    }
}

/// The DRM/KMS [`Backend`].
pub struct DrmBackend {
    config: DrmConfig,
    scanout: Option<Scanout>,
}

impl DrmBackend {
    /// Build the DRM backend from its configuration.
    pub fn new(config: DrmConfig) -> Self {
        Self {
            config,
            scanout: None,
        }
    }

    /// Build the backend from `DrmConfig::from_env()`.
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
        let scanout = self
            .scanout
            .as_mut()
            .ok_or_else(|| "the DRM backend is not initialized".to_string())?;

        // The mapping borrows only `scanout.buffer`, so the device (used below
        // for DirtyFB) stays independently accessible - no self-borrow.
        let mut mapping = scanout.device.map_dumb_buffer(&mut scanout.buffer)?;
        let width = scanout.width;
        let height = scanout.height;
        let pitch = scanout.pitch;
        let orientation = scanout.orientation;
        let framebuffer = scanout.framebuffer;
        let clip = [drm::control::ClipRect::new(0, 0, width, height)];

        // The single wake source for this loop: the runtime arms it (Dioxus
        // scheduler + shell redraw bridge) and it fires whenever a new frame
        // is wanted. Input devices, when added later, join the same poll set.
        // `waker()` leaks one tiny boxed waker per run, so it is created once.
        let wake = WakeFd::new()?;
        let waker = wake.waker();

        // Deadine-based pacing experiment (A/B, temporary): with
        // `TOUCHBARD_DRM_PACING_TEST=1` the loop anchors each frame to an
        // absolute 60 Hz deadline and waits only for the remaining time, so a
        // frame that takes a few ms to render + transfer still lands on the
        // grid instead of being shifted by the full extra tick. Missed
        // deadlines advance the grid (no backlog); this is not the default.
        let deadline_pacing =
            std::env::var("TOUCHBARD_DRM_PACING_TEST").as_deref() == Ok("1");
        let mut next_deadline = std::time::Instant::now();
        if deadline_pacing {
            eprintln!("DRM pacing: deadline-based (TOUCHBARD_DRM_PACING_TEST=1)");
        }

        loop {
            // Wait for the next reason to render: indefinitely while the
            // document is idle, at most one animation tick while animating
            // (or, under the experiment, until the next absolute deadline).
            let animating = source.borrow().needs_redraw();
            let wait_start = std::time::Instant::now();
            if deadline_pacing && animating {
                // Absolute frame deadline: wait only until `next_deadline`, so
                // the produce + DirtyFB transfer below stays inside the frame
                // budget instead of stacking a full 16ms tick on top of it.
                let now = std::time::Instant::now();
                if now < next_deadline {
                    wake.wait(Some(next_deadline - now))?;
                }
            } else {
                let budget = if animating {
                    Some(wakefd::ANIM_TICK)
                } else {
                    None
                };
                wake.wait(budget)?;
            }
            touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Wait {
                wait_us: wait_start.elapsed().as_micros() as u64,
            });

            // One scheduling decision per wake: present only when the source
            // actually has a frame for us.
            if let Some(frame) = source.borrow_mut().frame(Some(waker)) {
                let present_start = std::time::Instant::now();
                if let Err(e) =
                    convert::convert_frame(&frame, &mut mapping, width, height, pitch, orientation)
                {
                    eprintln!("WARN: dropping frame (conversion failed): {e}");
                } else if let Err(e) = scanout.device.dirty_framebuffer(framebuffer, &clip) {
                    eprintln!("WARN: dirty_framebuffer failed (frame not refreshed): {e}");
                }
                touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Present {
                    present_us: present_start.elapsed().as_micros() as u64,
                });
            }

            if deadline_pacing {
                // Advance the deadline in whole frame periods; never park it
                // behind `now` (a missed deadline costs one frame, not a
                // growing backlog).
                while next_deadline <= std::time::Instant::now() {
                    next_deadline += wakefd::FRAME_PERIOD;
                }
            }
        }
    }
}

/// Wrap an ioctl [`std::io::Error`] with the action that failed, for clean
/// error chaining toward `Box<dyn Error + Send + Sync>`.
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
        // The Touch Bar exposes a 60×2008 framebuffer; the UI must use the
        // physical 2008×60 landscape.
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
