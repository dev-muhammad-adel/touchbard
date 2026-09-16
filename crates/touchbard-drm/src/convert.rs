//! Frame → DRM buffer conversion with explicit panel orientation.
//!
//! Converts the canonical [`Frame`] (premultiplied RGBA8) into the XRGB8888
//! byte layout (`B,G,R,X` in memory) that a legacy `DRM_MODE_ADDFB`
//! framebuffer scans out, and maps the landscape UI frame into the DRM
//! buffer's geometry according to the panel's [`ScanoutOrientation`].
//!
//! # Panel orientation
//!
//! The physical Touch Bar panel is landscape: `2008` wide × `60` tall. The DRM
//! framebuffer the t2bdrm/appletbdrm driver exposes is the transpose of that,
//! `60` wide × `2008` tall, and the driver attaches that buffer to the panel
//! with `drm_connector_set_panel_orientation(RIGHT_UP)`. To make a landscape
//! `Frame` appear correct on the panel, its content must be stored in the DRM
//! buffer rotated 90° clockwise:
//!
//! ```text
//! dst(fx = H − 1 − v, fy = u) = src(u, v)
//! ```
//!
//! with `H` the frame height (60), `u` the frame column, `v` the frame row,
//! and `dst(fx, fy)` a buffer pixel at column `fx`, row `fy`. That is
//! [`ScanoutOrientation::Transpose`], the default. The [`Backend`] viewport
//! must consequently use the *physical* dimensions (2008×60), swapped relative
//! to the DRM buffer.
//!
//! [`Backend`]: touchbard_renderer::Backend

use std::error::Error;
use std::fmt;

use touchbard_renderer::{Frame, PixelFormat};

/// How the UI [`Frame`] is mapped into the DRM framebuffer's byte area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanoutOrientation {
    /// `dst(fx, fy) = src(fx, fy)`: the buffer has the same dimensions as the
    /// frame. For panels that expose an upright buffer of the UI's size.
    Normal,
    /// `dst(fx, fy) = src(fy, H − 1 − fx)`: the landscape frame is stored
    /// rotated 90° clockwise into a transposed (portrait) buffer. This is what
    /// the Touch Bar's t2bdrm/appletbdrm driver expects and is the default.
    #[default]
    Transpose,
}

impl ScanoutOrientation {
    /// The physical (panel) viewport size for a DRM buffer of `width` ×
    /// `height`. Transposing swaps the dimensions, so the UI is created at the
    /// landscape size the user sees.
    pub fn viewport_size(self, width: u16, height: u16) -> (u16, u16) {
        match self {
            ScanoutOrientation::Normal => (width, height),
            ScanoutOrientation::Transpose => (height, width),
        }
    }
}

/// Convert `frame` into XRGB8888 bytes in `dst`.
///
/// `dst` is the DRM buffer's mapped bytes, `width`×`height` pixels arranged at
/// row `pitch` (bytes). Only the `pitch × height` pixel area is written; `dst`
/// may be longer (the kernel rounds the mmap up to a page boundary) and extra
/// bytes are left untouched. The alpha byte of the premultiplied source is
/// dropped (the display is opaque) and the X pad is written as 0.
///
/// # Errors
///
/// - `frame.format` is not [`PixelFormat::Rgba8`];
/// - `frame` dimensions do not match `width`×`height` under `orientation`;
/// - `dst` is too small to hold `pitch × height` bytes.
pub fn convert_frame(
    frame: &Frame,
    dst: &mut [u8],
    width: u16,
    height: u16,
    pitch: u32,
    orientation: ScanoutOrientation,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if frame.format != PixelFormat::Rgba8 {
        return Err(ConvertError::UnsupportedFormat(frame.format).into());
    }

    let (w, h) = (usize::from(width), usize::from(height));
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    let expected =
        |label: &str, want_w: usize, want_h: usize| -> Result<(), Box<dyn Error + Send + Sync>> {
            if fw != want_w || fh != want_h {
                Err(ConvertError::DimensionMismatch {
                    frame: (fw, fh),
                    buffer: (w, h),
                    orientation,
                    expected: (want_w, want_h),
                    label: label.to_string(),
                }
                .into())
            } else {
                Ok(())
            }
        };
    match orientation {
        ScanoutOrientation::Normal => expected("normal buffer", w, h)?,
        ScanoutOrientation::Transpose => expected("transposed buffer", h, w)?,
    }

    let pitch = pitch as usize;
    if dst.len() < pitch * h {
        return Err(ConvertError::DestinationTooSmall {
            dst_bytes: dst.len(),
            needed: pitch * h,
        }
        .into());
    }

    match orientation {
        ScanoutOrientation::Normal => convert_normal(frame, dst, w, h, pitch),
        ScanoutOrientation::Transpose => convert_transpose(frame, dst, w, h, pitch),
    }
    Ok(())
}

/// Copy the premultiplied RGBA8 pixel at `src_off` into XRGB8888 bytes at
/// `dst_off` (`[B, G, R, X]`), dropping the alpha byte.
#[inline]
fn write_pixel(dst: &mut [u8], dst_off: usize, src: &[u8], src_off: usize) {
    dst[dst_off] = src[src_off + 2];
    dst[dst_off + 1] = src[src_off + 1];
    dst[dst_off + 2] = src[src_off];
    dst[dst_off + 3] = 0;
}

/// Identity mapping: `dst(fx, fy) = src(fx, fy)`.
fn convert_normal(frame: &Frame, dst: &mut [u8], w: usize, h: usize, pitch: usize) {
    let stride = frame.stride;
    for fy in 0..h {
        let src_row = fy * stride;
        let dst_row = fy * pitch;
        for fx in 0..w {
            let si = src_row + fx * 4;
            let di = dst_row + fx * 4;
            write_pixel(dst, di, &frame.data, si);
        }
    }
}

/// Transposed mapping: `dst(fx, fy) = src(fy, H − 1 − fx)`.
///
/// `frame.width == h` (`u` runs over buffer rows) and `frame.height == w`
/// (`v` runs over buffer columns, reversed) - verified by [`convert_frame`].
fn convert_transpose(frame: &Frame, dst: &mut [u8], w: usize, h: usize, pitch: usize) {
    let stride = frame.stride;
    let fh = frame.height as usize; // == w
    for fy in 0..h {
        let u = fy;
        let dst_row = fy * pitch;
        for fx in 0..w {
            let v = fh - 1 - fx;
            let si = v * stride + u * 4;
            let di = dst_row + fx * 4;
            write_pixel(dst, di, &frame.data, si);
        }
    }
}

/// Errors produced when a [`Frame`] cannot be converted to a DRM buffer.
#[derive(Debug)]
enum ConvertError {
    /// The frame is not premultiplied RGBA8.
    UnsupportedFormat(PixelFormat),
    /// The frame dimensions do not satisfy the orientation's mapping.
    DimensionMismatch {
        frame: (usize, usize),
        buffer: (usize, usize),
        orientation: ScanoutOrientation,
        expected: (usize, usize),
        label: String,
    },
    /// The byte slice is smaller than the buffer's pixel area.
    DestinationTooSmall { dst_bytes: usize, needed: usize },
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConvertError::UnsupportedFormat(format) => {
                write!(
                    f,
                    "cannot convert {format:?} frame to XRGB8888: only premultiplied RGBA8 is supported"
                )
            }
            ConvertError::DimensionMismatch {
                frame,
                buffer,
                orientation,
                expected,
                label,
            } => write!(
                f,
                "frame {}x{} does not match {label} {}x{} ({orientation:?} expects {}x{j})",
                frame.0,
                frame.1,
                buffer.0,
                buffer.1,
                expected.0,
                j = expected.1,
            ),
            ConvertError::DestinationTooSmall { dst_bytes, needed } => write!(
                f,
                "destination buffer too small: {dst_bytes} bytes, need {needed} (pitch × height)"
            ),
        }
    }
}

impl std::error::Error for ConvertError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read one XRGB8888 pixel at buffer column `x`, row `y`.
    fn px(buf: &[u8], x: usize, y: usize, pitch: usize) -> [u8; 4] {
        let off = y * pitch + x * 4;
        [buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]
    }

    /// Build a `4×3` frame; pixel `(u, v)` is `[B=u*64, G=v*64, R=0, A=255]`
    /// with a unique nonzero prefix so every pixel is distinguishable.
    fn four_by_three_frame() -> Frame {
        let mut frame = Frame::new(4, 3, PixelFormat::Rgba8);
        let bpp = frame.format.bytes_per_pixel();
        for v in 0..3 {
            for u in 0..4 {
                let off = v * frame.stride + u * bpp;
                frame.data[off] = u as u8 * 16 + 1;
                frame.data[off + 1] = v as u8 * 40 + 2;
                frame.data[off + 2] = 0;
                frame.data[off + 3] = 255;
            }
        }
        frame
    }

    #[test]
    fn transpose_rotates_the_frame_90_clockwise_into_the_buffer() {
        let frame = four_by_three_frame();
        let w = frame.height as u16; // 3
        let h = frame.width as u16; // 4
        let pitch = u32::from(w) * (frame.format.bytes_per_pixel() as u32);
        let mut buf = vec![0u8; (pitch as usize) * (h as usize)];

        convert_frame(&frame, &mut buf, w, h, pitch, ScanoutOrientation::Transpose)
            .expect("transpose conversion succeeds");

        // dst(fx, fy) = src(fy, H−1−fx), H=3.
        // Each frame pixel (u, v) stores R = u*16+1, G = v*40+2, B = 0, so the
        // XRGB8888 output is [B, G, R, X] = [0, v*40+2, u*16+1, 0].
        let at = |x: usize, y: usize| px(&buf, x, y, pitch as usize);
        // src(u=3, v=0) → dst(fx=2, fy=3).
        assert_eq!(at(2, 3), [0, 2, 49, 0], "src(3,0) → dst(2,3)");
        // src(u=0, v=2) → dst(fx=0, fy=0).
        assert_eq!(at(0, 0), [0, 82, 1, 0], "src(0,2) → dst(0,0)");
        // src(u=3, v=2) → dst(fx=0, fy=3).
        assert_eq!(at(0, 3), [0, 82, 49, 0], "src(3,2) → dst(0,3)");
        // src(u=0, v=0) → dst(fx=2, fy=0).
        assert_eq!(at(2, 0), [0, 2, 1, 0], "src(0,0) → dst(2,0)");
        // src(u=1, v=1) → dst(fx=1, fy=1).
        assert_eq!(at(1, 1), [0, 42, 17, 0], "src(1,1) → dst(1,1)");
    }

    #[test]
    fn normal_maps_pixels_in_place() {
        let frame = four_by_three_frame();
        let (w, h) = (4_u16, 3_u16);
        let pitch = 20_u32; // 4 bytes of padding per row
        let mut buf = vec![0xAAu8; pitch as usize * 3]; // dirty, sentinel bytes

        convert_frame(&frame, &mut buf, w, h, pitch, ScanoutOrientation::Normal)
            .expect("normal conversion succeeds");

        let at = |x: usize, y: usize| px(&buf, x, y, pitch as usize);
        // src(0,0): R=1, G=2 → dst [B,G,R,X] = [0,2,1,0].
        assert_eq!(at(0, 0), [0, 2, 1, 0], "src(0,0) → dst(0,0)");
        // src(3,2): R=49, G=82 → [0,82,49,0].
        assert_eq!(at(3, 2), [0, 82, 49, 0], "src(3,2) → dst(3,2)");
        // Pitch padding bytes (row 0, after its 16 pixel bytes) are untouched.
        assert_eq!(&buf[16..20], &[0xAA, 0xAA, 0xAA, 0xAA]);
        assert_eq!(buf.len(), 60);
    }

    #[test]
    fn normal_addresses_rows_by_pitch_not_tight_stride() {
        // A 2×2 frame on a buffer with pitch 12 (3 bytes per row gap) must
        // produce rows at offsets 0 and 12, not 0 and 8.
        let mut frame = Frame::new(2, 2, PixelFormat::Rgba8);
        frame.data[0] = 10; // (0,0): R
        frame.data[8] = 20; // (0,1): R (row 1 starts at stride 8)
        let mut buf = [0u8; 24];
        convert_frame(&frame, &mut buf, 2, 2, 12, ScanoutOrientation::Normal).expect("ok");

        assert_eq!(&buf[0..4], &[0, 0, 10, 0], "row 0 at offset 0");
        assert_eq!(&buf[12..16], &[0, 0, 20, 0], "row 1 at offset pitch=12");
        // Gaps (offsets 4..8 and 16..20) remain zero.
        assert_eq!(&buf[4..8], &[0; 4]);
        assert_eq!(&buf[16..20], &[0; 4]);
    }

    #[test]
    fn transpose_drops_alpha_and_orders_b_gr_x() {
        let mut frame = Frame::new(1, 1, PixelFormat::Rgba8);
        frame.data = vec![0x11, 0x22, 0x33, 0x99]; // premultiplied RGBA
        let mut buf = [0u8; 4];
        convert_frame(&frame, &mut buf, 1, 1, 4, ScanoutOrientation::Transpose).expect("ok");
        assert_eq!(buf, [0x33, 0x22, 0x11, 0x00], "B,G,R,X and alpha dropped");
    }

    #[test]
    fn viewport_size_swaps_dimensions_for_transpose() {
        assert_eq!(
            ScanoutOrientation::Transpose.viewport_size(60, 2008),
            (2008, 60)
        );
        assert_eq!(
            ScanoutOrientation::Normal.viewport_size(60, 2008),
            (60, 2008)
        );
    }

    #[test]
    fn rejects_wrong_dimensions_for_orientation() {
        let frame = four_by_three_frame(); // 4 wide × 3 tall
        let mut buf = [0u8; 16 * 3];

        // 4×3 buffer with Transpose wants frame 3×4.
        let err = convert_frame(&frame, &mut buf, 4, 3, 16, ScanoutOrientation::Transpose)
            .expect_err("must fail");
        assert!(
            err.to_string().contains("transposed"),
            "error should name the expected geometry: {err}"
        );

        // 3×4 buffer with Normal wants frame 3×4.
        let err = convert_frame(&frame, &mut buf, 3, 4, 16, ScanoutOrientation::Normal)
            .expect_err("must fail");
        assert!(err.to_string().contains("normal"), "unexpected: {err}");
    }

    #[test]
    fn rejects_undersized_destination() {
        let frame = Frame::new(4, 3, PixelFormat::Rgba8);
        // pitch 16 × height 3 = 48 bytes needed; give it 47.
        let mut buf = [0u8; 47];

        let err = convert_frame(&frame, &mut buf, 4, 3, 16, ScanoutOrientation::Normal)
            .expect_err("dst too small");
        assert!(err.to_string().contains("too small"), "unexpected: {err}");
    }

    #[test]
    fn leaves_bytes_beyond_pitch_times_height_untouched() {
        // Simulate the kernel rounding the mmap up: the mapping holds more bytes
        // than the buffer's pixel area and those trailing bytes are untouched.
        let mut frame = Frame::new(2, 2, PixelFormat::Rgba8);
        frame.data[0] = 7;
        let mut buf = [0xEEu8; 64]; // pitch 16 → pixel area is 32 bytes; 32 spare
        convert_frame(&frame, &mut buf, 2, 2, 16, ScanoutOrientation::Normal).expect("ok");
        assert_eq!(
            &buf[32..],
            &[0xEE; 32],
            "extra bytes beyond pitch×height untouched"
        );
    }
}
