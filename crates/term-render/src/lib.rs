//! Drawing the terminal grid.
//!
//! # Phase 0 implementation note
//!
//! This paints through `egui`'s text layout. That is the *correct* thing to build first — it makes
//! the grid visible and the whole pipeline testable — but it is not the shipping renderer. Phase 1
//! replaces the body of [`paint`] with a GPU path: `swash` rasterisation into an `etagere` glyph
//! atlas, uploaded once and drawn with `wgpu` through an `egui` paint callback, with damage tracking
//! so an idle terminal costs nothing.
//!
//! The crate boundary and the signatures here are already the ones that path needs. What changes is
//! what happens inside, not who calls it.
//!
//! Two consequences of the current approach, recorded so they are not mistaken for design:
//!
//! * Bold and italic are carried in [`runs::TextRun::flags`] but not yet rendered as different
//!   faces — `egui`'s bundled monospace font has no bold or italic variant. Underlines and
//!   strike-through *are* drawn, as lines.
//! * Every visible cell is laid out every frame. Acceptable at 80×24, visibly not so at 200×50.

pub mod keys;
pub mod runs;

use bestterm_core_terminal::{CellFlags, CursorKind, GridSnapshot, Rgb};
use egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2, pos2, vec2,
};

use crate::runs::{build_bg_runs, build_text_runs};

/// Terminals are square-cornered.
const SQUARE: CornerRadius = CornerRadius::ZERO;

/// Thickness of underlines, strike-through, and the beam and underline cursors.
const LINE_THICKNESS: f32 = 1.5;

/// How the terminal grid is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalStyle {
    /// The monospace font and size.
    pub font: FontId,
    /// Multiplier on the font's natural row height.
    ///
    /// 1.0 is the font's own metrics. Terminal users routinely want a little more air than that,
    /// which is why it is a knob rather than a constant.
    pub line_height_factor: f32,
}

impl Default for TerminalStyle {
    fn default() -> Self {
        Self {
            font: FontId::monospace(14.0),
            line_height_factor: 1.0,
        }
    }
}

/// The pixel size of one character cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalMetrics {
    /// Width of one cell.
    pub cell_width: f32,
    /// Height of one cell, including any extra line spacing.
    pub cell_height: f32,
}

impl TerminalMetrics {
    /// Measure a style against the loaded fonts.
    ///
    /// Uses `fonts_mut` because glyph measurement populates the font atlas cache, so it genuinely
    /// needs mutable access — `Context::fonts` will not do.
    pub fn measure(ctx: &egui::Context, style: &TerminalStyle) -> Self {
        let (width, height) = ctx.fonts_mut(|fonts| {
            // 'M' is the conventional reference glyph. In a monospace font every advance is equal,
            // so any glyph would do; 'M' makes the intent obvious.
            (
                fonts.glyph_width(&style.font, 'M'),
                fonts.row_height(&style.font),
            )
        });

        Self {
            // A zero cell size would divide by zero in `grid_for`. Fonts should never report zero,
            // but a missing font file is exactly the kind of thing that does.
            cell_width: width.max(1.0),
            cell_height: (height * style.line_height_factor).max(1.0),
        }
    }

    /// The largest grid that fits in `available`, at least 1×1.
    pub fn grid_for(&self, available: Vec2) -> (usize, usize) {
        let cols = (available.x / self.cell_width).floor().max(1.0) as usize;
        let rows = (available.y / self.cell_height).floor().max(1.0) as usize;
        (cols, rows)
    }

    /// The pixel size a grid of `cols` × `rows` occupies.
    pub fn size_for(&self, cols: usize, rows: usize) -> Vec2 {
        vec2(
            cols as f32 * self.cell_width,
            rows as f32 * self.cell_height,
        )
    }

    /// Which cell contains `pos`, or `None` if it falls outside `area`.
    ///
    /// Used for click-to-position and selection; the coordinates are clamped rather than wrapped so
    /// a drag that leaves the widget still selects to the edge.
    pub fn cell_at(&self, area: Rect, pos: Pos2, cols: usize, rows: usize) -> Option<(usize, usize)> {
        if !area.contains(pos) {
            return None;
        }
        let col = ((pos.x - area.left()) / self.cell_width).floor() as usize;
        let row = ((pos.y - area.top()) / self.cell_height).floor() as usize;
        Some((col.min(cols.saturating_sub(1)), row.min(rows.saturating_sub(1))))
    }
}

/// Paint `snapshot` into `area`.
///
/// `focused` selects the filled block cursor over the hollow one, which is how a terminal shows
/// which pane has the keyboard.
pub fn paint(
    painter: &Painter,
    area: Rect,
    snapshot: &GridSnapshot,
    metrics: &TerminalMetrics,
    style: &TerminalStyle,
    focused: bool,
) {
    // Clip so a grid larger than its pane cannot draw over the surrounding chrome.
    let painter = painter.with_clip_rect(area);

    painter.rect_filled(area, SQUARE, color(snapshot.default_bg));

    let (cw, ch) = (metrics.cell_width, metrics.cell_height);

    for row in 0..snapshot.rows {
        let cells = snapshot.row(row);
        if cells.is_empty() {
            continue;
        }
        let top = area.top() + row as f32 * ch;

        // Backgrounds first, so text is never painted over.
        for run in build_bg_runs(cells, snapshot.default_bg) {
            let rect = Rect::from_min_size(
                pos2(area.left() + run.col as f32 * cw, top),
                vec2(run.len as f32 * cw, ch),
            );
            painter.rect_filled(rect, SQUARE, color(run.bg));
        }

        for run in build_text_runs(cells) {
            let left = area.left() + run.col as f32 * cw;
            let fg = color(run.fg);

            painter.text(
                pos2(left, top),
                Align2::LEFT_TOP,
                &run.text,
                style.font.clone(),
                fg,
            );

            let width = run.width as f32 * cw;
            if run.flags.intersects(CellFlags::ANY_UNDERLINE) {
                // All underline styles are drawn as a single line for now; distinguishing curly from
                // dashed needs the glyph-atlas renderer.
                let y = top + ch - LINE_THICKNESS;
                painter.hline(left..=left + width, y, Stroke::new(LINE_THICKNESS, fg));
            }
            if run.flags.contains(CellFlags::STRIKEOUT) {
                let y = top + ch * 0.5;
                painter.hline(left..=left + width, y, Stroke::new(LINE_THICKNESS, fg));
            }
        }
    }

    paint_cursor(&painter, area, snapshot, metrics, style, focused);
}

fn paint_cursor(
    painter: &Painter,
    area: Rect,
    snapshot: &GridSnapshot,
    metrics: &TerminalMetrics,
    style: &TerminalStyle,
    focused: bool,
) {
    let cursor = snapshot.cursor;
    if !cursor.visible || cursor.kind == CursorKind::Hidden {
        return;
    }

    let (cw, ch) = (metrics.cell_width, metrics.cell_height);
    let origin = pos2(
        area.left() + cursor.col as f32 * cw,
        area.top() + cursor.row as f32 * ch,
    );
    let cell = Rect::from_min_size(origin, vec2(cw, ch));
    let ink = color(snapshot.default_fg);

    // An unfocused pane shows a hollow cursor whatever shape it asked for: that is the convention,
    // and it is the only cue that typing will go somewhere else.
    let kind = if focused {
        cursor.kind
    } else {
        CursorKind::HollowBlock
    };

    match kind {
        CursorKind::Block => {
            painter.rect_filled(cell, SQUARE, ink);
            // Redraw the covered character in the background colour so it stays legible.
            if let Some(under) = snapshot.cell(cursor.col, cursor.row) {
                if under.has_glyph() {
                    painter.text(
                        origin,
                        Align2::LEFT_TOP,
                        under.ch,
                        style.font.clone(),
                        color(under.bg),
                    );
                }
            }
        }
        CursorKind::HollowBlock => {
            painter.rect_stroke(cell, SQUARE, Stroke::new(1.0, ink), StrokeKind::Inside);
        }
        CursorKind::Underline => {
            let rect = Rect::from_min_size(
                pos2(origin.x, origin.y + ch - LINE_THICKNESS),
                vec2(cw, LINE_THICKNESS),
            );
            painter.rect_filled(rect, SQUARE, ink);
        }
        CursorKind::Beam => {
            let rect = Rect::from_min_size(origin, vec2(LINE_THICKNESS, ch));
            painter.rect_filled(rect, SQUARE, ink);
        }
        CursorKind::Hidden => {}
    }
}

/// Convert a terminal colour to an `egui` one.
pub fn color(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> TerminalMetrics {
        TerminalMetrics {
            cell_width: 8.0,
            cell_height: 16.0,
        }
    }

    #[test]
    fn grid_for_floors_to_whole_cells() {
        let m = metrics();
        assert_eq!(m.grid_for(vec2(800.0, 320.0)), (100, 20));
        // A partial cell does not count.
        assert_eq!(m.grid_for(vec2(807.0, 335.0)), (100, 20));
    }

    #[test]
    fn grid_for_never_returns_zero() {
        let m = metrics();
        assert_eq!(m.grid_for(vec2(0.0, 0.0)), (1, 1));
        assert_eq!(m.grid_for(vec2(-50.0, -50.0)), (1, 1));
        assert_eq!(m.grid_for(vec2(3.0, 3.0)), (1, 1));
    }

    #[test]
    fn size_for_is_the_inverse_of_grid_for() {
        let m = metrics();
        let size = m.size_for(80, 24);
        assert_eq!(size, vec2(640.0, 384.0));
        assert_eq!(m.grid_for(size), (80, 24));
    }

    #[test]
    fn cell_at_maps_positions_to_cells() {
        let m = metrics();
        let area = Rect::from_min_size(pos2(10.0, 20.0), vec2(640.0, 384.0));
        assert_eq!(m.cell_at(area, pos2(10.0, 20.0), 80, 24), Some((0, 0)));
        assert_eq!(m.cell_at(area, pos2(10.0 + 8.5, 20.0 + 17.0), 80, 24), Some((1, 1)));
        assert_eq!(m.cell_at(area, pos2(0.0, 0.0), 80, 24), None);
    }

    #[test]
    fn cell_at_clamps_to_the_grid() {
        let m = metrics();
        // An area wider than the grid it holds: the far right still maps to the last column.
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 1000.0));
        assert_eq!(m.cell_at(area, pos2(999.0, 999.0), 80, 24), Some((79, 23)));
    }

    #[test]
    fn cell_at_on_an_empty_grid_does_not_underflow() {
        let m = metrics();
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        assert_eq!(m.cell_at(area, pos2(50.0, 50.0), 0, 0), Some((0, 0)));
    }

    #[test]
    fn colour_conversion_is_channel_preserving() {
        assert_eq!(color(Rgb::new(1, 2, 3)), Color32::from_rgb(1, 2, 3));
    }
}
