//! DRM/KMS backend for the Touchbard framework.
//!
//! The backend renders into a single dumb buffer and presents a frame by
//! reporting its changed region to the driver through `dirty_framebuffer`.
//! This matches the t2bdrm/appletbdrm Touch Bar driver, which has no
//! framebuffer scanout: it is a USB bulk-transfer display that, on each atomic
//! commit, ships only the damaged rectangles to the device and waits for it to
//! acknowledge (`appletbdrm_flush_damage`). Sending the smallest changed
//! rectangle keeps each USB transfer tiny, which is what makes motion smooth;
//! reporting the whole buffer every frame would push ~360 KB per frame over
//! USB and introduce visible judder. For the same reason double buffering and
//! page flipping are counter-productive here — the driver treats a framebuffer
//! change as full-frame damage. Presentation is paced to the mode's vertical
//! refresh interval.
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
use std::time::{Duration, Instant};

use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::{ClipRect, Device as ControlDevice};
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

/// An active DRM presentation target and its kernel resources.
///
/// The t2bdrm/appletbdrm driver has no framebuffer scanout: it is a USB
/// bulk-transfer display that, for each atomic commit, sends only the
/// **damaged** rectangles of the framebuffer to the device and waits for it to
/// acknowledge. Presentation therefore uses a single dumb buffer and reports
/// the smallest changed rectangle through `dirty_framebuffer`, keeping every
/// USB transfer small and low-latency.
pub struct Scanout {
    device: DrmDevice,
    buffer: drm::control::dumbbuffer::DumbBuffer,
    framebuffer: drm::control::framebuffer::Handle,
    config: DrmDisplayConfig,
    orientation: ScanoutOrientation,
    width: u16,
    height: u16,
    pitch: u32,
    /// Frame interval implied by the mode's vertical refresh rate.
    vrefresh: Duration,
    /// Pixels of the previously presented frame, used to derive the damage
    /// rectangle of the next one.
    previous: Option<Vec<u8>>,
}

impl Scanout {
    /// The presented framebuffer.
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

    /// The display's vertical refresh interval.
    pub fn vrefresh(&self) -> Duration {
        self.vrefresh
    }

    pub fn viewport_size(&self) -> (u16, u16) {
        self.orientation.viewport_size(self.width, self.height)
    }

    /// Convert `frame` into the buffer and present its changed region.
    ///
    /// Returns `true` when a damage rectangle was committed (an update was
    /// sent to the device) and `false` when the frame was identical to the
    /// previous one, in which case nothing is transmitted.
    fn present(&mut self, frame: &Frame) -> Result<bool, Box<dyn Error + Send + Sync>> {
        let damage = self.damage_rect(frame);
        self.convert_into(frame)?;
        self.remember(frame);

        let Some(clip) = damage else {
            return Ok(false);
        };
        self.device
            .dirty_framebuffer(self.framebuffer, &[clip])
            .map_err(|e| io_err("dirty_framebuffer", e))?;
        Ok(true)
    }

    /// Copy `frame` into the dumb buffer.
    fn convert_into(&mut self, frame: &Frame) -> Result<(), Box<dyn Error + Send + Sync>> {
        let (width, height, pitch, orientation) =
            (self.width, self.height, self.pitch, self.orientation);
        let mut mapping = self.device.map_dumb_buffer(&mut self.buffer)?;
        convert::convert_frame(frame, &mut mapping, width, height, pitch, orientation)
    }

    /// Retain `frame`'s pixels so the next frame's damage can be derived.
    fn remember(&mut self, frame: &Frame) {
        match self.previous.as_mut() {
            Some(previous) if previous.len() == frame.data.len() => {
                previous.copy_from_slice(&frame.data);
            }
            _ => self.previous = Some(frame.data.clone()),
        }
    }

    /// The smallest buffer rectangle covering the pixels that differ between
    /// `frame` and the previously presented frame.
    ///
    /// Returns `None` when nothing changed, or the whole buffer when there is
    /// no usable previous frame (first frame, resume, or a geometry change).
    fn damage_rect(&self, frame: &Frame) -> Option<ClipRect> {
        let full = || ClipRect::new(0, 0, self.width, self.height);

        // The frame must have the geometry the buffer expects under the current
        // orientation, otherwise the rectangle mapping below is meaningless.
        let expected = match self.orientation {
            ScanoutOrientation::Normal => (usize::from(self.width), usize::from(self.height)),
            ScanoutOrientation::Transpose => (usize::from(self.height), usize::from(self.width)),
        };
        if (frame.width as usize, frame.height as usize) != expected {
            return Some(full());
        }

        let previous = match self.previous.as_ref() {
            Some(previous) if previous.len() == frame.data.len() => previous,
            _ => return Some(full()),
        };

        diff_bounds(frame, previous).map(|(u0, v0, u1, v1)| {
            frame_rect_to_buffer(self.orientation, self.width, u0, v0, u1, v1)
        })
    }
}

/// The bounding box `(u0, v0, u1, v1)` of the pixels that differ between
/// `frame` and `previous`, or `None` when they are identical.
///
/// `previous` must have the same layout as `frame.data` (guarded by the
/// caller).
fn diff_bounds(frame: &Frame, previous: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    let bpp = frame.format.bytes_per_pixel();
    let tight = fw * bpp;
    let stride = frame.stride;
    if stride < tight || previous.len() != frame.data.len() {
        return None;
    }

    let (mut min_u, mut max_u) = (fw, 0usize);
    let (mut min_v, mut max_v) = (fh, 0usize);
    for v in 0..fh {
        let row = v * stride;
        if frame.data[row..row + tight] == previous[row..row + tight] {
            continue;
        }

        // Narrow the changed span to its first and last differing pixels.
        let mut u0 = 0usize;
        while frame.data[row + u0 * bpp..row + u0 * bpp + bpp]
            == previous[row + u0 * bpp..row + u0 * bpp + bpp]
        {
            u0 += 1;
        }
        let mut u1 = fw;
        while u1 > u0
            && frame.data[row + (u1 - 1) * bpp..row + u1 * bpp]
                == previous[row + (u1 - 1) * bpp..row + u1 * bpp]
        {
            u1 -= 1;
        }

        min_u = min_u.min(u0);
        max_u = max_u.max(u1);
        min_v = min_v.min(v);
        max_v = v + 1;
    }

    if max_v == 0 {
        return None;
    }
    Some((min_u, min_v, max_u, max_v))
}

/// Map a frame pixel rectangle `[u0, u1) × [v0, v1)` to a buffer clip.
///
/// `buffer_width` is the framebuffer's width, which under
/// [`ScanoutOrientation::Transpose`] equals the frame height.
fn frame_rect_to_buffer(
    orientation: ScanoutOrientation,
    buffer_width: u16,
    u0: usize,
    v0: usize,
    u1: usize,
    v1: usize,
) -> ClipRect {
    match orientation {
        ScanoutOrientation::Normal => ClipRect::new(u0 as u16, v0 as u16, u1 as u16, v1 as u16),
        // dst(fx, fy) = src(fy, H − 1 − fx), so a frame column range maps to a
        // reversed buffer row range.
        ScanoutOrientation::Transpose => ClipRect::new(
            buffer_width - v1 as u16,
            u0 as u16,
            buffer_width - v0 as u16,
            u1 as u16,
        ),
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

        // After a USB config switch the DRM driver may not have finished
        // initialising the card node yet: sysfs reports the card and the
        // /dev node exists, but open() fails until the driver binds.
        let deadline = Instant::now() + Duration::from_secs(5);
        let device = loop {
            match DrmDevice::open(&card.device_path) {
                Ok(d) => break d,
                Err(e) if Instant::now() < deadline => {
                    eprintln!("DRM device not ready, retrying: {e}");
                    thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e),
            }
        };
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
        let vrefresh = refresh_interval(display_config.mode().vrefresh());

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
            vrefresh,
            previous: None,
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
        // Animation cadence deadline: one frame per display refresh. It is
        // advanced after every presented frame and resynced if rendering
        // overruns it, so a slow frame cannot turn into a hang-then-jump.
        let mut next_tick = Instant::now();

        loop {
            // Handle logind sleep events first, so a suspend/resume that
            // arrived during a wait is applied before touching the scanout.
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
                                    next_tick = Instant::now();
                                    break;
                                }
                                Err(error) => {
                                    eprintln!("DRM resume: display not ready, retrying: {error}");
                                    thread::sleep(Duration::from_millis(250));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            // A suspended session does not animate: block on the wake (the
            // logind watcher wakes the fd on resume) instead of polling.
            if suspended {
                let wait_start = Instant::now();
                wake.wait(None)?;
                touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Wait {
                    wait_us: wait_start.elapsed().as_micros() as u64,
                });
                continue;
            }

            let animating = source.borrow().needs_redraw();
            let vrefresh = scanout
                .as_ref()
                .map(|scanout| scanout.vrefresh)
                .unwrap_or(wakefd::ANIM_TICK);

            // Hold presentation to the display's refresh cadence. A host wake
            // during this wait (e.g. input) falls through so the change shows
            // immediately; the deadline itself is not shifted.
            if animating {
                let now = Instant::now();
                if now < next_tick {
                    let wait_start = Instant::now();
                    wake.wait(Some(next_tick - now))?;
                    touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Wait {
                        wait_us: wait_start.elapsed().as_micros() as u64,
                    });
                }
                if Instant::now().saturating_duration_since(next_tick) > vrefresh {
                    next_tick = Instant::now();
                }
            }

            let frame = if repaint_after_resume {
                repaint_after_resume = false;
                last_frame.clone()
            } else {
                source.borrow_mut().frame(Some(waker))
            };

            if let Some(frame) = frame {
                touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::SendStart);
                let present_start = Instant::now();
                let scanout = scanout
                    .as_mut()
                    .ok_or_else(|| "DRM scanout unavailable outside suspend".to_string())?;
                if let Err(error) = scanout.present(&frame) {
                    eprintln!("WARN: dropping frame (present failed): {error}");
                }
                touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Present {
                    present_us: present_start.elapsed().as_micros() as u64,
                });
                last_frame = Some(frame);
                if animating {
                    next_tick += vrefresh;
                    // If present() blocked longer than one tick (USB stall),
                    // next_tick is now in the past.  Without a floor the loop
                    // would immediately render and present another frame,
                    // piling up back-to-back 1 s stalls.  Clamping to
                    // now+vrefresh gives the USB bus time to clear before the
                    // next dirty_framebuffer ioctl.
                    let floor = Instant::now() + vrefresh;
                    if next_tick < floor {
                        next_tick = floor;
                    }
                }
            } else if animating {
                // An animated document should always answer `Some`; if it did
                // not, hold one refresh so the wait above cannot spin.
                next_tick = Instant::now() + vrefresh;
            } else {
                // Nothing to present: block until the host wakes us, but
                // never indefinitely — a timeout guarantees the loop
                // re-checks lifecycle and animation state even when no
                // input or scheduler wake arrives (mirrors tiny-dfr's
                // epoll ceiling).
                let wait_start = Instant::now();
                wake.wait(Some(Duration::from_secs(1)))?;
                touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Wait {
                    wait_us: wait_start.elapsed().as_micros() as u64,
                });
                continue;
            }
        }
    }
}

fn io_err(action: &str, e: std::io::Error) -> Box<dyn Error + Send + Sync> {
    format!("{action} failed: {e}").into()
}

/// The frame interval implied by a mode's vertical refresh rate.
///
/// Falls back to [`wakefd::ANIM_TICK`] when the driver reports no refresh rate,
/// so pacing stays bounded.
fn refresh_interval(vrefresh: u32) -> Duration {
    if vrefresh == 0 {
        wakefd::ANIM_TICK
    } else {
        Duration::from_secs_f64(1.0 / f64::from(vrefresh))
    }
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

    /// An RGBA8 frame of `width`×`height` filled with a single byte value.
    fn filled_frame(width: u32, height: u32, fill: u8) -> Frame {
        let mut frame = Frame::new(width, height, touchbard_renderer::PixelFormat::Rgba8);
        frame.data.fill(fill);
        frame
    }

    #[test]
    fn identical_frames_produce_no_damage() {
        let frame = filled_frame(8, 4, 0x20);
        assert_eq!(diff_bounds(&frame, &frame.data), None);
    }

    #[test]
    fn damage_bounds_cover_only_the_changed_pixels() {
        let base = filled_frame(8, 4, 0x20);
        let mut next = base.clone();
        let bpp = next.format.bytes_per_pixel();
        for v in 1..3 {
            for u in 5..7 {
                next.data[v * next.stride + u * bpp] ^= 0xFF;
            }
        }
        assert_eq!(diff_bounds(&next, &base.data), Some((5, 1, 7, 3)));
    }

    #[test]
    fn transpose_rect_maps_columns_to_reversed_rows() {
        // Buffer is 60 wide × 2008 tall, so H (the frame height) is 60.
        let clip = frame_rect_to_buffer(ScanoutOrientation::Transpose, 60, 100, 10, 123, 32);
        assert_eq!(
            (clip.x1(), clip.y1(), clip.x2(), clip.y2()),
            (28, 100, 50, 123)
        );
    }

    #[test]
    fn normal_rect_is_an_identity_mapping() {
        let clip = frame_rect_to_buffer(ScanoutOrientation::Normal, 60, 100, 10, 123, 32);
        assert_eq!(
            (clip.x1(), clip.y1(), clip.x2(), clip.y2()),
            (100, 10, 123, 32)
        );
    }

    /// The damage clip must contain the buffer pixel that `convert_frame`
    /// writes for the changed frame pixel.
    #[test]
    fn transpose_damage_contains_the_converted_pixel() {
        let base = filled_frame(4, 3, 0);
        let mut next = base.clone();
        let bpp = next.format.bytes_per_pixel();
        next.data[3 * bpp] = 0xAB; // frame pixel (u=3, v=0)

        let (u0, v0, u1, v1) = diff_bounds(&next, &base.data).expect("one pixel changed");
        assert_eq!((u0, v0, u1, v1), (3, 0, 4, 1));

        // Buffer is 3 wide × 4 tall (H = frame height = 3); convert_transpose
        // maps src(3, 0) to dst(fx = H−1−v = 2, fy = u = 3).
        let clip = frame_rect_to_buffer(ScanoutOrientation::Transpose, 3, u0, v0, u1, v1);
        assert!(
            clip.x1() <= 2 && 2 < clip.x2(),
            "clip must cover x=2: {clip:?}"
        );
        assert!(
            clip.y1() <= 3 && 3 < clip.y2(),
            "clip must cover y=3: {clip:?}"
        );
    }
}
