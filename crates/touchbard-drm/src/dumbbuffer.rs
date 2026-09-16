//! DRM dumb buffer allocation and mapping.
//!
//! A dumb buffer is the DRM subsystem's CPU-accessible, cross-driver fallback
//! buffer: it lives in kernel memory (a GEM object), is filled from userspace,
//! and is displayed by the card without any GPU-specific code. That makes it
//! the natural target for TouchBard's CPU-rendered frames (premultiplied RGBA8).
//!
//! This module owns the allocation through [`DrmDumbBuffer`], an RAII wrapper
//! that frees the kernel buffer exactly once, when dropped, and maps it for CPU
//! access through [`DrmDumbBuffer::map`], returning a [`DrmDumbBufferMapping`]
//! that unmaps on drop. Framebuffer creation and modeset build on the result in
//! [`crate::framebuffer`] and [`crate::modeset`].

use std::error::Error;
use std::fmt;
use std::ops::{Deref, DerefMut};

use drm::buffer::{self, Buffer as _};
use drm::control::Device as ControlDevice;

use crate::device::DrmDevice;
use crate::resources::DrmMode;

/// Bits per pixel of a [`DrmDumbBuffer`].
pub(crate) const BPP: u32 = 32;

/// An owned, kernel-backed, CPU-accessible DRM dumb buffer.
///
/// Created from the exact dimensions of a [`DrmMode`] via
/// [`DrmDumbBuffer::create`]. The buffer borrows the [`DrmDevice`] it was
/// created from (the lifetime enforces the device outlives the buffer) and,
/// when dropped, the kernel-side buffer is destroyed through
/// `DRM_IOCTL_MODE_DESTROYDUMB`.
///
/// The buffer is not itself a scanout source: it must first be attached to a
/// DRM framebuffer (see [`crate::framebuffer`]). It can be mapped for CPU
/// access with [`DrmDumbBuffer::map`].
pub struct DrmDumbBuffer<'a> {
    device: &'a DrmDevice,
    buffer: drm::control::dumbbuffer::DumbBuffer,
}

impl fmt::Debug for DrmDumbBuffer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrmDumbBuffer")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("pitch", &self.pitch())
            .field("bpp", &self.bpp())
            .field("size", &self.size())
            .field("handle", &self.handle())
            .finish_non_exhaustive()
    }
}

impl<'a> DrmDumbBuffer<'a> {
    /// Allocate a dumb buffer sized exactly by `mode`, in 32bpp `Abgr8888`
    /// (premultiplied RGBA8, matching the Vello CPU renderer's output).
    pub fn create(
        device: &'a DrmDevice,
        mode: &DrmMode,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let (width, height) = (u32::from(mode.width()), u32::from(mode.height()));
        let format = buffer::DrmFourcc::Abgr8888;
        let buffer = device
            .create_dumb_buffer((width, height), format, BPP)
            .map_err(|e| {
                io_err(
                    format!("create_dumb_buffer({width}x{height}, bpp {BPP})"),
                    e,
                )
            })?;
        Ok(Self { device, buffer })
    }

    /// Handle used to identify this buffer to the kernel.
    pub fn handle(&self) -> buffer::Handle {
        self.buffer.handle()
    }

    /// The allocated width in pixels (matches the mode width).
    pub fn width(&self) -> u16 {
        self.buffer.size().0 as u16
    }

    /// The allocated height in pixels (matches the mode height).
    pub fn height(&self) -> u16 {
        self.buffer.size().1 as u16
    }

    /// The pitch (stride) in bytes.
    pub fn pitch(&self) -> u32 {
        self.buffer.pitch()
    }

    /// Bits per pixel this buffer was allocated with.
    pub fn bpp(&self) -> u32 {
        BPP
    }

    /// Total size of the buffer's pixel data in bytes (`pitch * height`).
    pub fn size(&self) -> usize {
        usize::from(self.height()) * self.pitch() as usize
    }

    /// The pixel format this buffer was allocated with.
    pub fn format(&self) -> buffer::DrmFourcc {
        self.buffer.format()
    }

    /// Map the buffer into CPU address space.
    ///
    /// The returned [`DrmDumbBufferMapping`] unmaps the buffer automatically
    /// when dropped, and is tied to this buffer's lifetime: the mapping cannot
    /// outlive the [`DrmDumbBuffer`] (and therefore the [`DrmDevice`]).
    pub fn map(&mut self) -> Result<DrmDumbBufferMapping<'_>, Box<dyn Error + Send + Sync>> {
        let (width, height, pitch) = (self.width(), self.height(), self.pitch());
        let mapping = self
            .device
            .map_dumb_buffer(&mut self.buffer)
            .map_err(|e| io_err(format!("map_dumb_buffer({width}x{height})"), e))?;
        Ok(DrmDumbBufferMapping {
            mapping,
            width,
            height,
            pitch,
        })
    }

    pub(crate) fn as_dumb_buffer(&self) -> &drm::control::dumbbuffer::DumbBuffer {
        &self.buffer
    }
}

/// A CPU mapping of a [`DrmDumbBuffer`].
///
/// Obtained through [`DrmDumbBuffer::map`]. Unmaps automatically on drop
/// (`munmap` via the `drm` crate), and its lifetime is bounded by the buffer
/// it maps, which in turn is bounded by the [`DrmDevice`] that owns the
/// underlying GEM object.
///
/// The mapping is a flat byte range of the kernel-reported buffer size;
/// rows are laid out with [`DrmDumbBuffer::pitch`]-byte stride, which must not
/// be assumed to equal `width * 4`. Access the bytes through [`Deref`] /
/// [`DerefMut`] / [`AsRef`] / [`AsMut`], or the [`len`](DrmDumbBufferMapping::len)
/// accessor, always staying within the mapped length.
pub struct DrmDumbBufferMapping<'a> {
    mapping: drm::control::dumbbuffer::DumbMapping<'a>,
    width: u16,
    height: u16,
    pitch: u32,
}

impl fmt::Debug for DrmDumbBufferMapping<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrmDumbBufferMapping")
            .field("len", &self.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pitch", &self.pitch)
            .finish_non_exhaustive()
    }
}

impl DrmDumbBufferMapping<'_> {
    /// The number of mapped bytes (the kernel-reported buffer size).
    pub fn len(&self) -> usize {
        self.mapping.len()
    }

    /// Whether the mapping is empty.
    pub fn is_empty(&self) -> bool {
        self.mapping.is_empty()
    }

    /// The pixel width of the mapped buffer.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// The pixel height of the mapped buffer.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// The row stride in bytes of the mapped buffer.
    pub fn pitch(&self) -> u32 {
        self.pitch
    }
}

impl AsRef<[u8]> for DrmDumbBufferMapping<'_> {
    fn as_ref(&self) -> &[u8] {
        self.mapping.as_ref()
    }
}

impl AsMut<[u8]> for DrmDumbBufferMapping<'_> {
    fn as_mut(&mut self) -> &mut [u8] {
        self.mapping.as_mut()
    }
}

impl Deref for DrmDumbBufferMapping<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.mapping.deref()
    }
}

impl DerefMut for DrmDumbBufferMapping<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.mapping.deref_mut()
    }
}

impl Drop for DrmDumbBuffer<'_> {
    fn drop(&mut self) {
        // `DumbBuffer` is `Copy`, so hand the device a copy rather than
        // draining this field. `Drop` runs exactly once, so the kernel-side
        // buffer is destroyed exactly once, regardless of how the wrapper was
        // moved.
        let buffer = self.buffer;
        if let Err(e) = self.device.destroy_dumb_buffer(buffer) {
            eprintln!(
                "failed to destroy DRM dumb buffer ({}x{}): {e}",
                self.width(),
                self.height(),
            );
        }
    }
}

fn io_err(action: String, e: std::io::Error) -> Box<dyn Error + Send + Sync> {
    format!("{action} failed: {e}").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpp_and_format_match_premultiplied_rgba8() {
        assert_eq!(BPP, 32);
        assert_eq!(buffer::DrmFourcc::Abgr8888.to_string(), "AB24");
    }
}
