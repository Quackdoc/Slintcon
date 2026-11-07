//! Cell metrics, font discovery and shaping setup shared by both renderers.

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, Weight};
use rio_vt::crosswords::style::StyleFlags;

use super::row::{CellRun, CellSize};
use super::zoom::{FaceMetrics, MAX_FONT_PX, MIN_FONT_PX, ZoomLevels};

/// Cell metrics in physical pixels for the current font size.
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    /// Horizontal grid step in physical pixels, a whole number: the shaped
    /// advance rounded once, used for both run shaping and cursor math, so
    /// `col * cell_width` is exactly the x of column `col`.
    pub cell_width: u32,
    pub cell_height: u32,
    pub font_size: f32,
}

/// Rasterization key for a shaped glyph with its origin snapped to whole
/// pixels, so mark offsets don't land on a subpixel bin.
pub fn pixel_glyph(glyph: &cosmic_text::LayoutGlyph) -> cosmic_text::PhysicalGlyph {
    let x = glyph.font_size.mul_add(glyph.x_offset, glyph.x).round();
    let y = (glyph.y - glyph.font_size * glyph.y_offset).round();
    let (cache_key, x, y) = cosmic_text::CacheKey::new(
        glyph.font_id,
        glyph.glyph_id,
        glyph.font_size,
        (x, y),
        glyph.font_weight,
        glyph.cache_key_flags,
    );
    cosmic_text::PhysicalGlyph { cache_key, x, y }
}

/// Preferred terminal font family.
const FONT_FAMILY: &str = "Iosevka Term SS12";

/// Tried in order when `FONT_FAMILY` is not available. If none is installed the
/// generic monospace family is used.
const FALLBACK_FAMILIES: &[&str] = &[
    "JetBrains Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Hack",
    "Fira Mono",
    "Fira Code",
    "Source Code Pro",
    "Ubuntu Mono",
    "Cascadia Mono",
    "Menlo",
    "Consolas",
    "Courier New",
];

/// Embedded Iosevka Term SS12 faces (SIL OFL, see assets/fonts/LICENSE-Iosevka.md).
/// Only compiled in with the `bundled-font` feature.
#[cfg(feature = "bundled-font")]
static EMBEDDED_FONTS: &[&[u8]] = &[
    include_bytes!("../../assets/fonts/IosevkaTermSS12-Regular.ttf"),
    include_bytes!("../../assets/fonts/IosevkaTermSS12-Italic.ttf"),
    include_bytes!("../../assets/fonts/IosevkaTermSS12-Bold.ttf"),
    include_bytes!("../../assets/fonts/IosevkaTermSS12-BoldItalic.ttf"),
];

fn has_family(font_system: &FontSystem, name: &str) -> bool {
    font_system.db().faces().any(|face| {
        face.families
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
    })
}

/// Pick the preferred family if present, else the first installed fallback,
/// else the generic monospace family.
fn resolve_family(font_system: &FontSystem) -> cosmic_text::Family<'static> {
    std::iter::once(&FONT_FAMILY)
        .chain(FALLBACK_FAMILIES)
        .find(|name| has_family(font_system, name))
        .map_or(cosmic_text::Family::Monospace, |name| {
            cosmic_text::Family::Name(name)
        })
}

/// Font environment shared by both renderers: the cosmic-text font system plus
/// the measured cell metrics and the shaping attributes templates.
pub struct FontEnv {
    pub font_system: FontSystem,
    pub metrics: CellMetrics,
    /// The primary face's metrics and the zoom levels derived from them.
    zoom: ZoomLevels,
    attrs_template: Attrs<'static>,
    /// How `set_monospace_width` must be called to snap advances to the grid.
    mono_width_mode: MonoWidthMode,
}

/// cosmic-text 0.19 rounds advances in mixed pixel and em units, so any advance
/// other than the cell width lands on the wrong multiple. Passing
/// `width * font_size` fixes it; the behavior is probed at startup so a fixed
/// cosmic-text gets the plain width again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MonoWidthMode {
    /// `set_monospace_width(cell_width)` rounds to multiples of `cell_width`.
    Pixels,
    /// `set_monospace_width(cell_width * font_size)` does.
    ScaledByFontSize,
}

impl FontEnv {
    pub fn new(font_px: u32) -> Self {
        let font_px = font_px.clamp(MIN_FONT_PX, MAX_FONT_PX);
        #[allow(unused_mut)]
        let mut font_system = FontSystem::new();
        // System fonts stay loaded as fallback for glyphs the primary font lacks.
        #[cfg(feature = "bundled-font")]
        for data in EMBEDDED_FONTS {
            font_system.db_mut().load_font_data(data.to_vec());
        }
        let family = resolve_family(&font_system);
        let mut env = Self {
            font_system,
            metrics: CellMetrics {
                cell_width: 1,
                cell_height: 1,
                font_size: font_px as f32,
            },
            zoom: ZoomLevels::new(FaceMetrics::FALLBACK),
            attrs_template: Attrs::new().family(family),
            mono_width_mode: MonoWidthMode::Pixels,
        };
        env.mono_width_mode = env.probe_mono_width_mode();
        env.zoom = ZoomLevels::new(env.measure_face());
        env.remeasure(font_px);
        env
    }

    /// The cell size in pixels, or `None` while either dimension is zero.
    pub fn cell_size(&self) -> Option<CellSize> {
        let CellMetrics {
            cell_width,
            cell_height,
            ..
        } = self.metrics;
        (cell_width > 0 && cell_height > 0).then_some(CellSize {
            width: cell_width as f32,
            height: cell_height as f32,
        })
    }

    /// The zoom levels for the primary face.
    pub fn zoom(&self) -> &ZoomLevels {
        &self.zoom
    }

    /// Measure the face: shape a space for its advance, plus the line metrics.
    /// The probe size is a power of two so dividing by it is exact.
    fn measure_face(&mut self) -> FaceMetrics {
        const PROBE_SIZE: f32 = 64.0;
        let attrs = self.default_attrs();
        let mut probe = Buffer::new(
            &mut self.font_system,
            Metrics::new(PROBE_SIZE, PROBE_SIZE * 1.25),
        );
        probe.set_size(None, None);
        probe.set_text(" ", &attrs, Shaping::Advanced, None);
        probe.shape_until_scroll(&mut self.font_system, false);
        let Some(glyph) = probe
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().cloned())
        else {
            return FaceMetrics::FALLBACK;
        };
        let mut face = FaceMetrics::FALLBACK;
        if glyph.w > 0.0 {
            face.advance_em = glyph.w / PROBE_SIZE;
        }
        if let Some(font) = self.font_system.get_font(glyph.font_id, glyph.font_weight) {
            let m = font.metrics();
            let height = (m.ascent - m.descent + m.leading) / m.units_per_em as f32;
            if height > 0.0 {
                face.line_height_em = height;
            }
        }
        face
    }

    /// Recompute the cell metrics for a new absolute font size. Any size is
    /// accepted; the sharp ones are listed by [`Self::zoom`].
    pub fn remeasure(&mut self, font_px: u32) {
        let font_px = font_px.clamp(MIN_FONT_PX, MAX_FONT_PX);
        let face = self.zoom.face();
        self.metrics = CellMetrics {
            cell_width: face.cell_width(font_px),
            cell_height: face.cell_height(font_px),
            font_size: font_px as f32,
        };
    }

    /// Detect how cosmic-text interprets `set_monospace_width`: a space shaped
    /// at 16px with width 0.8n comes out 0.8n if correct, n if units are mixed.
    fn probe_mono_width_mode(&mut self) -> MonoWidthMode {
        const PROBE_SIZE: f32 = 16.0;
        let attrs = self.default_attrs();
        let mut advance = |width: Option<f32>| -> Option<f32> {
            let mut buffer = Buffer::new(
                &mut self.font_system,
                Metrics::new(PROBE_SIZE, PROBE_SIZE * 1.25),
            );
            buffer.set_size(None, None);
            buffer.set_monospace_width(width);
            buffer.set_text(" ", &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut self.font_system, false);
            let w = buffer.layout_runs().next()?.glyphs.first()?.w;
            Some(w)
        };
        let Some(natural) = advance(None).filter(|w| *w > 0.0) else {
            return MonoWidthMode::Pixels;
        };
        let snapped = advance(Some(natural * 0.8)).unwrap_or(natural * 0.8);
        if (snapped - natural).abs() < 0.01 {
            MonoWidthMode::ScaledByFontSize
        } else {
            MonoWidthMode::Pixels
        }
    }

    /// The value for `set_monospace_width` that snaps every advance to a
    /// whole multiple of `cell_width`.
    fn monospace_width(&self) -> f32 {
        match self.mono_width_mode {
            MonoWidthMode::Pixels => self.metrics.cell_width as f32,
            MonoWidthMode::ScaledByFontSize => {
                self.metrics.cell_width as f32 * self.metrics.font_size
            }
        }
    }

    pub fn default_attrs(&self) -> Attrs<'static> {
        self.attrs_template.clone()
    }

    /// Create an empty single-line buffer configured for grid-exact text.
    pub fn make_buffer(&mut self) -> Buffer {
        let metrics = Metrics::new(self.metrics.font_size, self.metrics.cell_height as f32);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(None, None);
        // Every advance snaps to a multiple of the cell width, so glyph x is
        // `col * cell_width`.
        buffer.set_monospace_width(Some(self.monospace_width()));
        buffer.set_wrap(cosmic_text::Wrap::None);
        buffer
    }

    /// Attrs for one run: color, weight and italic plus a disabled-ligature
    /// feature set so terminal cells never merge glyphs across columns.
    pub fn run_attrs(&self, run: &CellRun) -> Attrs<'static> {
        let mut attrs = self.default_attrs();
        let color = cosmic_text::Color::rgba(run.fg[0], run.fg[1], run.fg[2], run.fg[3]);
        attrs = attrs.color(color);
        if run.flags.contains(StyleFlags::BOLD) {
            attrs = attrs.weight(Weight::BOLD);
        }
        if run.flags.contains(StyleFlags::ITALIC) {
            attrs = attrs.style(cosmic_text::Style::Italic);
        }
        // Ligatures would visually join adjacent cells.
        let mut features = cosmic_text::FontFeatures::new();
        features.disable(cosmic_text::FeatureTag::STANDARD_LIGATURES);
        features.disable(cosmic_text::FeatureTag::DISCRETIONARY_LIGATURES);
        attrs = attrs.font_features(features);
        attrs
    }

    /// Replace a row buffer's contents with the given runs.
    pub fn set_row_text(&mut self, buffer: &mut Buffer, runs: &[CellRun]) {
        let spans = runs
            .iter()
            .filter(|run| !run.text.is_empty())
            .map(|run| (run.text.as_str(), self.run_attrs(run)));
        buffer.set_rich_text(spans, &self.default_attrs(), Shaping::Advanced, None);
    }

    /// Call once per painted frame: drops shaped runs that were not used for a
    /// while, so the run cache stays bounded.
    pub fn end_frame(&mut self) {
        self.font_system.shape_run_cache.trim(SHAPE_CACHE_FRAMES);
    }
}

/// Painted frames an unused shaped run stays cached.
const SHAPE_CACHE_FRAMES: u64 = 64;
