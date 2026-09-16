//! Framebuffer types describing the RGBA pixel output shared by every backend.

/// The pixel format of a rendered frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PixelFormat {
    /// 8 bits per channel, in R,G,B,A order, premultiplied alpha.
    ///
    /// This is the canonical format of a [`Frame`]: `vello_cpu`'s
    /// `RenderContext::render_to_buffer` (via `anyrender_vello_cpu`) writes
    /// premultiplied RGBA8, so the bytes are passed through untouched. A backend
    /// that needs a different format (e.g. straight alpha for a browser canvas)
    /// must convert at its own boundary, never inside the renderer.
    #[default]
    Rgba8,
}

impl PixelFormat {
    /// Number of bytes per pixel for this format.
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            PixelFormat::Rgba8 => 4,
        }
    }
}

/// A rendered RGBA framebuffer.
///
/// Row data is tightly packed; `stride == width * bytes_per_pixel()` by default,
/// but cropped/offset framebuffers may use a wider stride.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: PixelFormat,
    /// Raw pixel bytes. Length must be `stride * height`.
    pub data: Vec<u8>,
}

impl Frame {
    /// Create a new, zero-initialized frame.
    pub fn new(width: u32, height: u32, format: PixelFormat) -> Self {
        let stride = width as usize * format.bytes_per_pixel();
        let data = vec![0; stride * height as usize];
        Self {
            width,
            height,
            stride,
            format,
            data,
        }
    }

    /// Create a frame with a custom stride.
    pub fn with_stride(width: u32, height: u32, stride: usize, format: PixelFormat) -> Self {
        let data = vec![0; stride * height as usize];
        Self {
            width,
            height,
            stride,
            format,
            data,
        }
    }

    /// The minimum stride required for the given width.
    pub fn minimum_stride(&self) -> usize {
        self.width as usize * self.format.bytes_per_pixel()
    }

    /// Access the pixel at (x, y) as 4 consecutive bytes (R, G, B, A).
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (y as usize) * self.stride + (x as usize) * self.format.bytes_per_pixel();
        Some([
            *self.data.get(offset)?,
            *self.data.get(offset + 1)?,
            *self.data.get(offset + 2)?,
            *self.data.get(offset + 3)?,
        ])
    }

    /// Number of bytes in the framebuffer.
    pub fn byte_len(&self) -> usize {
        self.stride * self.height as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_dimensions() {
        let frame = Frame::new(1080, 30, PixelFormat::Rgba8);
        assert_eq!(frame.width, 1080);
        assert_eq!(frame.height, 30);
        assert_eq!(frame.stride, 1080 * 4);
        assert_eq!(frame.byte_len(), 1080 * 30 * 4);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(frame.pixel(1080, 0), None);
        assert_eq!(frame.pixel(0, 30), None);
    }

    #[test]
    fn test_pixel_format() {
        assert_eq!(PixelFormat::Rgba8.bytes_per_pixel(), 4);
        assert_eq!(PixelFormat::default(), PixelFormat::Rgba8);
    }

    #[test]
    fn test_custom_stride() {
        let frame = Frame::with_stride(4, 2, 32, PixelFormat::Rgba8);
        assert_eq!(frame.stride, 32);
        assert_eq!(frame.byte_len(), 64);
    }
}
