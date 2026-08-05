//! The renderer-facing view of terminal state.
//!
//! A snapshot contains plain `char`s and resolved RGB — no VT colour references, no emulator types.
//! That is what lets `term-render` be tested without a parser, and what lets the emulator behind
//! [`crate::TerminalEmulator`] be replaced without touching the renderer.

use crate::palette::Rgb;

bitflags::bitflags! {
    /// Per-cell rendering attributes.
    ///
    /// A narrowing of `alacritty_terminal`'s cell flags to what a renderer acts on. Attributes that
    /// the emulator resolves away — `INVERSE` and `HIDDEN` are folded into the colours before a
    /// snapshot is produced — are absent on purpose.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct CellFlags: u16 {
        /// Draw with a bold face.
        const BOLD              = 1 << 0;
        /// Draw with an italic face.
        const ITALIC            = 1 << 1;
        /// Single underline.
        const UNDERLINE         = 1 << 2;
        /// Double underline.
        const DOUBLE_UNDERLINE  = 1 << 3;
        /// Curly underline.
        const UNDERCURL         = 1 << 4;
        /// Dotted underline.
        const DOTTED_UNDERLINE  = 1 << 5;
        /// Dashed underline.
        const DASHED_UNDERLINE  = 1 << 6;
        /// Strike-through.
        const STRIKEOUT         = 1 << 7;
        /// The left half of a double-width character.
        const WIDE              = 1 << 8;
        /// The placeholder cell to the right of a [`CellFlags::WIDE`] cell. Renderers skip these.
        const WIDE_SPACER       = 1 << 9;
        /// Part of the current selection.
        const SELECTED          = 1 << 10;

        /// Any underline style at all.
        const ANY_UNDERLINE = Self::UNDERLINE.bits()
            | Self::DOUBLE_UNDERLINE.bits()
            | Self::UNDERCURL.bits()
            | Self::DOTTED_UNDERLINE.bits()
            | Self::DASHED_UNDERLINE.bits();
    }
}

/// One character cell, ready to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderCell {
    /// The character to draw. A space for an empty cell, never `'\0'`.
    pub ch: char,
    /// Resolved text colour.
    pub fg: Rgb,
    /// Resolved background colour.
    pub bg: Rgb,
    /// Rendering attributes.
    pub flags: CellFlags,
}

impl RenderCell {
    /// An empty cell in the given colours.
    pub const fn blank(fg: Rgb, bg: Rgb) -> Self {
        Self {
            ch: ' ',
            fg,
            bg,
            flags: CellFlags::empty(),
        }
    }

    /// Whether this cell needs a glyph drawn at all.
    ///
    /// Blank cells are the overwhelming majority in a typical screen, so the renderer's inner loop
    /// checks this before doing any text work.
    pub fn has_glyph(&self) -> bool {
        self.ch != ' ' && !self.flags.contains(CellFlags::WIDE_SPACER)
    }
}

/// Cursor appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorKind {
    /// Filled block.
    #[default]
    Block,
    /// Underscore.
    Underline,
    /// Vertical bar.
    Beam,
    /// Outlined block, used when the window is unfocused.
    HollowBlock,
    /// Not drawn.
    Hidden,
}

/// Where and how to draw the cursor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorSnapshot {
    /// Column, zero-based.
    pub col: usize,
    /// Row within the visible area, zero-based.
    pub row: usize,
    /// Appearance.
    pub kind: CursorKind,
    /// False when the cursor is hidden, or scrolled out of the visible area.
    pub visible: bool,
}

/// The visible terminal content at one instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridSnapshot {
    /// Columns.
    pub cols: usize,
    /// Visible rows.
    pub rows: usize,
    /// `cols * rows` cells in row-major order.
    pub cells: Vec<RenderCell>,
    /// The default text colour, after any OSC 10 override.
    ///
    /// Carried here so the renderer can draw the cursor and any leftover space below the last row
    /// without needing access to the palette.
    pub default_fg: Rgb,
    /// The default background colour, after any OSC 11 override.
    pub default_bg: Rgb,
    /// Cursor state.
    pub cursor: CursorSnapshot,
    /// How far back in the scrollback the view is scrolled. Zero means at the bottom.
    pub display_offset: usize,
    /// Lines available above the visible area.
    pub history_len: usize,
    /// Increments whenever the terminal has been advanced. Lets a renderer skip redundant work.
    pub generation: u64,
}

impl GridSnapshot {
    /// An all-blank snapshot, used before any output has arrived.
    pub fn blank(cols: usize, rows: usize, fg: Rgb, bg: Rgb) -> Self {
        Self {
            cols,
            rows,
            cells: vec![RenderCell::blank(fg, bg); cols * rows],
            default_fg: fg,
            default_bg: bg,
            cursor: CursorSnapshot::default(),
            display_offset: 0,
            history_len: 0,
            generation: 0,
        }
    }

    /// The cells of one row, or an empty slice if `row` is out of range.
    pub fn row(&self, row: usize) -> &[RenderCell] {
        if row >= self.rows || self.cols == 0 {
            return &[];
        }
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }

    /// One cell, or `None` if out of range.
    pub fn cell(&self, col: usize, row: usize) -> Option<&RenderCell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.cells.get(row * self.cols + col)
    }

    /// Whether the view is scrolled away from the live edge.
    pub fn is_scrolled_back(&self) -> bool {
        self.display_offset > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FG: Rgb = Rgb::new(0xD0, 0xD0, 0xD0);
    const BG: Rgb = Rgb::new(0x0C, 0x0C, 0x0C);

    #[test]
    fn blank_snapshot_has_the_right_shape() {
        let s = GridSnapshot::blank(80, 24, FG, BG);
        assert_eq!(s.cells.len(), 80 * 24);
        assert_eq!(s.row(0).len(), 80);
        assert_eq!(s.row(23).len(), 80);
        assert!(s.row(24).is_empty(), "out-of-range row must not panic");
        assert!(!s.is_scrolled_back());
    }

    #[test]
    fn cell_lookup_is_row_major() {
        let mut s = GridSnapshot::blank(4, 3, FG, BG);
        s.cells[2 * 4 + 1].ch = 'x';
        assert_eq!(s.cell(1, 2).map(|c| c.ch), Some('x'));
        assert_eq!(s.cell(0, 0).map(|c| c.ch), Some(' '));
        assert!(s.cell(4, 0).is_none());
        assert!(s.cell(0, 3).is_none());
    }

    #[test]
    fn zero_sized_grid_does_not_panic() {
        let s = GridSnapshot::blank(0, 0, FG, BG);
        assert!(s.row(0).is_empty());
        assert!(s.cell(0, 0).is_none());
    }

    #[test]
    fn blank_cells_need_no_glyph() {
        assert!(!RenderCell::blank(FG, BG).has_glyph());

        let mut c = RenderCell::blank(FG, BG);
        c.ch = 'A';
        assert!(c.has_glyph());

        // The right half of a wide character carries the same char but must not be drawn twice.
        c.flags |= CellFlags::WIDE_SPACER;
        assert!(!c.has_glyph());
    }

    #[test]
    fn any_underline_covers_every_style() {
        for style in [
            CellFlags::UNDERLINE,
            CellFlags::DOUBLE_UNDERLINE,
            CellFlags::UNDERCURL,
            CellFlags::DOTTED_UNDERLINE,
            CellFlags::DASHED_UNDERLINE,
        ] {
            assert!(CellFlags::ANY_UNDERLINE.contains(style));
        }
        assert!(!CellFlags::ANY_UNDERLINE.contains(CellFlags::BOLD));
    }
}
