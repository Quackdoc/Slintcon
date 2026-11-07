//! Turning terminal state into painted rows.

use std::time::Instant;

use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::pos::CursorState;
use rio_vt::event::TerminalDamage;

use crate::images::OverlayQuad;
use crate::overlays;
use crate::term_style::{CursorPaint, RowSource, RowView};

use super::*;

/// The cursor and the row it is on, or `None` when hidden or off the grid.
fn cursor_on_grid(cursor: &CursorState, rows: usize, cols: usize) -> Option<(usize, CursorPaint)> {
    if cursor.content == CursorShape::Hidden {
        return None;
    }
    let row = usize::try_from(cursor.pos.row.0).ok()?;
    if row >= rows || cursor.pos.col.0 >= cols {
        return None;
    }
    let paint = CursorPaint {
        col: cursor.pos.col.0,
        shape: cursor.content,
    };
    Some((row, paint))
}

impl TerminalApp {
    /// Copies what changed out of the terminal and repaints the rows whose
    /// content or view differ from what is on screen.
    pub(super) fn render(&mut self) {
        if !self.display_active {
            return;
        }
        let now = Instant::now();

        let shared = Arc::clone(&self.term);
        let mut term = shared.lock();
        let graphics_changed = self.apply_pending_graphics(&mut term);
        let damage = self.pending_damage(&term, graphics_changed);
        let changed = damage != TerminalDamage::Noop;
        self.copy_visible_rows(&mut term, &damage);
        self.cache.term_colors = *term.colors();
        let views = self.row_views(&term);
        // Placements only move with terminal changes.
        let overlays = changed.then(|| {
            overlays::collect(
                &term,
                &self.cache.snapshot.rows,
                &self.cache.snapshot.cells,
                &self.cache.snapshot.extras,
                self.renderer.cell_width().max(1) as f32,
                self.renderer.cell_height().max(1) as f32,
            )
        });
        let title = term.title.clone();
        drop(term);

        self.mark_dirty_rows(&views, overlays.as_deref());
        self.cache.views = views;
        if self.cache.dirty_rows.contains(&true) {
            self.pacing.touch(now);
            self.paint_dirty_rows(overlays);
        }
        self.set_title(title);
    }

    /// What each visible row should show: its selection, and the cursor on
    /// the row that has it.
    fn row_views(&self, term: &Term) -> Vec<RowView> {
        let mut selection = Vec::new();
        selection_rows_into(term, &mut selection);
        let mut views: Vec<RowView> = selection
            .into_iter()
            .take(self.cache.snapshot.rows.len())
            .map(|selection| RowView {
                cursor: None,
                selection,
            })
            .collect();
        let cursor = cursor_on_grid(&term.cursor(), views.len(), self.cache.grid_cols);
        if let Some((row, paint)) = cursor {
            views[row].cursor = Some(paint);
        }
        views
    }

    /// Hands queued graphics to the renderer; true if the images changed.
    fn apply_pending_graphics(&mut self, term: &mut Term) -> bool {
        let mut changed = false;
        if let Some(queues) = self.pending_graphics.take() {
            self.renderer.apply_images(queues);
            changed = true;
        }
        if term.graphics.kitty_graphics_dirty {
            term.graphics.kitty_graphics_dirty = false;
            changed = true;
        }
        changed
    }

    /// What must be recopied: everything on first paint, a requested redraw or
    /// a graphics change, otherwise whatever rio recorded.
    fn pending_damage(&self, term: &Term, graphics_changed: bool) -> TerminalDamage {
        if self.cache.snapshot.rows.is_empty() || graphics_changed || self.full_redraw {
            TerminalDamage::Full
        } else {
            term.peek_damage_event().unwrap_or(TerminalDamage::Noop)
        }
    }

    /// Copies the damaged rows into `self.cache.snapshot.rows` and tells rio they were taken.
    fn copy_visible_rows(&mut self, term: &mut Term, damage: &TerminalDamage) {
        term.snapshot_visible(
            damage,
            &mut self.cache.snapshot.rows,
            &mut self.cache.snapshot.cells,
            &mut self.cache.snapshot.extras,
        );
        term.reset_damage();
        term.damage_event_in_flight = false;
        self.full_redraw = false;
    }

    /// Flags the rows to repaint in `self.cache.dirty_rows`: those rio changed, those
    /// whose view differs from the painted one, and those under an image
    /// placement that moved or vanished (`overlays` is `None` when unchanged).
    fn mark_dirty_rows(&mut self, views: &[RowView], overlays: Option<&[OverlayQuad]>) {
        self.cache.dirty_rows.clear();
        let painted = &self.cache.views;
        self.cache.dirty_rows.extend(
            self.cache
                .snapshot
                .rows
                .iter_mut()
                .zip(views.iter().enumerate())
                .map(|(row, (i, view))| {
                    std::mem::take(&mut row.dirty) || painted.get(i) != Some(view)
                }),
        );

        let Some(overlays) = overlays.filter(|new| *new != self.cache.last_overlays) else {
            return;
        };
        // Overlays are only blitted while a row is redrawn.
        let cell_height = self.renderer.cell_height().max(1) as f32;
        for quad in self.cache.last_overlays.iter().chain(overlays) {
            let first = (quad.y / cell_height).floor().max(0.0) as usize;
            let last = ((quad.y + quad.height) / cell_height).ceil().max(0.0) as usize;
            for flag in self.cache.dirty_rows.iter_mut().take(last).skip(first) {
                *flag = true;
            }
        }
        self.cache.last_overlays = overlays.to_vec();
    }

    /// Paints the flagged rows in ascending order and hands the image to Slint.
    fn paint_dirty_rows(&mut self, overlays: Option<Vec<OverlayQuad>>) {
        // The CPU renderer blits overlays while drawing each row, so they must
        // be set first.
        if let Some(overlays) = overlays {
            self.renderer.set_overlays(overlays);
        }
        for (row, view) in self.cache.views.iter().enumerate() {
            if !self.cache.dirty_rows[row] {
                continue;
            }
            let source = RowSource {
                row: &self.cache.snapshot.rows[row],
                styles: &self.cache.snapshot.cells[row],
                extras: &self.cache.snapshot.extras,
            };
            self.renderer
                .draw_row(source, row, *view, &self.cache.term_colors);
        }
        if let Some(image) = self.renderer.image()
            && let Some(api) = self.api.upgrade()
        {
            api.set_frame(image);
        }
        self.renderer.end_frame();
    }

    pub(super) fn set_title(&mut self, title: String) {
        // Deferred while inactive: `render` sets the current title on resume.
        if !self.display_active || title == self.cache.last_title {
            return;
        }
        self.cache.last_title = title.clone();
        if let Some(api) = self.api.upgrade() {
            api.set_title(title.into());
        }
    }
}
