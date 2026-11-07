//! The RGBA8 framebuffer the CPU renderer draws into, and its blits.

use crate::images::{ImageView, OverlayQuad};
use crate::term_style::PRect;

/// An RGBA8 framebuffer.
pub struct Canvas {
    /// Row-major RGBA8 pixels.
    pub pixels: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Canvas {
    /// An empty canvas; call [`Canvas::resize`] before drawing.
    pub fn new() -> Self {
        Self {
            pixels: Vec::new(),
            width: 1,
            height: 1,
        }
    }

    /// Resize to `width` x `height` (at least 1 x 1) and clear to `bg`.
    pub fn resize(&mut self, width: u32, height: u32, bg: [u8; 4]) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.pixels
            .resize((self.width as usize) * (self.height as usize) * 4, 0);
        for pixel in self.pixels.as_chunks_mut::<4>().0 {
            *pixel = bg;
        }
    }

    /// Fill `rect` with its color, clipped to the canvas.
    pub fn fill_rect(&mut self, rect: PRect) {
        let PRect {
            x,
            y,
            width,
            height,
            color,
        } = rect;
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let x0 = (x.round() as i32).max(0) as u32;
        let y0 = (y.round() as i32).max(0) as u32;
        let x1 = ((x + width).round() as i32).max(0) as u32;
        let y1 = ((y + height).round() as i32).max(0) as u32;
        let x1 = x1.min(self.width);
        let y1 = y1.min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let span = (x1 - x0) as usize;
        for py in y0..y1 {
            let start = ((py as usize) * (self.width as usize) + x0 as usize) * 4;
            let row = &mut self.pixels[start..start + span * 4];
            for pixel in row.as_chunks_mut::<4>().0 {
                *pixel = color;
            }
        }
    }

    /// Alpha-blend a coverage mask of `size` with a solid color at `at`.
    pub fn blit_mask(&mut self, at: (i32, i32), size: (u32, u32), data: &[u8], color: [u8; 4]) {
        let (width, height) = (self.width as i32, self.height as i32);
        let (w, h) = size;
        for gy in 0..h as i32 {
            let py = at.1 + gy;
            if py < 0 || py >= height {
                continue;
            }
            for gx in 0..w as i32 {
                let px = at.0 + gx;
                if px < 0 || px >= width {
                    continue;
                }
                let coverage = data[(gy as usize) * (w as usize) + gx as usize] as u32;
                if coverage == 0 {
                    continue;
                }
                let di = ((py as usize) * (width as usize) + px as usize) * 4;
                let inv = 255 - coverage;
                for (dst, &src) in self.pixels[di..di + 3].iter_mut().zip(&color) {
                    *dst = ((src as u32 * coverage + *dst as u32 * inv) / 255) as u8;
                }
            }
        }
    }

    /// Straight-alpha source-over blend of an RGBA bitmap of `size` at `at`
    /// (color glyphs).
    pub fn blit_rgba(&mut self, at: (i32, i32), size: (u32, u32), data: &[u8]) {
        let (width, height) = (self.width as i32, self.height as i32);
        let (w, h) = size;
        for gy in 0..h as i32 {
            let py = at.1 + gy;
            if py < 0 || py >= height {
                continue;
            }
            for gx in 0..w as i32 {
                let px = at.0 + gx;
                if px < 0 || px >= width {
                    continue;
                }
                let si = ((gy as usize) * (w as usize) + gx as usize) * 4;
                let alpha = data[si + 3] as u32;
                if alpha == 0 {
                    continue;
                }
                let di = ((py as usize) * (width as usize) + px as usize) * 4;
                if alpha == 255 {
                    self.pixels[di..di + 4].copy_from_slice(&data[si..si + 4]);
                } else {
                    let inv = 255 - alpha;
                    for channel in 0..3 {
                        self.pixels[di + channel] = ((data[si + channel] as u32 * alpha
                            + self.pixels[di + channel] as u32 * inv)
                            / 255) as u8;
                    }
                    self.pixels[di + 3] = 255;
                }
            }
        }
    }

    /// Blit the part of `quad` (showing `img`) that lies between the rows
    /// `clip.0` and `clip.1`, with nearest-neighbour sampling and
    /// straight-alpha source-over blending.
    pub fn blit_image(&mut self, quad: &OverlayQuad, img: ImageView, clip: (f32, f32)) {
        let OverlayQuad {
            x,
            y,
            width,
            height,
            src,
            ..
        } = *quad;
        if width <= 0.0 || height <= 0.0 || img.width == 0 || img.height == 0 {
            return;
        }
        let (clip_top, clip_bottom) = clip;
        let x0 = x.floor().max(0.0) as i32;
        let y0 = y.floor().max(clip_top).max(0.0) as i32;
        let x1 = (x + width).ceil().min(self.width as f32) as i32;
        let y1 = (y + height).ceil().min(clip_bottom).min(self.height as f32) as i32;
        if x1 <= x0 || y1 <= y0 {
            return;
        }

        let iw = img.width as i32;
        let ih = img.height as i32;
        let [u0, v0, u1, v1] = src;

        for py in y0..y1 {
            let fy = ((py as f32 + 0.5 - y) / height).clamp(0.0, 1.0);
            let v = (v0 + (v1 - v0) * fy).clamp(0.0, 1.0);
            let sy = ((v * ih as f32) as i32).clamp(0, ih - 1);
            for px in x0..x1 {
                let fx = ((px as f32 + 0.5 - x) / width).clamp(0.0, 1.0);
                let u = (u0 + (u1 - u0) * fx).clamp(0.0, 1.0);
                let sx = ((u * iw as f32) as i32).clamp(0, iw - 1);

                let ch = img.channels as usize;
                let si = ((sy as usize) * (iw as usize) + sx as usize) * ch;
                let alpha = if ch == 4 {
                    img.data[si + 3] as u32
                } else {
                    255
                };
                if alpha == 0 {
                    continue;
                }
                let di = ((py as usize) * (self.width as usize) + px as usize) * 4;
                if alpha == 255 {
                    self.pixels[di..di + 3].copy_from_slice(&img.data[si..si + 3]);
                    self.pixels[di + 3] = 255;
                } else {
                    let inv = 255 - alpha;
                    for channel in 0..3 {
                        self.pixels[di + channel] = ((img.data[si + channel] as u32 * alpha
                            + self.pixels[di + channel] as u32 * inv)
                            / 255) as u8;
                    }
                    self.pixels[di + 3] =
                        ((alpha * 255 + self.pixels[di + 3] as u32 * inv) / 255) as u8;
                }
            }
        }
    }
}
