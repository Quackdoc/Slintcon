//! Palette resolution and color space conversions.

use rio_vt::config::colors::term::{List, TermColors};
use rio_vt::config::colors::{AnsiColor, ColorArray, Colors, NamedColor};

pub fn default_palette() -> List {
    List::from(&Colors::default())
}

pub fn default_background(palette: &List) -> [u8; 4] {
    to_rgba8(palette[NamedColor::Background])
}

/// The palette and the colors the program overrode at run time.
#[derive(Clone, Copy)]
pub struct ColorScheme<'a> {
    /// Colors the program set with escape sequences.
    pub term: &'a TermColors,
    /// The built-in palette, for what the program did not override.
    pub palette: &'a List,
}

impl ColorScheme<'_> {
    /// Resolve any ANSI color to straight RGBA8.
    pub(super) fn resolve(&self, color: AnsiColor) -> [u8; 4] {
        match color {
            AnsiColor::Named(named) => to_rgba8(self.term[named].unwrap_or(self.palette[named])),
            AnsiColor::Indexed(index) => {
                to_rgba8(self.term[index as usize].unwrap_or(self.palette[index as usize]))
            }
            AnsiColor::Spec(rgb) => [rgb.r, rgb.g, rgb.b, 255],
        }
    }
}

fn to_rgba8(color: ColorArray) -> [u8; 4] {
    [
        (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        255,
    ]
}

pub(super) fn brighten(color: AnsiColor) -> AnsiColor {
    match color {
        AnsiColor::Named(named) => AnsiColor::Named(match named {
            NamedColor::Black => NamedColor::LightBlack,
            NamedColor::Red => NamedColor::LightRed,
            NamedColor::Green => NamedColor::LightGreen,
            NamedColor::Yellow => NamedColor::LightYellow,
            NamedColor::Blue => NamedColor::LightBlue,
            NamedColor::Magenta => NamedColor::LightMagenta,
            NamedColor::Cyan => NamedColor::LightCyan,
            NamedColor::White => NamedColor::LightWhite,
            other => other,
        }),
        AnsiColor::Indexed(index) if index < 8 => AnsiColor::Indexed(index + 8),
        other => other,
    }
}
