//! Mouse handling that does not depend on the window: encoding reports for
//! programs that ask for the mouse, counting clicks, and mapping pixels to
//! cells.

use std::time::{Duration, Instant};

use crate::key::Modifiers;

/// Clicks closer together than this (on the same cell) are a double click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    Middle,
    Right,
    /// Back, forward and the like: nothing is bound to them.
    Other,
}

impl Button {
    /// Slint's `PointerEventButton` as passed through the `.slint` file.
    pub fn from_index(index: i32) -> Self {
        match index {
            0 => Self::Left,
            1 => Self::Middle,
            2 => Self::Right,
            _ => Self::Other,
        }
    }

    /// The button number in a report (before modifiers and motion bits).
    fn code(self) -> Option<u8> {
        match self {
            Self::Left => Some(0),
            Self::Middle => Some(1),
            Self::Right => Some(2),
            Self::Other => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Down,
    Up,
    Move,
    /// The grab was lost; treated as a release without any effect.
    Cancel,
}

impl Kind {
    pub fn from_index(index: i32) -> Self {
        match index {
            0 => Self::Down,
            1 => Self::Up,
            2 => Self::Move,
            _ => Self::Cancel,
        }
    }
}

/// How a program asked to receive reports (DECSET 9/1000/1002/1003, 1005,
/// 1006).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Protocol {
    /// Button press only, no modifiers (mode 9).
    pub x10: bool,
    /// Presses, releases and the wheel (mode 1000 and above).
    pub clicks: bool,
    /// Motion with a button held (mode 1002 and above).
    pub drag: bool,
    /// Motion with or without a button (mode 1003).
    pub motion: bool,
    /// `CSI < b ; x ; y M/m` (mode 1006).
    pub sgr: bool,
    /// Coordinates as UTF-8 (mode 1005).
    pub utf8: bool,
}

impl Protocol {
    pub fn from_mode(mode: rio_vt::crosswords::Mode) -> Self {
        use rio_vt::crosswords::Mode;
        Self {
            x10: mode.contains(Mode::MOUSE_REPORT_X10),
            clicks: mode.contains(Mode::MOUSE_REPORT_CLICK),
            drag: mode.contains(Mode::MOUSE_DRAG),
            motion: mode.contains(Mode::MOUSE_MOTION),
            sgr: mode.contains(Mode::SGR_MOUSE),
            utf8: mode.contains(Mode::UTF8_MOUSE),
        }
    }

    /// Whether the program wants the mouse at all.
    pub fn active(&self) -> bool {
        self.x10 || self.clicks || self.drag || self.motion
    }
}

/// What happened, as reports distinguish it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Press(Button),
    Release(Button),
    /// Motion; the button held, if any.
    Motion(Option<Button>),
    /// Wheel: up or down (horizontal scrolling is not reported).
    Wheel(Wheel),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wheel {
    Up,
    Down,
}

/// The bytes to send for `event` at the 0-based cell (`col`, `row`), or
/// `None` when the protocol does not report it (or it cannot be encoded).
pub fn report(
    protocol: Protocol,
    event: Event,
    mods: Modifiers,
    col: usize,
    row: usize,
) -> Option<Vec<u8>> {
    // The base code, whether it is a release, and the motion bit.
    let (mut code, release, motion) = match event {
        Event::Press(button) => (button.code()?, false, false),
        Event::Release(button) => (button.code()?, true, false),
        Event::Motion(button) => {
            let allowed = protocol.motion || (protocol.drag && button.is_some());
            if !allowed {
                return None;
            }
            // Motion without a button reports "no button" (3).
            (button.and_then(Button::code).unwrap_or(3), false, true)
        }
        Event::Wheel(wheel) => (
            match wheel {
                Wheel::Up => 64,
                Wheel::Down => 65,
            },
            false,
            false,
        ),
    };

    if protocol.x10 && !protocol.clicks && !protocol.drag && !protocol.motion {
        // X10 reports presses of the three buttons and nothing else.
        if !matches!(event, Event::Press(_)) {
            return None;
        }
    } else {
        if mods.contains(Modifiers::SHIFT) {
            code |= 4;
        }
        if mods.contains(Modifiers::ALT) {
            code |= 8;
        }
        if mods.contains(Modifiers::CTRL) {
            code |= 16;
        }
    }
    if motion {
        code |= 32;
    }

    let (x, y) = (col + 1, row + 1);
    if protocol.sgr {
        let last = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{last}").into_bytes());
    }

    // The legacy encodings have one "release" code, whichever button it was.
    if release {
        code = (code & !3) | 3;
    }
    let mut out = vec![0x1b, b'[', b'M'];
    out.push(32 + code);
    if protocol.utf8 {
        push_utf8(&mut out, 32 + x)?;
        push_utf8(&mut out, 32 + y)?;
    } else {
        // One byte each: positions past 223 do not fit.
        out.push(u8::try_from(32 + x).ok()?);
        out.push(u8::try_from(32 + y).ok()?);
    }
    Some(out)
}

fn push_utf8(out: &mut Vec<u8>, value: usize) -> Option<()> {
    let ch = char::from_u32(u32::try_from(value).ok()?)?;
    let mut buffer = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
    Some(())
}

/// Counts consecutive clicks of one button on one cell.
#[derive(Default)]
pub struct Clicks {
    last: Option<(Instant, (usize, usize), Button)>,
    count: u8,
}

impl Clicks {
    /// Register a press and return how many clicks in a row this makes
    /// (1, 2, 3, then starting over).
    pub fn register(&mut self, now: Instant, cell: (usize, usize), button: Button) -> u8 {
        let repeated = self.last.is_some_and(|(at, last_cell, last_button)| {
            last_button == button && last_cell == cell && now.duration_since(at) <= MULTI_CLICK
        });
        self.count = if repeated { self.count % 3 + 1 } else { 1 };
        self.last = Some((now, cell, button));
        self.count
    }
}

/// A position within the terminal grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub col: usize,
    pub row: usize,
    /// The pointer is in the right half of the cell (selections start and
    /// end between cells, on the side nearest the pointer).
    pub right: bool,
}

/// The cell of the pixel (`x`, `y`) on a `cols` x `rows` grid, clamped; the
/// second result is `-1`/`0`/`1` for above, inside or below the grid.
pub fn locate(
    x: f32,
    y: f32,
    cell_width: f32,
    cell_height: f32,
    cols: usize,
    rows: usize,
) -> (Cell, i32) {
    let fx = (x / cell_width.max(1.0)).max(0.0);
    let fy = (y / cell_height.max(1.0)).max(0.0);
    let col = (fx as usize).min(cols.saturating_sub(1));
    let row = (fy as usize).min(rows.saturating_sub(1));
    let outside = if y < 0.0 {
        -1
    } else if rows > 0 && fy >= rows as f32 {
        1
    } else {
        0
    };
    let right = fx >= cols as f32 || fx.fract() >= 0.5;
    (Cell { col, row, right }, outside)
}
