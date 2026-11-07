//! Groups of `TerminalApp` state that change together.

use std::time::{Duration, Instant};

use rio_vt::config::colors::term::TermColors;
use rio_vt::crosswords::grid::row::Row;
use rio_vt::crosswords::square::{Extras, Square};
use rio_vt::crosswords::style::Style;
use rustc_hash::FxHashMap;

use super::OverlayQuad;
use crate::Config;
use crate::term_style::{self, RowView, ZoomLevels};

/// The visible screen as last read from the terminal.
#[derive(Default)]
pub(super) struct Snapshot {
    /// The visible grid rows.
    pub rows: Vec<Row<Square>>,
    /// The style of every cell, `[row][col]`, parallel to `rows`.
    pub cells: Vec<Vec<Style>>,
    /// Extra cell data (e.g. hyperlinks, underline colours) by id.
    pub extras: FxHashMap<u16, Extras>,
}

/// The grid as last read from the terminal, and what it was last painted with.
pub(super) struct FrameCache {
    /// The visible screen as last read from the terminal.
    pub snapshot: Snapshot,
    /// Reusable per-frame mask of rows to repaint, parallel to `rows`.
    pub dirty_rows: Vec<bool>,
    /// The terminal's colour palette as last read.
    pub term_colors: TermColors,
    /// Columns in the grid at the current size.
    pub grid_cols: usize,
    /// Overlays painted last frame; rows they covered must be repainted when
    /// the set changes, since the CPU renderer only redraws dirty rows.
    pub last_overlays: Vec<OverlayQuad>,
    /// The window title last sent to the UI.
    pub last_title: String,
    /// What each row was last painted with besides its cells (cursor,
    /// selection); neither carries terminal damage, so rows are found by
    /// comparing against this.
    pub views: Vec<RowView>,
}

impl FrameCache {
    /// An empty cache for a grid `grid_cols` wide.
    pub fn new(grid_cols: usize) -> Self {
        Self {
            snapshot: Snapshot::default(),
            dirty_rows: Vec::new(),
            term_colors: TermColors::default(),
            grid_cols,
            last_overlays: Vec::new(),
            last_title: String::new(),
            views: Vec::new(),
        }
    }
}

/// The font size and whether it still follows the automatic default.
///
/// The automatic default depends on the renderer's zoom levels, and the
/// renderer needs a size to be built. So `new` gives a provisional size to
/// build the renderer with, and `refit` then settles the real one.
pub(super) struct FontSize {
    /// Absolute size in physical pixels: the default zoom level, an explicit
    /// `Config::font_px`, or whatever Ctrl+/Ctrl- stepped to.
    pub px: u32,
    /// True while `px` still follows the automatic default.
    pub auto: bool,
    /// Columns the automatic size aims to fit (see `Config::fit_columns`).
    pub fit_columns: Option<u32>,
    /// `Config::font_px`: what a request of 0 falls back to.
    configured: Option<u32>,
}

impl FontSize {
    /// An explicit `Config::font_px` is kept; otherwise the size is automatic
    /// and starts at the default font size until `refit` runs.
    pub fn new(config: &Config) -> Self {
        Self {
            px: config.font_px.unwrap_or(term_style::FONT_SIZE as u32),
            auto: config.font_px.is_none(),
            fit_columns: config.fit_columns,
            configured: config.font_px,
        }
    }

    /// Apply the widget's `font-size` (physical pixels): 0 falls back to the
    /// configured size, or to the automatic one if there is none.
    pub fn request(&mut self, px: u32) {
        match (px > 0).then_some(px).or(self.configured) {
            Some(px) => {
                self.set(px);
            }
            None => self.auto = true,
        }
    }

    /// Recompute the automatic size for a viewport; no-op for an explicit size.
    pub fn refit(&mut self, zoom: &ZoomLevels, width: u32, scale: f32) {
        if self.auto {
            self.px = term_style::default_font_px(zoom, self.fit_columns, width, scale);
        }
    }

    /// Pin an explicit size, clamped; true if it changed.
    pub fn set(&mut self, px: u32) -> bool {
        let px = px.clamp(term_style::MIN_FONT_PX, term_style::MAX_FONT_PX);
        self.auto = false;
        std::mem::replace(&mut self.px, px) != px
    }
}

/// When the UI should refresh: busy/idle backoff.
pub(super) struct Pacing {
    /// Last real change (painted terminal content or selection) or input.
    last_activity: Instant,
    /// How long after activity the UI keeps its fast refresh rate.
    idle_delay: Duration,
    /// Refresh interval while busy.
    frame_interval: Duration,
    /// Refresh interval once idle.
    idle_interval: Duration,
}

impl Pacing {
    /// Pacing from the host's `Config`.
    pub fn new(config: &Config) -> Self {
        Self {
            last_activity: Instant::now(),
            idle_delay: config.idle_delay,
            frame_interval: config.frame_interval,
            idle_interval: config.idle_interval,
        }
    }

    /// Record a change or input at `now`.
    pub fn touch(&mut self, now: Instant) {
        self.last_activity = now;
    }

    /// How often the UI should refresh at `now`: fast while something changed
    /// or was typed recently, slow once idle.
    pub fn interval(&self, now: Instant) -> Duration {
        if now.duration_since(self.last_activity) < self.idle_delay {
            self.frame_interval
        } else {
            self.idle_interval
        }
    }
}

/// The widget's size in physical pixels and the scale factor.
pub(super) struct Viewport {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// Scale factor from logical to physical pixels.
    pub scale: f32,
    /// Reported by the widget but not yet applied to the renderer and pty.
    pub stale: bool,
}
