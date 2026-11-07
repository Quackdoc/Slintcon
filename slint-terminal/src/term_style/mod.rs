//! Styling shared by the CPU and GPU renderers: palette ([`color`]), cell runs
//! with decoration and cursor geometry ([`row`]), font metrics ([`font`]) and
//! zoom levels ([`zoom`]).

mod color;
mod font;
mod row;
mod sprite;
mod zoom;

pub use color::{ColorScheme, default_background, default_palette};
pub use font::{FontEnv, pixel_glyph};
pub use row::{
    CellRun, CursorPaint, PRect, RowPaint, RowSource, RowView, Selected, paint_row, text_segments,
};
pub use zoom::{FONT_SIZE, MAX_FONT_PX, MIN_FONT_PX, ZoomLevels, default_font_px};
