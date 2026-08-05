//! Grouping a row of cells into drawable runs.
//!
//! Drawing a terminal cell by cell is correct and unusably slow: an 80×50 grid is four thousand draw
//! calls per frame before anything interesting happens. Grouping adjacent cells that share a style
//! into one run brings a typical screen down to a few dozen.
//!
//! This module is pure — no `egui`, no GPU — which is what makes the grouping rules testable. The
//! rules are where the bugs live; the drawing that consumes them is mechanical.

use bestterm_core_terminal::{CellFlags, RenderCell, Rgb};

/// Attributes that change how a glyph is drawn, and therefore split a text run.
///
/// `WIDE`, `WIDE_SPACER` and `SELECTED` are excluded: the first two affect *layout* and are handled
/// by the run builder directly, and selection is drawn as a background, not a text style.
pub const TEXT_STYLE_MASK: CellFlags = CellFlags::BOLD
    .union(CellFlags::ITALIC)
    .union(CellFlags::STRIKEOUT)
    .union(CellFlags::ANY_UNDERLINE);

/// A horizontal span of identically-coloured background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BgRun {
    /// First column of the span.
    pub col: usize,
    /// Number of columns it covers.
    pub len: usize,
    /// The colour to fill.
    pub bg: Rgb,
}

/// A horizontal span of text sharing one colour and style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRun {
    /// Column the run starts at.
    pub col: usize,
    /// The characters, in order.
    pub text: String,
    /// Text colour.
    pub fg: Rgb,
    /// Style attributes, already masked by [`TEXT_STYLE_MASK`].
    pub flags: CellFlags,
    /// Columns the run occupies, which exceeds `text.chars().count()` when it holds a wide glyph.
    pub width: usize,
}

/// Group a row into background spans, skipping anything already the default colour.
///
/// Skipping the default is what makes this worth doing: on a typical screen almost every cell has
/// the default background, and the window is already cleared to it.
pub fn build_bg_runs(cells: &[RenderCell], default_bg: Rgb) -> Vec<BgRun> {
    let mut runs = Vec::new();
    let mut start = 0usize;

    while start < cells.len() {
        let bg = cells[start].bg;
        let mut end = start + 1;
        while end < cells.len() && cells[end].bg == bg {
            end += 1;
        }
        if bg != default_bg {
            runs.push(BgRun {
                col: start,
                len: end - start,
                bg,
            });
        }
        start = end;
    }

    runs
}

/// Group a row into text runs.
///
/// Runs break on a change of colour or style, on a blank cell, and around double-width glyphs. Wide
/// glyphs stand alone because a run's on-screen width is otherwise assumed to be one cell per
/// character — mixing them in makes everything after the glyph drift by a column.
pub fn build_text_runs(cells: &[RenderCell]) -> Vec<TextRun> {
    let mut runs: Vec<TextRun> = Vec::new();
    let mut current: Option<TextRun> = None;
    // The column the next character must sit at to continue the current run.
    let mut expected_col = 0usize;

    for (col, cell) in cells.iter().enumerate() {
        // The right half of a wide glyph carries no glyph of its own.
        if cell.flags.contains(CellFlags::WIDE_SPACER) {
            continue;
        }

        if !cell.has_glyph() {
            if let Some(run) = current.take() {
                runs.push(run);
            }
            continue;
        }

        let flags = cell.flags & TEXT_STYLE_MASK;
        let wide = cell.flags.contains(CellFlags::WIDE);

        let continues = current
            .as_ref()
            .is_some_and(|run| run.fg == cell.fg && run.flags == flags && col == expected_col);

        if !continues {
            if let Some(run) = current.take() {
                runs.push(run);
            }
            current = Some(TextRun {
                col,
                text: String::new(),
                fg: cell.fg,
                flags,
                width: 0,
            });
        }

        let advance = if wide { 2 } else { 1 };
        if let Some(run) = current.as_mut() {
            run.text.push(cell.ch);
            run.width += advance;
        }
        expected_col = col + advance;

        // A wide glyph terminates its run: see the note above about column drift.
        if wide {
            if let Some(run) = current.take() {
                runs.push(run);
            }
        }
    }

    if let Some(run) = current.take() {
        runs.push(run);
    }

    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    const FG: Rgb = Rgb::new(0xD0, 0xD0, 0xD0);
    const BG: Rgb = Rgb::new(0x0C, 0x0C, 0x0C);
    const RED: Rgb = Rgb::new(0xFF, 0x00, 0x00);
    const BLUE: Rgb = Rgb::new(0x00, 0x00, 0xFF);

    fn row(text: &str) -> Vec<RenderCell> {
        text.chars()
            .map(|ch| RenderCell {
                ch,
                fg: FG,
                bg: BG,
                flags: CellFlags::empty(),
            })
            .collect()
    }

    #[test]
    fn plain_text_is_one_run() {
        let runs = build_text_runs(&row("hello"));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].col, 0);
        assert_eq!(runs[0].text, "hello");
        assert_eq!(runs[0].width, 5);
    }

    #[test]
    fn blanks_split_runs_and_are_not_drawn() {
        let runs = build_text_runs(&row("ab  cd"));
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].col, runs[0].text.as_str()), (0, "ab"));
        assert_eq!((runs[1].col, runs[1].text.as_str()), (4, "cd"));
    }

    #[test]
    fn a_colour_change_splits_a_run() {
        let mut cells = row("abcd");
        cells[2].fg = RED;
        cells[3].fg = RED;
        let runs = build_text_runs(&cells);
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].text.as_str(), runs[0].fg), ("ab", FG));
        assert_eq!((runs[1].text.as_str(), runs[1].fg), ("cd", RED));
        assert_eq!(runs[1].col, 2);
    }

    #[test]
    fn a_style_change_splits_a_run() {
        let mut cells = row("abcd");
        cells[2].flags |= CellFlags::BOLD;
        cells[3].flags |= CellFlags::BOLD;
        let runs = build_text_runs(&cells);
        assert_eq!(runs.len(), 2);
        assert!(!runs[0].flags.contains(CellFlags::BOLD));
        assert!(runs[1].flags.contains(CellFlags::BOLD));
    }

    #[test]
    fn selection_alone_does_not_split_a_text_run() {
        // Selection is drawn as a background, so it must not fragment text.
        let mut cells = row("abcd");
        cells[1].flags |= CellFlags::SELECTED;
        cells[2].flags |= CellFlags::SELECTED;
        let runs = build_text_runs(&cells);
        assert_eq!(runs.len(), 1, "got {runs:?}");
        assert_eq!(runs[0].text, "abcd");
    }

    #[test]
    fn wide_glyphs_stand_alone_and_advance_two_columns() {
        let mut cells = row("a漢 b");
        cells[1].ch = '漢';
        cells[1].flags |= CellFlags::WIDE;
        cells[2].ch = ' ';
        cells[2].flags |= CellFlags::WIDE_SPACER;

        let runs = build_text_runs(&cells);
        assert_eq!(runs.len(), 3, "got {runs:?}");
        assert_eq!((runs[0].col, runs[0].text.as_str()), (0, "a"));
        assert_eq!(
            (runs[1].col, runs[1].text.as_str(), runs[1].width),
            (1, "漢", 2)
        );
        assert_eq!((runs[2].col, runs[2].text.as_str()), (3, "b"));
    }

    #[test]
    fn background_runs_skip_the_default_colour() {
        let mut cells = row("abcdef");
        cells[2].bg = RED;
        cells[3].bg = RED;
        let runs = build_bg_runs(&cells, BG);
        assert_eq!(
            runs,
            vec![BgRun {
                col: 2,
                len: 2,
                bg: RED
            }]
        );
    }

    #[test]
    fn adjacent_different_backgrounds_are_separate_runs() {
        let mut cells = row("abcd");
        cells[1].bg = RED;
        cells[2].bg = BLUE;
        let runs = build_bg_runs(&cells, BG);
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[0],
            BgRun {
                col: 1,
                len: 1,
                bg: RED
            }
        );
        assert_eq!(
            runs[1],
            BgRun {
                col: 2,
                len: 1,
                bg: BLUE
            }
        );
    }

    #[test]
    fn a_fully_coloured_row_is_one_background_run() {
        let mut cells = row("abcd");
        for cell in &mut cells {
            cell.bg = RED;
        }
        let runs = build_bg_runs(&cells, BG);
        assert_eq!(
            runs,
            vec![BgRun {
                col: 0,
                len: 4,
                bg: RED
            }]
        );
    }

    #[test]
    fn empty_row_produces_nothing() {
        assert!(build_text_runs(&[]).is_empty());
        assert!(build_bg_runs(&[], BG).is_empty());
    }

    #[test]
    fn all_blank_row_produces_no_text_runs() {
        assert!(build_text_runs(&row("    ")).is_empty());
    }
}
