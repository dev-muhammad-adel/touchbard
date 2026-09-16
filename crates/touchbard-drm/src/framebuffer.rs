//! DRM framebuffer creation.
//!
//! A DRM framebuffer is the object that makes a buffer (here: a
//! [`DrmDumbBuffer`]) a scanout source: it binds the buffer's GEM handle,
//! pitch, dimensions and pixel format so a CRTC can display it. It is created
//! from the buffer and lives exactly as long as the [`DrmFramebuffer`] RAII
//! wrapper: destruction (`DRM_IOCTL_MODE_RMFB`) happens once, on drop, and
//! always runs before the wrapping crate drops the dumb buffer it was built
//! from.

use std::error::Error;
use std::fmt;

use drm::control::{framebuffer, Device as ControlDevice};

use crate::device::DrmDevice;
use crate::dumbbuffer::DrmDumbBuffer;

/// Legacy framebuffer depth arguments passed to `add_framebuffer`.
///
/// `drm 0.15.0` exposes only the legacy `DRM_MODE_ADDFB` ioctl for dumb
/// buffers (24-bit color depth, 32-bit pixel) - it carries no FourCC and
/// derives the pixel format from `depth`/`bpp`. The buffer's pitch is taken
/// from the [`DrmDumbBuffer`] itself, so no assumption about tightly packed
/// rows is made.
pub(crate) const DEPTH: u32 = 24;

/// An owned DRM framebuffer bound to a [`DrmDumbBuffer`].
///
/// Created via [`DrmFramebuffer::create`]. Holds the frame's kernel handle and
/// a reference to the [`DrmDevice`] that owns it (the device must outlive the
/// framebuffer, which the lifetime enforces). Dropping the framebuffer
/// destroys it in the kernel; because the framebuffer holds a reference to the
/// dumb buffer's GEM object, it must be dropped before the dumb buffer.
pub struct DrmFramebuffer<'a> {
    device: &'a DrmDevice,
    handle: framebuffer::Handle,
}

impl fmt::Debug for DrmFramebuffer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrmFramebuffer")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl<'a> DrmFramebuffer<'a> {
    /// Create a framebuffer from `buffer`.
    pub fn create(
        device: &'a DrmDevice,
        buffer: &DrmDumbBuffer<'_>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let handle = device
            .add_framebuffer(buffer.as_dumb_buffer(), DEPTH, buffer.bpp())
            .map_err(|e| {
                io_err(
                    format!(
                        "add_framebuffer({}x{}, depth {}, bpp {})",
                        buffer.width(),
                        buffer.height(),
                        DEPTH,
                        buffer.bpp(),
                    ),
                    e,
                )
            })?;
        Ok(Self { device, handle })
    }

    /// The handle to this framebuffer.
    pub fn handle(&self) -> framebuffer::Handle {
        self.handle
    }

    /// Query the kernel for details of this framebuffer.
    pub fn info(&self) -> Result<framebuffer::Info, Box<dyn Error + Send + Sync>> {
        self.device
            .get_framebuffer(self.handle)
            .map_err(|e| io_err(format!("get_framebuffer({})", u32::from(self.handle)), e))
    }
}

impl Drop for DrmFramebuffer<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.device.destroy_framebuffer(self.handle) {
            eprintln!(
                "failed to destroy DRM framebuffer {}: {e}",
                u32::from(self.handle),
            );
        }
    }
}

fn io_err(action: String, e: std::io::Error) -> Box<dyn Error + Send + Sync> {
    format!("{action} failed: {e}").into()
}
