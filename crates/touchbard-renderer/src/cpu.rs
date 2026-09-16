//! CPU (Vello) rasterization pipeline.
//!
//! The renderer crate owns the concrete rasterizer (`VelloCpuImageRenderer`) and
//! the framebuffer it writes into. Painting is delegated back to the caller via
//! a draw closure so this crate stays free of any Dioxus/Blitz document types.

use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;

use crate::{Frame, PixelFormat};

/// The scene painter the CPU rasterizer understands (`anyrender::PaintScene`).
pub type ScenePainter<'a> = <VelloCpuImageRenderer as ImageRenderer>::ScenePainter<'a>;

/// A CPU framebuffer renderer.
///
/// Rasterizes an [`anyrender`](https://docs.rs/anyrender) scene into a
/// premultiplied RGBA8 [`Frame`] synchronously.
pub struct CpuRenderer {
    renderer: VelloCpuImageRenderer,
    format: PixelFormat,
}

impl CpuRenderer {
    /// Create a renderer sized for a `width`×`height` framebuffer.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            renderer: VelloCpuImageRenderer::new(width, height),
            format: PixelFormat::Rgba8,
        }
    }

    /// Resize the backing render target.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.renderer.resize(width, height);
    }

    /// The pixel format produced by this renderer.
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Rasterize a scene into a fresh RGBA `width`×`height` [`Frame`].
    ///
    /// `paint` receives the [`anyrender::PaintScene`] and pushes drawing
    /// commands into it (e.g. `blitz_paint::paint_scene`).
    pub fn render(
        &mut self,
        paint: impl FnOnce(&mut ScenePainter<'_>),
        width: u32,
        height: u32,
    ) -> Frame {
        let mut frame = Frame::new(width, height, self.format);
        let paint_start = std::time::Instant::now();
        self.renderer.render(paint, &mut frame.data);
        if std::env::var_os("TOUCHBARD_RASTER_TRACE").is_some() {
            let mut weighted_x = 0.0_f64;
            let mut weight_total = 0.0_f64;
            let mut matching = 0_u64;
            for y in 0..frame.height {
                for x in 0..frame.width {
                    let offset = y as usize * frame.stride + x as usize * 4;
                    let r = frame.data[offset] as f64;
                    let g = frame.data[offset + 1] as f64;
                    let b = frame.data[offset + 2] as f64;
                    let weight = (b - r).max(0.0) + (g - r).max(0.0) * 0.25;
                    if weight > 20.0 {
                        weighted_x += x as f64 * weight;
                        weight_total += weight;
                        matching += 1;
                    }
                }
            }
            let centroid_x = if weight_total > 0.0 {
                weighted_x / weight_total
            } else {
                f64::NAN
            };
            let wall_us = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros())
                .unwrap_or_default();
            eprintln!(
                "RASTER_TRACE wall_us={} centroid_x={:.6} matching_pixels={} weight={:.3}",
                wall_us, centroid_x, matching, weight_total
            );
        }
        crate::diag::record(crate::diag::Ev::Raster {
            render_us: paint_start.elapsed().as_micros() as u64,
        });
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_renderer_produces_frame() {
        let mut renderer = CpuRenderer::new(64, 16);
        let frame = renderer.render(|_scene| {}, 64, 16);
        assert_eq!(frame.width, 64);
        assert_eq!(frame.height, 16);
        assert_eq!(frame.data.len(), 64 * 16 * 4);
        assert_eq!(frame.format, PixelFormat::Rgba8);
    }

    #[test]
    fn test_renderer_reuse_and_resize() {
        let mut renderer = CpuRenderer::new(32, 8);
        let frame1 = renderer.render(|_scene| {}, 32, 8);
        renderer.resize(128, 32);
        let frame2 = renderer.render(|_scene| {}, 128, 32);
        assert_eq!(frame1.data.len(), 32 * 8 * 4);
        assert_eq!(frame2.data.len(), 128 * 32 * 4);
        assert_eq!(frame2.width, 128);
    }
}
