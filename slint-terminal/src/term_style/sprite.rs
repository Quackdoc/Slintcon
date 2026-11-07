//! Pixel-exact box-drawing, block and braille characters.
//!
//! These are drawn as rectangles on the cell grid instead of taken from the
//! font, so lines join across cells whatever font is in use. Both renderers
//! already paint rectangles, so nothing here is renderer specific. Dashed
//! lines, rounded corners and diagonals (U+2504-250B, U+254C-254F,
//! U+256D-2573) are left to the font.

use super::row::{CellSize, PRect};

/// A cell whose character is drawn by [`push_rects`] instead of the font.
#[derive(Clone, Copy, Debug)]
pub struct SpriteCell {
    pub col: usize,
    pub ch: char,
    pub fg: [u8; 4],
    pub bg: [u8; 4],
}

/// Line style of each arm of a box-drawing character: 0 none, 1 light,
/// 2 heavy, 3 double.
type Arms = [u8; 4];
const LEFT: usize = 0;
const RIGHT: usize = 1;
const UP: usize = 2;
const DOWN: usize = 3;

/// `[left, right, up, down]` for the line and double-line characters.
fn arms(c: u32) -> Option<Arms> {
    let a: Arms = match c {
        0x2500 => [1, 1, 0, 0],
        0x2501 => [2, 2, 0, 0],
        0x2502 => [0, 0, 1, 1],
        0x2503 => [0, 0, 2, 2],
        0x250C => [0, 1, 0, 1],
        0x250D => [0, 2, 0, 1],
        0x250E => [0, 1, 0, 2],
        0x250F => [0, 2, 0, 2],
        0x2510 => [1, 0, 0, 1],
        0x2511 => [2, 0, 0, 1],
        0x2512 => [1, 0, 0, 2],
        0x2513 => [2, 0, 0, 2],
        0x2514 => [0, 1, 1, 0],
        0x2515 => [0, 2, 1, 0],
        0x2516 => [0, 1, 2, 0],
        0x2517 => [0, 2, 2, 0],
        0x2518 => [1, 0, 1, 0],
        0x2519 => [2, 0, 1, 0],
        0x251A => [1, 0, 2, 0],
        0x251B => [2, 0, 2, 0],
        0x251C => [0, 1, 1, 1],
        0x251D => [0, 2, 1, 1],
        0x251E => [0, 1, 2, 1],
        0x251F => [0, 1, 1, 2],
        0x2520 => [0, 1, 2, 2],
        0x2521 => [0, 2, 2, 1],
        0x2522 => [0, 2, 1, 2],
        0x2523 => [0, 2, 2, 2],
        0x2524 => [1, 0, 1, 1],
        0x2525 => [2, 0, 1, 1],
        0x2526 => [1, 0, 2, 1],
        0x2527 => [1, 0, 1, 2],
        0x2528 => [1, 0, 2, 2],
        0x2529 => [2, 0, 2, 1],
        0x252A => [2, 0, 1, 2],
        0x252B => [2, 0, 2, 2],
        0x252C => [1, 1, 0, 1],
        0x252D => [2, 1, 0, 1],
        0x252E => [1, 2, 0, 1],
        0x252F => [2, 2, 0, 1],
        0x2530 => [1, 1, 0, 2],
        0x2531 => [2, 1, 0, 2],
        0x2532 => [1, 2, 0, 2],
        0x2533 => [2, 2, 0, 2],
        0x2534 => [1, 1, 1, 0],
        0x2535 => [2, 1, 1, 0],
        0x2536 => [1, 2, 1, 0],
        0x2537 => [2, 2, 1, 0],
        0x2538 => [1, 1, 2, 0],
        0x2539 => [2, 1, 2, 0],
        0x253A => [1, 2, 2, 0],
        0x253B => [2, 2, 2, 0],
        0x253C => [1, 1, 1, 1],
        0x253D => [2, 1, 1, 1],
        0x253E => [1, 2, 1, 1],
        0x253F => [2, 2, 1, 1],
        0x2540 => [1, 1, 2, 1],
        0x2541 => [1, 1, 1, 2],
        0x2542 => [1, 1, 2, 2],
        0x2543 => [2, 1, 2, 1],
        0x2544 => [1, 2, 2, 1],
        0x2545 => [2, 1, 1, 2],
        0x2546 => [1, 2, 1, 2],
        0x2547 => [2, 2, 2, 1],
        0x2548 => [2, 2, 1, 2],
        0x2549 => [2, 1, 2, 2],
        0x254A => [1, 2, 2, 2],
        0x254B => [2, 2, 2, 2],
        0x2550 => [3, 3, 0, 0],
        0x2551 => [0, 0, 3, 3],
        0x2552 => [0, 3, 0, 1],
        0x2553 => [0, 1, 0, 3],
        0x2554 => [0, 3, 0, 3],
        0x2555 => [3, 0, 0, 1],
        0x2556 => [1, 0, 0, 3],
        0x2557 => [3, 0, 0, 3],
        0x2558 => [0, 3, 1, 0],
        0x2559 => [0, 1, 3, 0],
        0x255A => [0, 3, 3, 0],
        0x255B => [3, 0, 1, 0],
        0x255C => [1, 0, 3, 0],
        0x255D => [3, 0, 3, 0],
        0x255E => [0, 3, 1, 1],
        0x255F => [0, 1, 3, 3],
        0x2560 => [0, 3, 3, 3],
        0x2561 => [3, 0, 1, 1],
        0x2562 => [1, 0, 3, 3],
        0x2563 => [3, 0, 3, 3],
        0x2564 => [3, 3, 0, 1],
        0x2565 => [1, 1, 0, 3],
        0x2566 => [3, 3, 0, 3],
        0x2567 => [3, 3, 1, 0],
        0x2568 => [1, 1, 3, 0],
        0x2569 => [3, 3, 3, 0],
        0x256A => [3, 3, 1, 1],
        0x256B => [1, 1, 3, 3],
        0x256C => [3, 3, 3, 3],
        0x2574 => [1, 0, 0, 0],
        0x2575 => [0, 0, 1, 0],
        0x2576 => [0, 1, 0, 0],
        0x2577 => [0, 0, 0, 1],
        0x2578 => [2, 0, 0, 0],
        0x2579 => [0, 0, 2, 0],
        0x257A => [0, 2, 0, 0],
        0x257B => [0, 0, 0, 2],
        0x257C => [1, 2, 0, 0],
        0x257D => [0, 0, 1, 2],
        0x257E => [2, 1, 0, 0],
        0x257F => [0, 0, 2, 1],
        _ => return None,
    };
    Some(a)
}

/// Whether [`push_rects`] draws `ch`.
pub fn is_sprite(ch: char) -> bool {
    let c = ch as u32;
    match c {
        0x2500..=0x257F => arms(c).is_some(),
        0x2580..=0x259F | 0x2800..=0x28FF => true,
        _ => false,
    }
}

/// Appends the rectangles that draw `sprite` in a cell of size `cell` whose
/// top edge is at `row_y`.
pub fn push_rects(sprite: SpriteCell, cell: CellSize, row_y: f32, out: &mut Vec<PRect>) {
    let mut cell = Cell {
        x: sprite.col as f32 * cell.width,
        y: row_y,
        w: cell.width,
        h: cell.height,
        fg: sprite.fg,
        out,
    };
    let c = sprite.ch as u32;
    match c {
        0x2500..=0x257F => {
            if let Some(arms) = arms(c) {
                push_box(arms, &mut cell);
            }
        }
        0x2580..=0x259F => push_block(c, sprite.bg, &mut cell),
        0x2800..=0x28FF => push_braille(c - 0x2800, &mut cell),
        _ => {}
    }
}

/// A cell being drawn: its origin and size in pixels, the foreground color,
/// and where the rectangles go.
struct Cell<'a> {
    /// Left edge.
    x: f32,
    /// Top edge.
    y: f32,
    /// Width.
    w: f32,
    /// Height.
    h: f32,
    /// Foreground color.
    fg: [u8; 4],
    /// Receives the rectangles.
    out: &'a mut Vec<PRect>,
}

impl Cell<'_> {
    /// Fill the `w` x `h` rectangle at offset (`dx`, `dy`) from the cell's
    /// origin in the foreground color; empty rectangles are skipped.
    fn fill(&mut self, dx: f32, dy: f32, w: f32, h: f32) {
        self.fill_with(self.fg, dx, dy, w, h);
    }

    /// Like [`Cell::fill`], in `color`.
    fn fill_with(&mut self, color: [u8; 4], dx: f32, dy: f32, w: f32, h: f32) {
        if w > 0.0 && h > 0.0 {
            self.out.push(PRect {
                x: self.x + dx,
                y: self.y + dy,
                width: w,
                height: h,
                color,
            });
        }
    }
}

/// Where an arm's strokes stop on the way to the centre, along its own axis.
/// `band0..band1` is the span the perpendicular arms occupy. A stroke that
/// faces a double perpendicular arm stops at that arm's nearest stroke, which
/// keeps the inner corner of double lines open; any other stroke runs to the
/// far edge of the band so the corner is filled.
fn reach(lower: bool, facing: u8, band0: f32, band1: f32, t: f32) -> f32 {
    match (lower, facing == 3) {
        (true, true) => band0 + t,
        (true, false) => band1,
        (false, true) => band1 - t,
        (false, false) => band0,
    }
}

/// Appends the strokes of a box-drawing character with the given arm styles.
fn push_box(arms: Arms, cell: &mut Cell) {
    let (w, h) = (cell.w, cell.h);
    let t = (h / 14.0).round().max(1.0);

    // Thickness of a whole arm: light `t`, heavy `2t`, and a double line is
    // two strokes of `t` with a gap of `t`.
    let size = |level: u8| match level {
        0 => 0.0,
        1 => t,
        2 => 2.0 * t,
        _ => 3.0 * t,
    };
    // Strokes (offset, thickness) of one arm across a cell dimension of `len`.
    let strokes = |level: u8, len: f32| {
        let start = ((len - size(level)) / 2.0).floor();
        match level {
            0 => [None, None],
            3 => [Some((start, t)), Some((start + 2.0 * t, t))],
            _ => [Some((start, size(level))), None],
        }
    };
    // The span two opposite arms occupy across the middle of the cell.
    let band = |a: usize, b: usize, len: f32| {
        let span = size(arms[a]).max(size(arms[b]));
        let start = ((len - span) / 2.0).floor();
        (start, start + span)
    };
    let (band_x, band_y) = (band(UP, DOWN, w), band(LEFT, RIGHT, h));

    // Each arm, whether it starts at the cell's left or top edge, and its axis.
    let all = [
        (LEFT, true, true),
        (RIGHT, false, true),
        (UP, true, false),
        (DOWN, false, false),
    ];
    for (arm, from_start, horizontal) in all {
        // The cell size along and across the arm, the band the perpendicular
        // arms occupy along it, and those perpendicular arms.
        let (along, across, band, sides) = if horizontal {
            (w, h, band_x, [UP, DOWN])
        } else {
            (h, w, band_y, [LEFT, RIGHT])
        };
        for (i, (offset, thickness)) in strokes(arms[arm], across).into_iter().flatten().enumerate()
        {
            // The two strokes of a double arm each face one perpendicular arm.
            let facing = if arms[arm] == 3 { arms[sides[i]] } else { 0 };
            let edge = reach(from_start, facing, band.0, band.1, t);
            let (start, len) = if from_start {
                (0.0, edge)
            } else {
                (edge, along - edge)
            };
            if horizontal {
                cell.fill(start, offset, len, thickness);
            } else {
                cell.fill(offset, start, thickness, len);
            }
        }
    }
}

/// Appends the rectangles of a block element (U+2580-259F) into `cell`.
fn push_block(c: u32, bg: [u8; 4], cell: &mut Cell) {
    let (w, h) = (cell.w, cell.h);
    let top = (h / 2.0).round();
    let left = (w / 2.0).round();
    let eighths = |len: f32, n: u32| (len * n as f32 / 8.0).round().max(1.0);
    match c {
        0x2580 => cell.fill(0.0, 0.0, w, top),
        // Lower n/8 block; the half block tiles exactly with U+2580.
        0x2581..=0x2588 => {
            let n = c - 0x2580;
            let height = if n == 4 {
                h - top
            } else {
                eighths(h, n).min(h)
            };
            cell.fill(0.0, h - height, w, height);
        }
        // Left n/8 block, from 7/8 down to 1/8; the half block tiles with U+2590.
        0x2589..=0x258F => {
            let n = 8 - (c - 0x2588);
            let width = if n == 4 { left } else { eighths(w, n).min(w) };
            cell.fill(0.0, 0.0, width, h);
        }
        0x2590 => cell.fill(left, 0.0, w - left, h),
        // Shades: the foreground blended into the background.
        0x2591..=0x2593 => {
            let alpha = (c - 0x2590) as f32 / 4.0;
            let fg = cell.fg;
            let mut color = [255; 4];
            for i in 0..3 {
                color[i] = (bg[i] as f32 * (1.0 - alpha) + fg[i] as f32 * alpha).round() as u8;
            }
            cell.fill_with(color, 0.0, 0.0, w, h);
        }
        0x2594 => cell.fill(0.0, 0.0, w, eighths(h, 1)),
        0x2595 => {
            let width = eighths(w, 1);
            cell.fill(w - width, 0.0, width, h);
        }
        // Quadrants: bit 0 upper left, 1 upper right, 2 lower left, 3 lower right.
        _ => {
            let mask: u8 = match c {
                0x2596 => 0b0100,
                0x2597 => 0b1000,
                0x2598 => 0b0001,
                0x2599 => 0b1101,
                0x259A => 0b1001,
                0x259B => 0b0111,
                0x259C => 0b1011,
                0x259D => 0b0010,
                0x259E => 0b0110,
                _ => 0b1110,
            };
            for bit in 0..4 {
                if mask & (1 << bit) != 0 {
                    let (qx, qw) = if bit & 1 == 0 {
                        (0.0, left)
                    } else {
                        (left, w - left)
                    };
                    let (qy, qh) = if bit & 2 == 0 {
                        (0.0, top)
                    } else {
                        (top, h - top)
                    };
                    cell.fill(qx, qy, qw, qh);
                }
            }
        }
    }
}

/// Braille: two columns by four rows of square dots.
fn push_braille(bits: u32, cell: &mut Cell) {
    let slot_w = cell.w / 2.0;
    let slot_h = cell.h / 4.0;
    let side = (slot_w.min(slot_h) * 0.6).round().max(1.0);
    // Dot number (bit) for each (column, row).
    const BIT: [[u32; 4]; 2] = [[0, 1, 2, 6], [3, 4, 5, 7]];
    for (col, rows) in BIT.iter().enumerate() {
        for (row, &bit) in rows.iter().enumerate() {
            if bits & (1 << bit) != 0 {
                let dx = (col as f32 * slot_w + (slot_w - side) / 2.0).floor();
                let dy = (row as f32 * slot_h + (slot_h - side) / 2.0).floor();
                cell.fill(dx, dy, side, side);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rects(ch: char, w: f32, h: f32) -> Vec<PRect> {
        let mut out = Vec::new();
        let sprite = SpriteCell {
            col: 0,
            ch,
            fg: [255; 4],
            bg: [0, 0, 0, 255],
        };
        let cell = CellSize {
            width: w,
            height: h,
        };
        push_rects(sprite, cell, 0.0, &mut out);
        out
    }

    /// Which pixels of a `w`x`h` cell the rectangles cover.
    fn coverage(rects: &[PRect], w: usize, h: usize) -> Vec<bool> {
        let mut px = vec![false; w * h];
        for r in rects {
            for yy in r.y as usize..(r.y + r.height) as usize {
                for xx in r.x as usize..(r.x + r.width) as usize {
                    assert!(xx < w && yy < h, "rect outside the cell: {r:?}");
                    px[yy * w + xx] = true;
                }
            }
        }
        px
    }

    #[test]
    fn lines_reach_the_cell_edges() {
        for (w, h) in [(8, 16), (9, 18), (10, 21), (6, 12)] {
            let horizontal = coverage(&rects('─', w as f32, h as f32), w, h);
            let vertical = coverage(&rects('│', w as f32, h as f32), w, h);
            let row = (0..h).find(|&y| horizontal[y * w]).unwrap();
            assert!(
                (0..w).all(|x| horizontal[row * w + x]),
                "{w}x{h} ─ has a gap"
            );
            let col = (0..w).find(|&x| vertical[x]).unwrap();
            assert!((0..h).all(|y| vertical[y * w + col]), "{w}x{h} │ has a gap");
        }
    }

    #[test]
    fn neighbouring_cells_join() {
        // The right edge of ┌ and the left edge of ─ must be on the same rows.
        let (w, h) = (9, 18);
        let corner = coverage(&rects('┌', w as f32, h as f32), w, h);
        let line = coverage(&rects('─', w as f32, h as f32), w, h);
        for y in 0..h {
            assert_eq!(corner[y * w + w - 1], line[y * w], "row {y}");
        }
    }

    #[test]
    fn every_sprite_stays_inside_the_cell() {
        for (w, h) in [(8, 16), (9, 18), (5, 10), (3, 6)] {
            for c in 0x2500u32..=0x28FF {
                if let Some(ch) = char::from_u32(c).filter(|&ch| is_sprite(ch)) {
                    coverage(&rects(ch, w as f32, h as f32), w, h);
                }
            }
        }
    }

    #[test]
    fn block_halves_tile() {
        let (w, h) = (9, 19);
        let mut both = rects('\u{2580}', w as f32, h as f32);
        both.extend(rects('\u{2584}', w as f32, h as f32));
        assert!(coverage(&both, w, h).iter().all(|&p| p));
        let mut both = rects('\u{258C}', w as f32, h as f32);
        both.extend(rects('\u{2590}', w as f32, h as f32));
        assert!(coverage(&both, w, h).iter().all(|&p| p));
    }

    #[test]
    fn braille_dots() {
        assert!(rects('\u{2800}', 8.0, 16.0).is_empty());
        assert_eq!(rects('\u{28FF}', 8.0, 16.0).len(), 8);
        assert_eq!(rects('\u{2801}', 8.0, 16.0).len(), 1);
    }

    #[test]
    fn font_drawn_characters_are_not_sprites() {
        assert!(!is_sprite('\u{2504}'));
        assert!(!is_sprite('\u{256D}'));
        assert!(!is_sprite('\u{2571}'));
        assert!(!is_sprite('a'));
    }
}
