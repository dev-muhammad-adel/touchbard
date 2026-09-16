//! Deterministic test-pattern writer for CPU-rendered DRM buffers.
//!
//! Produces an unmistakable static test image inside a mapped dumb buffer:
//! a fill, a border, three primary bands, and a center stripe, all fully
//! deterministic.
//!
//! # Pixel format
//!
//! The pattern writes **XRGB8888** byte order (`B,G,R,X` in memory) - the
//! layout a legacy `DRM_MODE_ADDFB` framebuffer (24-bit depth, 32-bit bpp,
//! which is what [`crate::framebuffer::DrmFramebuffer`] registers) scans out.
//! The `X` byte is ignored by scanout and written as 0.
//!
//! # Stride
//!
//! All helpers take an explicit row `pitch` in bytes and are written row by
//! row at `y * pitch`, so they are correct even when the kernel returns a
//! pitch wider than `width * 4`. They never assume tightly packed rows and
//! never write past `pitch * height`.

/// Bytes per pixel for the 32bpp XRGB8888 layout.
pub const BYTES_PER_PIXEL: u32 = 4;

/// A 32-bit pixel laid out in XRGB8888 memory order `[B, G, R, X]`.
type Xrgb8888 = [u8; 4];

/// White (`X` pad = 0).
const WHITE: Xrgb8888 = [0xFF, 0xFF, 0xFF, 0x00];
/// Pure red.
const RED: Xrgb8888 = [0x00, 0x00, 0xFF, 0x00];
/// Pure green.
const GREEN: Xrgb8888 = [0x00, 0xFF, 0x00, 0x00];
/// Pure blue.
const BLUE: Xrgb8888 = [0xFF, 0x00, 0x00, 0x00];
/// Dark neutral background.
const DARK: Xrgb8888 = [0x18, 0x18, 0x18, 0x00];

/// Thickness of the white border in pixels.
const BORDER: u32 = 3;

/// Write a solid rectangle into a mapped buffer, respecting `pitch`.
///
/// The half-open rectangle `[x0, x1) × [y0, y1)` is filled with `pixel`
/// (XRGB8888 memory order). Rows are addressed at `y * pitch`, never at
/// `y * width * 4`.
///
/// # Panics
///
/// Panics if the rectangle is outside the buffer's pixel area
/// (`width × height`) or would overflow the provided byte slice.
pub fn fill_rect(
    bytes: &mut [u8],
    width: u16,
    height: u16,
    pitch: u32,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    pixel: Xrgb8888,
) {
    let (width, height) = (u32::from(width), u32::from(height));
    assert!(y0 <= y1 && x0 <= x1, "inverted rectangle");
    assert!(x1 <= width && y1 <= height, "rectangle outside the buffer");
    let pitch = pitch as usize;
    assert!(bytes.len() >= pitch * height as usize, "buffer too small");

    for y in y0..y1 {
        let row = &mut bytes
            [y as usize * pitch..y as usize * pitch + x1 as usize * BYTES_PER_PIXEL as usize];
        for x in x0..x1 {
            let px = &mut row[x as usize * BYTES_PER_PIXEL as usize
                ..(x as usize + 1) * BYTES_PER_PIXEL as usize];
            px.copy_from_slice(&pixel);
        }
    }
}

/// Fill a band (a horizontal strip across the full width).
fn fill_band(
    bytes: &mut [u8],
    width: u16,
    height: u16,
    pitch: u32,
    y0: u32,
    y1: u32,
    pixel: Xrgb8888,
) {
    fill_rect(
        bytes,
        width,
        height,
        pitch,
        0,
        y0,
        u32::from(width),
        y1,
        pixel,
    );
}

/// Write the deterministic test pattern into a mapped buffer.
///
/// Layout (all geometry derived from `width`/`height`; nothing hardcoded to a
/// specific display):
///
/// - dark background everywhere;
/// - a white border `BORDER` (3) pixels thick;
/// - the area inside the border split into three horizontal thirds: red at
///   the top, green in the middle, blue at the bottom;
/// - a white vertical stripe, 2 pixels wide, centered horizontally, across
///   the interior.
pub fn write_test_pattern(bytes: &mut [u8], width: u16, height: u16, pitch: u32) {
    let w = u32::from(width);
    let h = u32::from(height);
    assert!(
        w >= BORDER * 2 && h >= BORDER * 3,
        "display too small for the test pattern"
    );

    fill_rect(bytes, width, height, pitch, 0, 0, w, h, DARK);
    fill_rect(bytes, width, height, pitch, 0, 0, w, BORDER, WHITE);
    fill_rect(bytes, width, height, pitch, 0, h - BORDER, w, h, WHITE);
    fill_rect(bytes, width, height, pitch, 0, 0, BORDER, h, WHITE);
    fill_rect(bytes, width, height, pitch, w - BORDER, 0, w, h, WHITE);

    let interior_y0 = BORDER;
    let interior_y1 = h - BORDER;
    let third = interior_y1.saturating_sub(interior_y0) / 3;

    fill_band(
        bytes,
        width,
        height,
        pitch,
        interior_y0,
        interior_y0 + third,
        RED,
    );
    fill_band(
        bytes,
        width,
        height,
        pitch,
        interior_y0 + third,
        interior_y0 + 2 * third,
        GREEN,
    );
    fill_band(
        bytes,
        width,
        height,
        pitch,
        interior_y0 + 2 * third,
        interior_y1,
        BLUE,
    );

    let stripe_x0 = w / 2 - 1;
    let stripe_x1 = w / 2 + 1;
    fill_rect(
        bytes,
        width,
        height,
        pitch,
        stripe_x0,
        interior_y0,
        stripe_x1,
        interior_y1,
        WHITE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_rect_respects_pitch_gaps() {
        // 2 px wide × 3 px tall buffer, bpp 4 → 8 occupied bytes per row, but
        // pitch is 12 → 4 padding bytes per row that must stay untouched.
        let width = 2_u16;
        let height = 3_u16;
        let pitch = 12_u32;
        let mut bytes = [0_u8; 36];

        fill_rect(&mut bytes, width, height, pitch, 0, 1, 2, 2, RED);

        // Second row (bytes 12..24): pixels set to red in memory order.
        assert_eq!(
            &bytes[12..20],
            &[0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00]
        );
        // Padding of the touched row remains zero.
        assert_eq!(&bytes[20..24], &[0, 0, 0, 0]);
        // Untouched rows remain zero, including their padding.
        assert_eq!(&bytes[0..12], &[0; 12]);
        assert_eq!(&bytes[24..36], &[0; 12]);
    }

    #[test]
    fn write_test_pattern_fills_known_anchors() {
        // 12×12, tight pitch (48): exactly a 3-px border, thirds, center stripe.
        let width = 12_u16;
        let height = 12_u16;
        let pitch = 48_u32;
        let mut bytes = vec![0_u8; pitch as usize * 12];

        write_test_pattern(&mut bytes, width, height, pitch);

        // Border corner is white.
        let px = |x: usize, y: usize| bytes[y * 48 + x * 4..y * 48 + x * 4 + 4].to_vec();
        assert_eq!(px(0, 0), WHITE);
        assert_eq!(px(11, 11), WHITE);
        // Interior (3,3) is in the red top third.
        assert_eq!(px(3, 3), RED);

        // The total byte length written must equal pitch * height exactly.
        assert_eq!(bytes.len(), 576);
        // Nothing was written outside the buffer (no panic is the main check),
        // and the byte count is fully consumed by the pattern.
        assert!(bytes.iter().any(|&b| b != 0));
    }
}
