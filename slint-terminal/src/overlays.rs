//! Graphics overlay resolution: turns rio-vt's sixel/iTerm2/kitty placements
//! into screen-space quads for the renderers.

use rio_vt::ansi::graphics::{
    KITTY_PLACEHOLDER, KittyOverlayGeometry, OverlayViewport, atlas_overlay_geometry,
    kitty_overlay_geometry,
};
use rio_vt::ansi::kitty_virtual::{
    IncompletePlacement, compute_run_geometry, resolve_virtual_placement,
};
use rio_vt::crosswords::Crosswords;
use rio_vt::crosswords::grid::row::Row;
use rio_vt::crosswords::square::{ContentTag, Extras, Square};
use rio_vt::crosswords::style::Style;
use rio_vt::event::EventListener;
use rustc_hash::FxHashMap;

use crate::images::OverlayQuad;

fn quad(image_key: u64, geo: KittyOverlayGeometry, over_text: bool, z: i32) -> OverlayQuad {
    OverlayQuad {
        image_key,
        x: geo.x,
        y: geo.y,
        width: geo.width,
        height: geo.height,
        src: geo.source_rect,
        over_text,
        z,
    }
}

/// Resolve rio-vt's graphics state into screen-space overlay quads: sixel and
/// iTerm2 atlas placements, direct kitty placements, and kitty Unicode
/// placeholder runs.
pub fn collect<T: EventListener>(
    term: &Crosswords<T>,
    rows: &[Row<Square>],
    cell_styles: &[Vec<Style>],
    extras: &FxHashMap<u16, Extras>,
    cell_width: f32,
    cell_height: f32,
) -> Vec<OverlayQuad> {
    let mut overlays = Vec::new();

    let viewport = OverlayViewport {
        cell_width,
        cell_height,
        origin_x: 0.0,
        origin_y: 0.0,
        // Placements anchor in a row space that starts at the first evicted
        // row, so the viewport's history must include `lines_evicted`.
        history_size: term.lines_evicted() as i64 + term.history_size() as i64,
        display_offset: term.display_offset() as i64,
        screen_lines: term.screen_lines() as i64,
    };

    // Sixel and iTerm2 share the atlas image path and always render under text.
    for placement in &term.graphics.atlas_placements {
        if let Some(geo) = atlas_overlay_geometry(placement, &viewport) {
            overlays.push(quad(placement.image_key, geo, false, 0));
        }
    }

    // Direct kitty placements (kitty graphics protocol `a=p` / `i=`).
    for placement in term.graphics.kitty_placements.values() {
        let Some(stored) = term.graphics.kitty_images.get(&placement.image_id) else {
            continue;
        };
        let image_width = stored.data.width;
        let image_height = stored.data.height;
        if let Some(geo) = kitty_overlay_geometry(placement, image_width, image_height, &viewport) {
            let key = rio_graphics::kitty_image_key(placement.image_id);
            overlays.push(quad(key, geo, placement.z_index >= 0, placement.z_index));
        }
    }

    // Kitty Unicode placeholders: each contiguous run of U+10EEEE cells shows
    // one slice of a virtual placement.
    for (line, row) in rows.iter().enumerate() {
        if !row.kitty_virtual_placeholder {
            continue;
        }
        let styles = cell_styles.get(line);
        let flush = |overlays: &mut Vec<OverlayQuad>, run: &IncompletePlacement, start: usize| {
            overlays.extend(placeholder_quad(
                term,
                run,
                line,
                start,
                cell_width,
                cell_height,
            ));
        };
        let mut pending: Option<(IncompletePlacement, usize)> = None;
        for (col, square) in row.inner.iter().enumerate() {
            let is_placeholder =
                square.content_tag() == ContentTag::Codepoint && square.c() == KITTY_PLACEHOLDER;
            if !is_placeholder {
                if let Some((run, start)) = pending.take() {
                    flush(&mut overlays, &run, start);
                }
                continue;
            }

            let style = styles.and_then(|s| s.get(col)).copied().unwrap_or_default();
            let zerowidth: &[char] = square
                .extras_id()
                .and_then(|id| extras.get(&id))
                .map(|extras| extras.zerowidth.as_slice())
                .unwrap_or(&[]);
            let cell = IncompletePlacement::from_cell(style.fg, style.underline_color, zerowidth);

            match pending.as_mut() {
                Some((run, _)) if run.can_append(&cell) => run.append(),
                Some(_) => {
                    let (run, start) = pending.take().unwrap();
                    flush(&mut overlays, &run, start);
                    pending = Some((cell, col));
                }
                None => pending = Some((cell, col)),
            }
        }
        if let Some((run, start)) = pending.take() {
            flush(&mut overlays, &run, start);
        }
    }

    overlays
}

fn placeholder_quad<T: EventListener>(
    term: &Crosswords<T>,
    run: &IncompletePlacement,
    line: usize,
    start_col: usize,
    cell_width: f32,
    cell_height: f32,
) -> Option<OverlayQuad> {
    let placeholder = run.complete();
    let virtual_placement = resolve_virtual_placement(
        &term.graphics.kitty_virtual_placements,
        placeholder.image_id,
        placeholder.placement_id,
    )?;
    let stored = term.graphics.kitty_images.get(&placeholder.image_id)?;
    let image_width = stored.data.width as u32;
    let image_height = stored.data.height as u32;

    let geo = compute_run_geometry(
        &placeholder,
        virtual_placement.columns,
        virtual_placement.rows,
        image_width,
        image_height,
        (
            virtual_placement.x,
            virtual_placement.y,
            virtual_placement.width,
            virtual_placement.height,
        ),
        cell_width,
        cell_height,
        0.0,
        0.0,
        line,
        start_col,
    )?;
    let key = rio_graphics::kitty_image_key(placeholder.image_id);
    let geo = KittyOverlayGeometry {
        x: geo.x,
        y: geo.y,
        width: geo.width,
        height: geo.height,
        source_rect: geo.source_rect,
    };
    Some(quad(key, geo, true, 0))
}
