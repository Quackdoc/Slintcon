//! CPU rasterizer for the terminal grid.
//!
//! Rows are shaped with cosmic-text (monospace width snapped to the cell grid)
//! and rasterized through its swash cache into an RGBA8 buffer handed to Slint
//! as one `Image`. Overlays are blitted from rio-vt's store (kitty) or our
//! cache (sixel/iTerm2).

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::SwashCache;
use rio_vt::ansi::graphics::UpdateQueues;
use rio_vt::config::colors::term::{List, TermColors};
use rio_vt::event::sync::FairMutex;

use crate::app::Term;
use crate::canvas::Canvas;
use crate::images::{ImagePixelsCache, ImageView, OverlayQuad, cull_hidden, is_atlas_key};
use crate::term_style::{
    self, ColorScheme, FontEnv, PRect, RowPaint, RowSource, RowView, ZoomLevels,
};

/// True if every pixel of `img` is opaque, scanned once per image.
fn cached_opaque(cache: &mut HashMap<u64, (usize, usize, bool)>, key: u64, img: ImageView) -> bool {
    let tag = (img.data.as_ptr() as usize, img.data.len());
    if let Some(&(ptr, len, opaque)) = cache.get(&key)
        && (ptr, len) == tag
    {
        return opaque;
    }
    let opaque = img.is_opaque();
    cache.insert(key, (tag.0, tag.1, opaque));
    opaque
}

/// Terminal renderer drawing into an in-memory RGBA8 canvas.
pub struct CpuRenderer {
    /// Font system and cell metrics.
    env: FontEnv,
    /// Glyph rasterizer.
    swash: SwashCache,
    /// The default color palette.
    palette: List,
    /// The framebuffer handed to Slint.
    canvas: Canvas,
    /// Sixel/iTerm2 pixels (rio-vt drops them after handing them over).
    images: ImagePixelsCache,
    /// Source of kitty pixels: rio-vt keeps them, so we read them in place
    /// instead of holding a second copy.
    kitty_source: Option<Arc<FairMutex<Term>>>,
    /// Image quads in stacking order.
    overlays: Vec<OverlayQuad>,
    /// Whether an image has no transparent pixel, keyed by image and tagged
    /// with its pixel buffer's address and length so a replaced image is
    /// scanned again.
    opaque: HashMap<u64, (usize, usize, bool)>,
    /// Scratch for the row being drawn.
    paint: RowPaint,
}

impl CpuRenderer {
    /// A renderer at `font_px`.
    pub fn new(font_px: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let env = FontEnv::new(font_px);
        let palette = term_style::default_palette();
        Ok(Self {
            env,
            swash: SwashCache::new(),
            palette,
            canvas: Canvas::new(),
            images: ImagePixelsCache::default(),
            kitty_source: None,
            overlays: Vec::new(),
            opaque: HashMap::new(),
            paint: RowPaint::default(),
        })
    }

    /// Cell width in pixels.
    pub fn cell_width(&self) -> u32 {
        self.env.metrics.cell_width
    }

    /// Cell height in pixels.
    pub fn cell_height(&self) -> u32 {
        self.env.metrics.cell_height
    }

    /// The zoom levels for the font.
    pub fn zoom(&self) -> &ZoomLevels {
        self.env.zoom()
    }

    /// Resize the canvas and remeasure the font; every row repaints.
    pub fn resize(&mut self, width: u32, height: u32, font_px: u32) {
        self.env.remeasure(font_px);
        let bg = term_style::default_background(&self.palette);
        self.canvas.resize(width, height, bg);
        // Cell metrics may have changed; all row buffers are rebuilt lazily.
    }

    /// Read kitty pixels from `source` in place instead of keeping a copy.
    pub fn set_kitty_source(&mut self, source: Arc<FairMutex<Term>>) {
        self.kitty_source = Some(source);
    }

    /// Keep sixel and iTerm2 pixels (kitty ones are read from `kitty_source`).
    pub fn apply_images(&mut self, queues: UpdateQueues) {
        self.images.apply(queues, false);
    }

    /// Keep the quads in stacking order, minus those hidden by an opaque one.
    pub fn set_overlays(&mut self, mut overlays: Vec<OverlayQuad>) {
        let mut guard = None;
        cull_hidden(&mut overlays, |key| {
            if is_atlas_key(key) {
                self.images
                    .get(key)
                    .is_some_and(|img| cached_opaque(&mut self.opaque, key, img.view()))
            } else {
                self.kitty_source.as_ref().is_some_and(|source| {
                    let term = guard.get_or_insert_with(|| source.lock());
                    term.graphics
                        .get_kitty_image(key as u32)
                        .and_then(|stored| ImageView::from_graphic(&stored.data))
                        .is_some_and(|img| cached_opaque(&mut self.opaque, key, img))
                })
            }
        });
        self.opaque
            .retain(|key, _| overlays.iter().any(|q| q.image_key == *key));
        self.overlays = overlays;
    }

    /// Housekeeping after a painted frame.
    pub fn end_frame(&mut self) {
        self.env.end_frame();
    }

    /// Convert the raw buffer into the image Slint renders.
    pub fn image(&self) -> Option<slint::Image> {
        Some(slint::Image::from_rgba8(
            slint::SharedPixelBuffer::clone_from_slice(
                &self.canvas.pixels,
                self.canvas.width,
                self.canvas.height,
            ),
        ))
    }

    /// Draw one row. `line` is viewport-relative and `styles` is parallel to
    /// the row's cells.
    pub fn draw_row(
        &mut self,
        source: RowSource,
        line: usize,
        view: RowView,
        term_colors: &TermColors,
    ) {
        let Some(cell) = self.env.cell_size() else {
            return;
        };
        let row_y = (line as u64 * cell.height as u64) as u32;
        if row_y >= self.canvas.height {
            return;
        }
        let row_y_f = row_y as f32;

        // Take the scratch out of `self` so painting can borrow `self`.
        let mut paint = std::mem::take(&mut self.paint);
        let colors = ColorScheme {
            term: term_colors,
            palette: &self.palette,
        };
        let width = self.canvas.width;
        term_style::paint_row(source, view, colors, width, cell, row_y_f, &mut paint);

        // Row background and per-run backgrounds.
        let row_bg = PRect {
            x: 0.0,
            y: row_y_f,
            width: width as f32,
            height: cell.height,
            color: term_style::default_background(&self.palette),
        };
        for rect in std::iter::once(row_bg).chain(paint.backgrounds.iter().copied()) {
            self.canvas.fill_rect(rect);
        }

        // Overlays below the text (sixel/iTerm2 and negative-z kitty).
        self.blit_overlays_for_row(row_y_f, cell.height, false);

        // Shape and blit the row text.
        self.draw_text(line, &paint.runs);

        // Decorations and non-block cursor.
        for &rect in paint.decorations.iter().chain(&paint.cursor_bars) {
            self.canvas.fill_rect(rect);
        }
        self.paint = paint;

        // Overlays above the text (kitty z >= 0 and placeholder runs).
        self.blit_overlays_for_row(row_y_f, cell.height, true);
    }

    /// Shape the row's text segments and blit their glyphs.
    fn draw_text(&mut self, line: usize, runs: &[term_style::CellRun]) {
        let mut buffer = self.env.make_buffer();
        let row_y = (line as u64 * self.env.metrics.cell_height as u64) as u32;
        let cell_width = self.env.metrics.cell_width;
        for segment in term_style::text_segments(runs) {
            self.env.set_row_text(&mut buffer, segment);
            buffer.shape_until_scroll(&mut self.env.font_system, false);
            let origin_x = (segment[0].col as u32 * cell_width) as i32;
            self.draw_segment(&buffer, row_y, origin_x);
        }
    }

    /// Blit the glyphs of one shaped segment starting at `origin_x`.
    fn draw_segment(&mut self, buffer: &cosmic_text::Buffer, row_y: u32, origin_x: i32) {
        let glyphs = buffer.layout_runs().flat_map(|run| {
            let baseline = run.line_y.round() as i32;
            run.glyphs.iter().map(move |glyph| (baseline, glyph))
        });
        for (baseline, glyph) in glyphs {
            // Whole-pixel origin: no subpixel bin, so no extra blur.
            let physical = term_style::pixel_glyph(glyph);
            let Some(image) = self
                .swash
                .get_image(&mut self.env.font_system, physical.cache_key)
            else {
                continue;
            };
            let (left, top) = (image.placement.left, image.placement.top);
            let (width, height) = (image.placement.width, image.placement.height);
            if width == 0 || height == 0 {
                continue;
            }
            let at = (
                origin_x + physical.x + left,
                row_y as i32 + baseline + physical.y - top,
            );
            // Coverage masks are one byte per pixel; color glyphs are
            // straight RGBA8 bitmaps.
            if image.data.len() == (width as usize) * (height as usize) {
                let color = glyph.color_opt.map_or([255; 4], |color| color.as_rgba());
                self.canvas
                    .blit_mask(at, (width, height), &image.data, color);
            } else {
                self.canvas.blit_rgba(at, (width, height), &image.data);
            }
        }
    }

    /// Blit the overlays crossing the row, below or above the text.
    fn blit_overlays_for_row(&mut self, row_y: f32, row_height: f32, over_text: bool) {
        let top = row_y.max(0.0);
        let bottom = (row_y + row_height).min(self.canvas.height as f32);
        if bottom <= top {
            return;
        }
        // Locked lazily and at most once per pass, and never while the
        // terminal lock is held elsewhere (painting runs outside it).
        let mut guard = None;
        for quad in &self.overlays {
            if quad.over_text != over_text || quad.y >= bottom || quad.y + quad.height <= top {
                continue;
            }
            let img = if is_atlas_key(quad.image_key) {
                self.images.get(quad.image_key).map(|img| img.view())
            } else {
                let Some(source) = self.kitty_source.as_ref() else {
                    continue;
                };
                guard
                    .get_or_insert_with(|| source.lock())
                    .graphics
                    .get_kitty_image(quad.image_key as u32)
                    .and_then(|stored| ImageView::from_graphic(&stored.data))
            };
            let Some(img) = img else {
                continue;
            };
            self.canvas.blit_image(quad, img, (top, bottom));
        }
    }
}
