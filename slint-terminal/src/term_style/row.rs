//! Cell runs and the geometry of one painted terminal row.

use rio_vt::ansi::CursorShape;
use rio_vt::ansi::graphics::KITTY_PLACEHOLDER;
use rio_vt::config::colors::{AnsiColor, ColorRgb, NamedColor};
use rio_vt::crosswords::grid::row::Row;
use rio_vt::crosswords::square::{ContentTag, Extras, Square, Wide};
use rio_vt::crosswords::style::{Style, StyleFlags};
use rustc_hash::FxHashMap;

use super::color::{ColorScheme, brighten};
use super::sprite::{self, SpriteCell};

/// One snapshot row with the styles and extras its cells refer to.
#[derive(Clone, Copy)]
pub struct RowSource<'a> {
    /// The row's cells.
    pub row: &'a Row<Square>,
    /// The style of each cell, parallel to `row`.
    pub styles: &'a [Style],
    /// Extra cell data (e.g. zero-width characters) by id.
    pub extras: &'a FxHashMap<u16, Extras>,
}

/// What a row looks like beyond its cells; a row is repainted when its view
/// differs from the one it was last painted with.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RowView {
    /// The cursor, if it is on this row.
    pub cursor: Option<CursorPaint>,
    /// The selected columns, if any.
    pub selection: Option<Selected>,
}

/// The size of one grid cell in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct CellSize {
    /// Cell width.
    pub width: f32,
    /// Cell height.
    pub height: f32,
}

/// A physical-pixel rectangle with a non managed color.
#[derive(Clone, Copy, Debug)]
pub struct PRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: [u8; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorPaint {
    pub col: usize,
    pub shape: CursorShape,
}

/// The selected columns of a row, both ends included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selected {
    pub first: usize,
    pub last: usize,
}

impl Selected {
    /// Whether any of the `span` cells starting at `col` is selected.
    fn covers(&self, col: usize, span: usize) -> bool {
        col <= self.last && col + span > self.first
    }
}

/// One horizontal run of cells that share the same paint attributes. The runs
/// of a row cover every column in order (wide-character spacer cells extend the
/// previous run's `cells`).
#[derive(Clone, Debug)]
pub struct CellRun {
    pub col: usize,
    /// Number of grid cells covered (2 for a wide character, else 1).
    pub cells: usize,
    pub text: String,
    pub fg: [u8; 4],
    pub bg: [u8; 4],
    /// True when the background equals the terminal's default background and
    /// therefore does not need to be painted.
    pub bg_default: bool,
    pub flags: StyleFlags,
    pub underline_color: [u8; 4],
    /// True when the block cursor covers this run and the colors were swapped.
    pub cursor: bool,
    /// A single private-use (icon) cell, shaped on its own because icon fonts
    /// often advance two cells for one and would shift the glyphs after it.
    pub isolate: bool,
}

/// True for private-use code points, where fallback fonts commonly report
/// advances that disagree with the terminal's one-cell width.
fn is_private_use(ch: char) -> bool {
    matches!(ch as u32, 0xE000..=0xF8FF | 0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD)
}

/// Split a row's runs into independently shaped segments, placed at
/// `segments[i][0].col * cell_width`. Isolated runs are their own segment.
pub fn text_segments(runs: &[CellRun]) -> Vec<&[CellRun]> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (i, run) in runs.iter().enumerate() {
        if run.isolate {
            if start < i {
                segments.push(&runs[start..i]);
            }
            segments.push(&runs[i..=i]);
            start = i + 1;
        }
    }
    if start < runs.len() {
        segments.push(&runs[start..]);
    }
    segments
}

fn cell_style(square: Square, styles: &[Style], col: usize) -> Style {
    match square.content_tag() {
        ContentTag::Codepoint => styles.get(col).copied().unwrap_or_default(),
        ContentTag::BgPalette => Style {
            bg: AnsiColor::Indexed(square.bg_palette_index()),
            ..Style::default()
        },
        ContentTag::BgRgb => {
            let (r, g, b) = square.bg_rgb();
            Style {
                bg: AnsiColor::Spec(ColorRgb { r, g, b }),
                ..Style::default()
            }
        }
    }
}

/// Fold a style into foreground/background colors; the third result is whether
/// the background is the default and can be skipped.
fn style_colors(style: Style, tag: ContentTag, colors: ColorScheme) -> ([u8; 4], [u8; 4], bool) {
    let flags = style.flags;
    let fg_color = if flags.contains(StyleFlags::BOLD) {
        brighten(style.fg)
    } else {
        style.fg
    };
    let mut fg = colors.resolve(fg_color);
    let mut bg = colors.resolve(style.bg);

    if flags.contains(StyleFlags::DIM) {
        for channel in &mut fg[..3] {
            *channel = ((*channel as f32) * 0.66).round() as u8;
        }
    }

    let inverse = flags.contains(StyleFlags::INVERSE);
    let bg_default = !inverse
        && matches!(tag, ContentTag::Codepoint)
        && style.bg == AnsiColor::Named(NamedColor::Background);

    if inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if flags.contains(StyleFlags::HIDDEN) {
        fg = bg;
    }

    (fg, bg, bg_default)
}

/// Decompose one snapshot row into styled runs and sprite cells (replacing
/// those in `out`).
fn build_row_runs(
    source: RowSource,
    view: RowView,
    colors: ColorScheme,
    max_cols: usize,
    out: &mut RowPaint,
) {
    let RowSource {
        row,
        styles,
        extras,
    } = source;
    let RowView { cursor, selection } = view;
    let RowPaint { runs, sprites, .. } = out;
    let cols = row.inner.len().min(max_cols);
    runs.clear();
    sprites.clear();

    // The colors of a cell and whether the cursor is on it. The block cursor
    // and the selection both show the cell inverted.
    let cell_colors = |style: Style, tag: ContentTag, col: usize, span: usize| {
        let (fg, bg, bg_default) = style_colors(style, tag, colors);
        let cursor_here = cursor.filter(|c| c.col == col || (span == 2 && c.col == col + 1));
        let inverted = cursor_here.is_some_and(|c| c.shape == CursorShape::Block)
            || selection.is_some_and(|selected| selected.covers(col, span));
        let (fg, bg, bg_default) = if inverted {
            (bg, fg, false)
        } else {
            (fg, bg, bg_default)
        };
        (fg, bg, bg_default, cursor_here.is_some())
    };

    // The text of a cell, whether it must be shaped alone, and the character
    // to draw as a sprite instead (the cell is blank for the font then).
    let cell_text = |square: Square, tag: ContentTag| {
        let ch = square.c();
        match tag {
            // Placeholder cells are painted by the graphics layer; a blank
            // preserves the grid advance.
            ContentTag::Codepoint if matches!(ch, KITTY_PLACEHOLDER | '\0' | '\u{a0}') => {
                (" ".to_string(), false, None)
            }
            ContentTag::Codepoint if square.extras_id().is_none() && sprite::is_sprite(ch) => {
                (" ".to_string(), false, Some(ch))
            }
            ContentTag::Codepoint => {
                let zerowidth = square
                    .extras_id()
                    .and_then(|id| extras.get(&id))
                    .into_iter()
                    .flat_map(|extras| extras.zerowidth.iter().copied());
                let text = std::iter::once(ch).chain(zerowidth).collect();
                (text, is_private_use(ch), None)
            }
            _ => (" ".to_string(), false, None),
        }
    };

    // Extend the last run if `run` looks the same, else start a new one.
    let mut push_or_merge = |run: CellRun| match runs.last_mut() {
        Some(last)
            if !(last.cursor || last.isolate || run.cursor || run.isolate)
                && last.col + last.cells == run.col
                && (
                    last.fg,
                    last.bg,
                    last.bg_default,
                    last.flags,
                    last.underline_color,
                ) == (
                    run.fg,
                    run.bg,
                    run.bg_default,
                    run.flags,
                    run.underline_color,
                ) =>
        {
            last.cells += run.cells;
            last.text.push_str(&run.text);
        }
        _ => runs.push(run),
    };

    let mut col = 0;
    while col < cols {
        let square = row.inner[col];
        let span = match square.wide() {
            Wide::Spacer => {
                col += 1;
                continue;
            }
            Wide::Wide => 2,
            _ => 1,
        };
        let tag = square.content_tag();
        let style = cell_style(square, styles, col);

        let (fg, bg, bg_default, cursor_here) = cell_colors(style, tag, col, span);
        let (text, isolate, sprite) = cell_text(square, tag);
        sprites.extend(sprite.map(|ch| SpriteCell { col, ch, fg, bg }));
        let underline_color = style.underline_color.map_or(fg, |c| colors.resolve(c));

        push_or_merge(CellRun {
            col,
            cells: span,
            text,
            fg,
            bg,
            bg_default,
            flags: style.flags,
            underline_color,
            cursor: cursor_here,
            isolate,
        });
        col += span;
    }
}

/// Appends the underline and strikeout rectangles for a row, in screen pixels.
fn decoration_rects(runs: &[CellRun], cell: CellSize, row_y: f32, rects: &mut Vec<PRect>) {
    let CellSize {
        width: cell_width,
        height: cell_height,
    } = cell;
    let thickness = (cell_height / 14.0).round().max(1.0);
    let base_y = row_y + cell_height - thickness - 1.0;
    let mut bar = |x: f32, y: f32, width: f32, color: [u8; 4]| {
        rects.push(PRect {
            x,
            y,
            width,
            height: thickness,
            color,
        });
    };

    for run in runs {
        let x = run.col as f32 * cell_width;
        let width = run.cells as f32 * cell_width;
        let (color, flags) = (run.underline_color, run.flags);
        // The left edge of each cell of the run.
        let cells = || (0..run.cells).map(|i| x + i as f32 * cell_width);

        match flags {
            f if f.contains(StyleFlags::DOUBLE_UNDERLINE) => {
                bar(x, base_y - thickness - 1.0, width, color);
                bar(x, base_y, width, color);
            }
            // A wave approximated by four quarter-cell steps per cell.
            f if f.contains(StyleFlags::UNDERCURL) => {
                for step in 0..run.cells * 4 {
                    let dy = thickness * [0.0, 1.0, 2.0, 1.0][step % 4];
                    let left = x + step as f32 * cell_width / 4.0;
                    bar(left, base_y - dy, cell_width / 4.0, color);
                }
            }
            f if f.contains(StyleFlags::DOTTED_UNDERLINE) => {
                let dot = (cell_width * 0.25).max(1.0);
                cells().for_each(|left| bar(left + (cell_width - dot) * 0.5, base_y, dot, color));
            }
            f if f.contains(StyleFlags::DASHED_UNDERLINE) => {
                let dash = (cell_width * 0.6).max(1.0);
                cells().for_each(|left| bar(left, base_y, dash, color));
            }
            f if f.intersects(StyleFlags::ALL_UNDERLINES) => bar(x, base_y, width, color),
            _ => {}
        }

        if flags.contains(StyleFlags::STRIKEOUT) {
            bar(x, row_y + (cell_height * 0.5).floor(), width, run.fg);
        }
    }
}

/// Appends the beam/underline cursor bar for the row containing the cursor.
/// The block cursor is folded into the cell colors by [`build_row_runs`].
fn cursor_bar_rects(
    cursor: CursorPaint,
    cell: CellSize,
    row_y: f32,
    color: [u8; 4],
    rects: &mut Vec<PRect>,
) {
    let CellSize {
        width: cell_width,
        height: cell_height,
    } = cell;
    let x = cursor.col as f32 * cell_width;
    let thickness = (cell_height / 14.0).round().max(1.0);
    match cursor.shape {
        CursorShape::Beam => {
            let width = (cell_width / 8.0).round().max(1.0);
            rects.push(PRect {
                x,
                y: row_y,
                width,
                height: cell_height,
                color,
            });
        }
        CursorShape::Underline => rects.push(PRect {
            x,
            y: row_y + cell_height - thickness - 1.0,
            width: cell_width,
            height: thickness,
            color,
        }),
        _ => {}
    }
}

/// Everything needed to paint one terminal row, in physical pixels. Owned by
/// the caller and refilled by [`paint_row`] so its allocations are reused.
#[derive(Debug, Default)]
pub struct RowPaint {
    /// Styled runs, for text shaping.
    pub runs: Vec<CellRun>,
    /// Per-run background rectangles (default-background runs are skipped).
    pub backgrounds: Vec<PRect>,
    /// Underline and strikeout rectangles, painted over the text.
    pub decorations: Vec<PRect>,
    /// Beam/underline cursor bar, painted over the text.
    pub cursor_bars: Vec<PRect>,
    /// Box-drawing, block and braille cells, expanded into `decorations`.
    sprites: Vec<SpriteCell>,
}

/// Fill `out` (cleared first) with the paint data for one row whose top edge
/// is at `row_y`. Columns beyond `width_px` are dropped (at least one is kept).
/// `cell` is the grid step in physical pixels.
pub fn paint_row(
    source: RowSource,
    view: RowView,
    colors: ColorScheme,
    width_px: u32,
    cell: CellSize,
    row_y: f32,
    out: &mut RowPaint,
) {
    let max_cols = (width_px as f32 / cell.width).floor().max(1.0) as usize;
    build_row_runs(source, view, colors, max_cols, out);

    out.backgrounds.clear();
    out.backgrounds.extend(
        out.runs
            .iter()
            .filter(|run| !run.bg_default)
            .map(|run| PRect {
                x: run.col as f32 * cell.width,
                y: row_y,
                width: run.cells as f32 * cell.width,
                height: cell.height,
                color: run.bg,
            }),
    );

    out.decorations.clear();
    decoration_rects(&out.runs, cell, row_y, &mut out.decorations);
    for &sprite_cell in &out.sprites {
        sprite::push_rects(sprite_cell, cell, row_y, &mut out.decorations);
    }

    out.cursor_bars.clear();
    if let Some(cursor) = view.cursor {
        let cursor_color = colors.resolve(AnsiColor::Named(NamedColor::Cursor));
        cursor_bar_rects(cursor, cell, row_y, cursor_color, &mut out.cursor_bars);
    }
}
