//! Font size constants, face metrics and the zoom levels derived from them.

/// Base font size in logical pixels, used for the windowed default
/// (multiplied by the window scale factor and rounded to whole pixels).
pub const FONT_SIZE: f32 = 14.0;

/// Smallest and largest absolute font sizes, in physical pixels.
pub const MIN_FONT_PX: u32 = 6;
pub const MAX_FONT_PX: u32 = 160;
/// Smallest font size picked automatically for a window.
const MIN_DEFAULT_FONT_PX: u32 = 10;

/// Default absolute font size (physical pixels), always a zoom level.
/// With `fit_columns`: see [`fit_columns_font_px`]; `scale` is ignored.
/// Otherwise the base size times `scale`, snapped down to a level, at least
/// `MIN_DEFAULT_FONT_PX` when a level that large exists.
pub fn default_font_px(zoom: &ZoomLevels, fit_columns: Option<u32>, width: u32, scale: f32) -> u32 {
    if let Some(columns) = fit_columns {
        return fit_columns_font_px(zoom, columns, width);
    }
    let target = ((FONT_SIZE * scale).round() as u32).clamp(MIN_DEFAULT_FONT_PX, MAX_FONT_PX);
    let px = zoom.snap_down(target);
    if px < MIN_DEFAULT_FONT_PX {
        zoom.snap_up(MIN_DEFAULT_FONT_PX)
    } else {
        px
    }
}

/// The largest zoom level whose grid still fits `columns` columns across
/// `width` (the smallest level if none does). The row count follows from the
/// height.
fn fit_columns_font_px(zoom: &ZoomLevels, columns: u32, width: u32) -> u32 {
    let face = zoom.face();
    let levels = &zoom.levels;
    // Levels strictly grow the cell width, so the column count falls as the
    // level rises.
    levels
        .iter()
        .rev()
        .copied()
        .find(|px| width / face.cell_width(*px) >= columns)
        .unwrap_or(levels[0])
}

/// Size-independent metrics of the primary face, in em. Cell metrics for
/// any font size are derived from these, so measuring once is enough.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceMetrics {
    /// Shaped advance of a space.
    pub advance_em: f32,
    /// Ascent + descent + line gap.
    pub line_height_em: f32,
}

impl FaceMetrics {
    /// Used when the face cannot be measured.
    pub(super) const FALLBACK: Self = Self {
        advance_em: 0.6,
        line_height_em: 1.2,
    };

    /// The face's natural (unrounded) advance at `font_px`.
    pub fn advance(&self, font_px: u32) -> f32 {
        self.advance_em * font_px as f32
    }

    /// Integer grid step: the natural advance rounded to whole pixels.
    pub fn cell_width(&self, font_px: u32) -> u32 {
        (self.advance(font_px).round() as u32).max(1)
    }

    /// Integer row height: the line height rounded up to whole pixels (the
    /// epsilon keeps exact products like 1.25 × 16 from rounding up).
    pub fn cell_height(&self, font_px: u32) -> u32 {
        ((self.line_height_em * font_px as f32 - 1e-3).ceil() as u32).max(1)
    }

    pub fn cell(&self, font_px: u32) -> (u32, u32) {
        (self.cell_width(font_px), self.cell_height(font_px))
    }

    /// Distance of the natural advance from the integer cell width. Outlines
    /// are only hinted vertically, so a fractional advance (7.5px in an 8px
    /// cell) smears one stem across two pixel columns; whole-pixel advances
    /// render sharp.
    pub fn advance_error(&self, font_px: u32) -> f32 {
        (self.advance(font_px) - self.cell_width(font_px) as f32).abs()
    }
}

/// The font sizes Ctrl+/Ctrl- step through, ascending. Sizes are grouped by the
/// cell width they produce and each group contributes the one whose advance is
/// closest to a whole pixel (ties: the smaller), so every step changes the grid
/// and every level is sharp. For Iosevka (0.5 em) that is every even size.
#[derive(Clone, Debug)]
pub struct ZoomLevels {
    face: FaceMetrics,
    levels: Vec<u32>,
}

impl ZoomLevels {
    /// The zoom levels of a face.
    pub fn new(face: FaceMetrics) -> Self {
        let sizes: Vec<u32> = (MIN_FONT_PX..=MAX_FONT_PX).collect();
        let levels = sizes
            .chunk_by(|a, b| face.cell_width(*a) == face.cell_width(*b))
            // Per cell width, the size whose advance is closest to a whole
            // pixel; a later size must be clearly better to replace one.
            .filter_map(|group| {
                group.iter().copied().reduce(|best, px| {
                    if face.advance_error(px) < face.advance_error(best) - 1e-4 {
                        px
                    } else {
                        best
                    }
                })
            })
            .collect();
        Self { face, levels }
    }

    pub fn face(&self) -> &FaceMetrics {
        &self.face
    }

    /// The adjacent level from `font_px` (which need not be a level), or
    /// `font_px` itself at either end.
    pub fn step(&self, font_px: u32, zoom_in: bool) -> u32 {
        let current = self.face.cell(font_px);
        let mut candidates = self.levels.iter().copied().filter(|&px| {
            let ahead = if zoom_in { px > font_px } else { px < font_px };
            ahead && self.face.cell(px) != current
        });
        // The nearest one: the first above, or the last below.
        let next = if zoom_in {
            candidates.next()
        } else {
            candidates.next_back()
        };
        next.unwrap_or(font_px)
    }

    /// Largest level not above `font_px`, or the smallest level.
    pub fn snap_down(&self, font_px: u32) -> u32 {
        self.levels
            .iter()
            .rev()
            .copied()
            .find(|px| *px <= font_px)
            .unwrap_or(self.levels[0])
    }

    /// Smallest level not below `font_px`, or the largest level.
    pub fn snap_up(&self, font_px: u32) -> u32 {
        self.levels
            .iter()
            .copied()
            .find(|px| *px >= font_px)
            .unwrap_or(*self.levels.last().unwrap())
    }
}
