//! Pointer input: mouse reports, wheel and text selection.

use std::time::{Duration, Instant};

use rio_vt::crosswords::Mode;
use rio_vt::crosswords::grid::Scroll;
use rio_vt::crosswords::pos::{Column, Line, Pos, Side};
use rio_vt::selection::{Selection, SelectionType};

use crate::clipboard;
use crate::key::Modifiers;
use crate::mouse::{self, Button, Clicks, Protocol};
use crate::term_style::Selected;

use super::*;

/// How often a drag past the top or bottom edge scrolls one line.
pub(super) const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// Most wheel reports sent for one scroll event.
pub(super) const MAX_WHEEL_REPORTS: i32 = 20;

/// What the pointer is doing.
#[derive(Default)]
pub(super) struct PointerState {
    clicks: Clicks,
    /// Where the pointer last was.
    cell: Option<mouse::Cell>,
    /// The button whose press went to the program (its release must too,
    /// even if the program has since stopped asking for the mouse).
    reported: Option<Button>,
    /// The last cell sent as motion, so one cell is reported once.
    reported_cell: Option<(usize, usize)>,
    /// A drag is extending the selection.
    selecting: bool,
    /// -1 above the grid, 1 below: the drag scrolls while it stays there.
    autoscroll: i32,
    last_autoscroll: Option<Instant>,
}

/// The selected columns of each viewport row (`out` is reused).
pub(super) fn selection_rows_into(term: &Term, out: &mut Vec<Option<Selected>>) {
    out.clear();
    let range = term.selection.as_ref().and_then(|s| s.to_range(term));
    let last_col = term.columns().saturating_sub(1);
    let offset = term.display_offset() as i32;

    out.extend((0..term.screen_lines()).map(|row| {
        let range = range.as_ref()?;
        let line = row as i32 - offset;
        let (start, end) = (range.start, range.end);
        if !(start.row.0..=end.row.0).contains(&line) {
            return None;
        }
        let (first, last) = if range.is_block {
            (start.col.0.min(end.col.0), start.col.0.max(end.col.0))
        } else {
            (
                if line == start.row.0 { start.col.0 } else { 0 },
                if line == end.row.0 {
                    end.col.0
                } else {
                    last_col
                },
            )
        };
        Some(Selected {
            first: first.min(last_col),
            last: last.min(last_col),
        })
    }));
}

/// The grid position and side of a cell as the selection stores them.
fn grid_point(term: &Term, cell: mouse::Cell) -> (Pos, Side) {
    let point = Pos::new(
        Line(cell.row as i32 - term.display_offset() as i32),
        Column(cell.col),
    );
    (point, if cell.right { Side::Right } else { Side::Left })
}

/// Extend the selection, if there is one, to `cell`.
fn extend_selection(term: &mut Term, cell: mouse::Cell) {
    let (point, side) = grid_point(term, cell);
    if let Some(selection) = term.selection.as_mut() {
        selection.update(point, side);
    }
}

impl TerminalApp {
    /// The wheel: reports to programs that asked for the mouse, cursor keys on
    /// the alternate screen, else history scrolling. Shift always scrolls.
    pub fn handle_scroll(&mut self, delta_y: f32, x: f32, y: f32, mods: Modifiers) {
        if delta_y == 0.0 {
            return;
        }
        self.note_input();
        let mods = self.external_modifiers.unwrap_or(mods);

        // Wheel notches are ~50px, trackpads send many small events: convert to
        // rows, but never lose an event entirely.
        let rows = (-delta_y / self.renderer.cell_height().max(1) as f32).round() as i32;
        let delta = if rows != 0 {
            rows
        } else {
            -delta_y.signum() as i32
        };
        let (up, lines) = (delta > 0, delta.abs().min(MAX_WHEEL_REPORTS));

        let mode = self.term.lock().mode();
        let protocol = Protocol::from_mode(mode);
        let reporting = protocol.active() && !mods.contains(Modifiers::SHIFT);
        match (
            reporting,
            mode.contains(Mode::ALT_SCREEN),
            mode.contains(Mode::ALTERNATE_SCROLL),
        ) {
            (true, ..) => {
                let (cell, _) = self.locate(x, y);
                let wheel = if up {
                    mouse::Wheel::Up
                } else {
                    mouse::Wheel::Down
                };
                self.report_mouse(protocol, mouse::Event::Wheel(wheel), cell, mods, lines);
            }
            (false, false, _) => self.term.lock().scroll_display(Scroll::Delta(delta)),
            (false, true, true) => {
                let intro = if mode.contains(Mode::APP_CURSOR) {
                    "\x1bO"
                } else {
                    "\x1b["
                };
                let key = format!("{intro}{}", if up { 'A' } else { 'B' });
                self.send_pty(key.repeat(lines as usize).into_bytes());
            }
            // The program owns the wheel and did not ask for it.
            (false, true, false) => {}
        }
    }

    /// Where a pointer position (logical pixels) falls on the grid, and
    /// whether it is above (-1) or below (1) it.
    pub(super) fn locate(&self, x: f32, y: f32) -> (mouse::Cell, i32) {
        let scale = self.viewport.scale;
        mouse::locate(
            x * scale,
            y * scale,
            self.renderer.cell_width() as f32,
            self.renderer.cell_height() as f32,
            self.cache.grid_cols,
            self.cache.snapshot.rows.len(),
        )
    }

    /// Send a mouse report to the program, `times` over, if the protocol has one.
    fn report_mouse(
        &mut self,
        protocol: Protocol,
        event: mouse::Event,
        cell: mouse::Cell,
        mods: Modifiers,
        times: i32,
    ) {
        if let Some(bytes) = mouse::report(protocol, event, mods, cell.col, cell.row) {
            self.send_pty(bytes.repeat(times as usize));
        }
    }

    /// A pointer press, release or move at (`x`, `y`) logical pixels. Programs
    /// that asked for the mouse get reports unless Shift is held; see
    /// `docs/mouse.md` for the selection gestures.
    pub fn handle_pointer(
        &mut self,
        kind: mouse::Kind,
        button: Button,
        x: f32,
        y: f32,
        mods: Modifiers,
    ) {
        use mouse::Kind;
        self.note_input();
        let mods = self.external_modifiers.unwrap_or(mods);
        let (cell, outside) = self.locate(x, y);
        self.pointer.cell = Some(cell);
        let protocol = self.protocol;
        let reporting = protocol.active() && !mods.contains(Modifiers::SHIFT);
        let at = (cell.col, cell.row);

        match kind {
            Kind::Down if reporting => {
                self.report_mouse(protocol, mouse::Event::Press(button), cell, mods, 1);
                if self.pointer.reported.is_none() && button != Button::Other {
                    self.pointer.reported = Some(button);
                }
                self.pointer.reported_cell = Some(at);
            }
            Kind::Down => self.press_selection(button, cell, mods),
            Kind::Up | Kind::Cancel => {
                if self.pointer.reported == Some(button) {
                    self.pointer.reported = None;
                    if kind == Kind::Up {
                        self.report_mouse(protocol, mouse::Event::Release(button), cell, mods, 1);
                    }
                } else if self.pointer.selecting {
                    self.finish_selection();
                }
            }
            Kind::Move => {
                if self.pointer.reported.is_some() || (reporting && protocol.motion) {
                    if self.pointer.reported_cell != Some(at) {
                        self.pointer.reported_cell = Some(at);
                        let event = mouse::Event::Motion(self.pointer.reported);
                        self.report_mouse(protocol, event, cell, mods, 1);
                    }
                } else if self.pointer.selecting {
                    self.drag_selection(cell, outside);
                }
            }
        }
    }

    pub(super) fn press_selection(&mut self, button: Button, cell: mouse::Cell, mods: Modifiers) {
        match button {
            Button::Left => {
                let clicks = &mut self.pointer.clicks;
                let count = clicks.register(Instant::now(), (cell.col, cell.row), button);
                let mut term = self.term.lock();
                let ty = match count {
                    // Shift+click extends the selection there is.
                    1 if mods.contains(Modifiers::SHIFT) && term.selection.is_some() => None,
                    2 => Some(SelectionType::Semantic),
                    3 => Some(SelectionType::Lines),
                    _ if mods.contains(Modifiers::CTRL | Modifiers::ALT) => {
                        Some(SelectionType::Block)
                    }
                    _ => Some(SelectionType::Simple),
                };
                match ty {
                    None => extend_selection(&mut term, cell),
                    Some(ty) => {
                        let (point, side) = grid_point(&term, cell);
                        term.selection = Some(Selection::new(ty, point, side));
                    }
                }
                self.pointer.selecting = true;
            }
            Button::Right => {
                let mut term = self.term.lock();
                if term.selection.is_some() {
                    extend_selection(&mut term, cell);
                    self.pointer.selecting = true;
                }
            }
            Button::Middle => self.paste(clipboard::Kind::Primary),
            Button::Other => {}
        }
    }

    pub(super) fn drag_selection(&mut self, cell: mouse::Cell, outside: i32) {
        self.pointer.autoscroll = outside;
        extend_selection(&mut self.term.lock(), cell);
    }

    /// Scroll while a drag is held above or below the grid.
    pub(super) fn autoscroll(&mut self) {
        let direction = self.pointer.autoscroll;
        if !self.pointer.selecting || direction == 0 {
            return;
        }
        let now = Instant::now();
        if self
            .pointer
            .last_autoscroll
            .is_some_and(|at| now.duration_since(at) < AUTOSCROLL_INTERVAL)
        {
            return;
        }
        self.pointer.last_autoscroll = Some(now);
        let Some(cell) = self.pointer.cell else {
            return;
        };
        let mut term = self.term.lock();
        term.scroll_display(Scroll::Delta(-direction));
        extend_selection(&mut term, cell);
    }

    /// The button came up: a click without a drag selects nothing, anything
    /// else becomes the primary selection.
    pub(super) fn finish_selection(&mut self) {
        self.pointer.selecting = false;
        self.pointer.autoscroll = 0;
        let text = {
            let mut term = self.term.lock();
            if term.selection.as_ref().is_none_or(|s| s.is_empty()) {
                term.selection = None;
                None
            } else {
                term.selection_to_string()
            }
        };
        if let Some(text) = text {
            self.clipboard.set(clipboard::Kind::Primary, text);
        }
    }

    /// Copy the selection to the clipboard.
    pub(super) fn copy_selection(&mut self) {
        let text = self.term.lock().selection_to_string();
        if let Some(text) = text {
            self.clipboard.set(clipboard::Kind::Clipboard, text);
        }
    }
}
