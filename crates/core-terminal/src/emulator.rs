//! [`TerminalEmulator`] implemented over `alacritty_terminal`.
//!
//! This is the only module in the workspace that names an `alacritty_terminal` type. Everything the
//! rest of the application sees is [`GridSnapshot`] and friends, which is what makes swapping in
//! `libghostty-vt` later a contained experiment rather than a rewrite.

use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{
    Color as VteColor, CursorShape, NamedColor, Processor, Rgb as VteRgb,
};
use parking_lot::Mutex;

use crate::TerminalEmulator;
use crate::palette::{Palette, Rgb};
use crate::snapshot::{CellFlags, CursorKind, CursorSnapshot, GridSnapshot, RenderCell};

/// Named-colour slot numbers, from `alacritty_terminal::term::color`'s documented layout.
/// Reproduced as constants because matching on enum discriminants needs integers.
mod slot {
    /// First of the 16 ANSI colours.
    pub(super) const ANSI_FIRST: usize = 0;
    /// Last of the 16 ANSI colours.
    pub(super) const ANSI_LAST: usize = 15;
    /// Default foreground.
    pub(super) const FOREGROUND: usize = 256;
    /// Default background.
    pub(super) const BACKGROUND: usize = 257;
    /// Cursor colour.
    pub(super) const CURSOR: usize = 258;
    /// First of the eight dim colours.
    pub(super) const DIM_FIRST: usize = 259;
    /// Last of the eight dim colours.
    pub(super) const DIM_LAST: usize = 266;
    /// Bright foreground.
    pub(super) const BRIGHT_FOREGROUND: usize = 267;
    /// Dim foreground.
    pub(super) const DIM_FOREGROUND: usize = 268;
}

/// Collects [`Event`]s from the emulator for the owner to drain.
///
/// `EventListener::send_event` takes `&self`, so interior mutability is required. The lock is only
/// ever held for a push or a `mem::take`, both of which are uncontended in practice: the emulator is
/// driven from a single thread.
#[derive(Clone, Default)]
struct EventProxy(Arc<Mutex<Vec<Event>>>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        self.0.lock().push(event);
    }
}

/// Grid dimensions in the shape `alacritty_terminal` wants them.
#[derive(Clone, Copy)]
struct Dims {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        // Matches alacritty's own `TermSize` helper: the scrollback limit comes from `Config`, not
        // from here, so reporting the visible height is correct.
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// A terminal emulator backed by `alacritty_terminal`.
pub struct AlacrittyEmulator {
    term: Term<EventProxy>,
    processor: Processor,
    events: EventProxy,
    palette: Palette,
    title: Option<String>,
    bell: bool,
    responses: Vec<Vec<u8>>,
    clipboard_stores: Vec<String>,
    cell_size: (u16, u16),
    generation: u64,
}

impl AlacrittyEmulator {
    /// A terminal of `cols` × `rows` with `scrollback` lines of history.
    pub fn new(cols: usize, rows: usize, scrollback: usize, palette: Palette) -> Self {
        let dims = Dims {
            cols: cols.max(1),
            rows: rows.max(1),
        };
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let events = EventProxy::default();
        let term = Term::new(config, &dims, events.clone());

        Self {
            term,
            processor: Processor::new(),
            events,
            palette,
            title: None,
            bell: false,
            responses: Vec::new(),
            clipboard_stores: Vec::new(),
            cell_size: (0, 0),
            generation: 0,
        }
    }

    /// Replace the colour table. Takes effect on the next snapshot.
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    /// Tell the emulator how large one cell is in pixels.
    ///
    /// Only used to answer the "how big is the text area?" escape sequence; programs such as image
    /// viewers ask, and a wrong answer makes them draw at the wrong scale.
    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        self.cell_size = (width, height);
    }

    /// Text the remote program asked to be placed on the clipboard, oldest first.
    pub fn take_clipboard_stores(&mut self) -> Vec<String> {
        std::mem::take(&mut self.clipboard_stores)
    }

    fn drain_events(&mut self) {
        let drained: Vec<Event> = {
            let mut queue = self.events.0.lock();
            std::mem::take(&mut *queue)
        };

        for event in drained {
            match event {
                Event::Title(title) => self.title = Some(title),
                Event::ResetTitle => self.title = None,
                Event::Bell => self.bell = true,
                Event::PtyWrite(text) => self.responses.push(text.into_bytes()),
                Event::ClipboardStore(_, text) => self.clipboard_stores.push(text),
                Event::ClipboardLoad(_, format) => {
                    // Clipboard integration arrives with the UI in phase 1. Until then answer with
                    // an empty paste rather than staying silent: a program that issued OSC 52 and
                    // gets no reply waits indefinitely.
                    self.responses.push(format("").into_bytes());
                }
                Event::ColorRequest(index, format) => {
                    let rgb = report_color(&self.palette, self.term.colors(), index);
                    self.responses.push(format(rgb).into_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let size = WindowSize {
                        num_lines: self.term.screen_lines() as u16,
                        num_cols: self.term.columns() as u16,
                        cell_width: self.cell_size.0,
                        cell_height: self.cell_size.1,
                    };
                    self.responses.push(format(size).into_bytes());
                }
                // Nothing to do: repaint is driven by `generation`, and process exit is reported by
                // the transport, which is the layer that actually owns the child.
                Event::Wakeup
                | Event::MouseCursorDirty
                | Event::CursorBlinkingChange
                | Event::Exit
                | Event::ChildExit(_) => {}
            }
        }
    }
}

impl TerminalEmulator for AlacrittyEmulator {
    fn advance(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
        self.drain_events();
        self.generation = self.generation.wrapping_add(1);
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        let dims = Dims {
            cols: cols.max(1),
            rows: rows.max(1),
        };
        if dims.cols == self.term.columns() && dims.rows == self.term.screen_lines() {
            return;
        }
        self.term.resize(dims);
        self.generation = self.generation.wrapping_add(1);
    }

    fn size(&self) -> (usize, usize) {
        (self.term.columns(), self.term.screen_lines())
    }

    fn snapshot(&self) -> GridSnapshot {
        let cols = self.term.columns();
        let rows = self.term.screen_lines();
        let content = self.term.renderable_content();
        let offset = content.display_offset;
        let overrides = content.colors;

        let default_fg = resolve_named(&self.palette, overrides, NamedColor::Foreground);
        let default_bg = resolve_named(&self.palette, overrides, NamedColor::Background);
        let mut cells = vec![RenderCell::blank(default_fg, default_bg); cols * rows];

        for indexed in content.display_iter {
            // `display_iter` yields grid lines, where 0 is the top of the screen region and
            // negative values are scrollback. Shifting by the display offset maps them onto visible
            // rows.
            let line = indexed.point.line.0 + offset as i32;
            if line < 0 {
                continue;
            }
            let (row, col) = (line as usize, indexed.point.column.0);
            if row >= rows || col >= cols {
                continue;
            }

            let cell = indexed.cell;
            let dim = cell.flags.intersects(Flags::DIM);

            let mut fg = resolve(&self.palette, overrides, cell.fg, dim);
            let mut bg = resolve(&self.palette, overrides, cell.bg, false);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }

            cells[row * cols + col] = RenderCell {
                ch: cell.c,
                fg,
                bg,
                flags: translate_flags(cell.flags),
            };
        }

        let cursor_line = content.cursor.point.line.0 + offset as i32;
        let cursor_visible = content.cursor.shape != CursorShape::Hidden
            && cursor_line >= 0
            && (cursor_line as usize) < rows;

        GridSnapshot {
            cols,
            rows,
            cells,
            default_fg,
            default_bg,
            cursor: CursorSnapshot {
                col: content.cursor.point.column.0.min(cols.saturating_sub(1)),
                row: if cursor_visible { cursor_line as usize } else { 0 },
                kind: translate_cursor(content.cursor.shape),
                visible: cursor_visible,
            },
            display_offset: offset,
            history_len: self.term.history_size(),
            generation: self.generation,
        }
    }

    fn scroll(&mut self, lines: i32) {
        if lines == 0 {
            return;
        }
        self.term.scroll_display(Scroll::Delta(lines));
        self.drain_events();
        self.generation = self.generation.wrapping_add(1);
    }

    fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.drain_events();
        self.generation = self.generation.wrapping_add(1);
    }

    fn take_responses(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.responses)
    }

    fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell)
    }

    fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    fn generation(&self) -> u64 {
        self.generation
    }
}

fn translate_flags(flags: Flags) -> CellFlags {
    let mut out = CellFlags::empty();
    out.set(CellFlags::BOLD, flags.intersects(Flags::BOLD));
    out.set(CellFlags::ITALIC, flags.intersects(Flags::ITALIC));
    out.set(CellFlags::UNDERLINE, flags.contains(Flags::UNDERLINE));
    out.set(
        CellFlags::DOUBLE_UNDERLINE,
        flags.contains(Flags::DOUBLE_UNDERLINE),
    );
    out.set(CellFlags::UNDERCURL, flags.contains(Flags::UNDERCURL));
    out.set(
        CellFlags::DOTTED_UNDERLINE,
        flags.contains(Flags::DOTTED_UNDERLINE),
    );
    out.set(
        CellFlags::DASHED_UNDERLINE,
        flags.contains(Flags::DASHED_UNDERLINE),
    );
    out.set(CellFlags::STRIKEOUT, flags.contains(Flags::STRIKEOUT));
    out.set(CellFlags::WIDE, flags.contains(Flags::WIDE_CHAR));
    out.set(
        CellFlags::WIDE_SPACER,
        flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
    );
    out
}

fn translate_cursor(shape: CursorShape) -> CursorKind {
    match shape {
        CursorShape::Block => CursorKind::Block,
        CursorShape::Underline => CursorKind::Underline,
        CursorShape::Beam => CursorKind::Beam,
        CursorShape::HollowBlock => CursorKind::HollowBlock,
        CursorShape::Hidden => CursorKind::Hidden,
    }
}

fn convert(rgb: VteRgb) -> Rgb {
    Rgb::new(rgb.r, rgb.g, rgb.b)
}

/// Resolve a cell's colour reference to concrete RGB.
///
/// `dim` applies the SGR 2 attribute. It is deliberately *not* applied to backgrounds: dimming a
/// background is not what any terminal does, and doing so makes dim text on a coloured background
/// unreadable.
fn resolve(palette: &Palette, overrides: &Colors, color: VteColor, dim: bool) -> Rgb {
    match color {
        VteColor::Spec(rgb) => {
            let base = convert(rgb);
            if dim { base.dimmed() } else { base }
        }
        VteColor::Indexed(index) => {
            let index = usize::from(index);
            let base = overrides[index]
                .map(convert)
                .unwrap_or_else(|| palette.indexed(index));
            if dim { base.dimmed() } else { base }
        }
        VteColor::Named(named) => {
            let base = resolve_named(palette, overrides, named);
            // The Dim* slots are already dim; dimming them again would compound.
            let slot = named as usize;
            let already_dim = (slot::DIM_FIRST..=slot::DIM_LAST).contains(&slot)
                || slot == slot::DIM_FOREGROUND;
            if dim && !already_dim { base.dimmed() } else { base }
        }
    }
}

fn resolve_named(palette: &Palette, overrides: &Colors, named: NamedColor) -> Rgb {
    // An OSC 4 / OSC 10 / OSC 11 override from the remote program always wins.
    if let Some(rgb) = overrides[named] {
        return convert(rgb);
    }

    let slot = named as usize;
    match slot {
        slot::ANSI_FIRST..=slot::ANSI_LAST => palette.indexed(slot),
        slot::FOREGROUND => palette.foreground,
        slot::BACKGROUND => palette.background,
        slot::CURSOR => palette.cursor,
        slot::DIM_FIRST..=slot::DIM_LAST => palette.indexed(slot - slot::DIM_FIRST).dimmed(),
        // No configured bright foreground; falling back to the plain one matches alacritty.
        slot::BRIGHT_FOREGROUND => palette.foreground,
        slot::DIM_FOREGROUND => palette.foreground.dimmed(),
        _ => palette.foreground,
    }
}

/// The colour to report back for an OSC colour query.
fn report_color(palette: &Palette, overrides: &Colors, index: usize) -> VteRgb {
    let rgb = if let Some(rgb) = overrides[index] {
        convert(rgb)
    } else {
        match index {
            slot::FOREGROUND => palette.foreground,
            slot::BACKGROUND => palette.background,
            slot::CURSOR => palette.cursor,
            other => palette.indexed(other),
        }
    };
    VteRgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emulator() -> AlacrittyEmulator {
        AlacrittyEmulator::new(20, 5, 1000, Palette::xterm())
    }

    /// The text of a snapshot row, trailing blanks removed.
    fn row_text(snapshot: &GridSnapshot, row: usize) -> String {
        snapshot
            .row(row)
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn plain_text_lands_in_the_grid() {
        let mut term = emulator();
        term.advance(b"hello");
        let snap = term.snapshot();
        assert_eq!(snap.cols, 20);
        assert_eq!(snap.rows, 5);
        assert_eq!(row_text(&snap, 0), "hello");
        assert!(snap.cursor.visible);
        assert_eq!((snap.cursor.row, snap.cursor.col), (0, 5));
    }

    #[test]
    fn newline_and_carriage_return_move_the_cursor() {
        let mut term = emulator();
        term.advance(b"one\r\ntwo");
        let snap = term.snapshot();
        assert_eq!(row_text(&snap, 0), "one");
        assert_eq!(row_text(&snap, 1), "two");
        assert_eq!(snap.cursor.row, 1);
    }

    #[test]
    fn generation_advances_only_on_change() {
        let mut term = emulator();
        let g0 = term.generation();
        term.advance(b"x");
        assert_ne!(term.generation(), g0);

        let g1 = term.generation();
        // A resize to the current size must not invalidate anyone's cached frame.
        term.resize(20, 5);
        assert_eq!(term.generation(), g1);
        term.resize(30, 8);
        assert_ne!(term.generation(), g1);
        assert_eq!(term.size(), (30, 8));
    }

    #[test]
    fn sgr_sets_foreground_from_the_palette() {
        let mut term = emulator();
        // SGR 31 = red foreground.
        term.advance(b"\x1b[31mR");
        let snap = term.snapshot();
        let cell = snap.cell(0, 0).expect("cell 0,0");
        assert_eq!(cell.ch, 'R');
        assert_eq!(cell.fg, Palette::xterm().indexed(1));
    }

    #[test]
    fn truecolor_sgr_is_honoured_verbatim() {
        let mut term = emulator();
        term.advance(b"\x1b[38;2;10;20;30mT");
        let cell = *term.snapshot().cell(0, 0).expect("cell 0,0");
        assert_eq!(cell.fg, Rgb::new(10, 20, 30));
    }

    #[test]
    fn inverse_swaps_foreground_and_background() {
        let palette = Palette::xterm();
        let mut term = emulator();
        term.advance(b"\x1b[7mI");
        let cell = *term.snapshot().cell(0, 0).expect("cell 0,0");
        assert_eq!(cell.fg, palette.background);
        assert_eq!(cell.bg, palette.foreground);
    }

    #[test]
    fn dim_darkens_the_foreground_but_not_the_background() {
        let palette = Palette::xterm();
        let mut term = emulator();
        term.advance(b"\x1b[2mD");
        let cell = *term.snapshot().cell(0, 0).expect("cell 0,0");
        assert_eq!(cell.bg, palette.background, "background must not be dimmed");
        assert_ne!(cell.fg, palette.foreground);
    }

    #[test]
    fn bold_and_underline_reach_the_snapshot() {
        let mut term = emulator();
        term.advance(b"\x1b[1;4mB");
        let cell = *term.snapshot().cell(0, 0).expect("cell 0,0");
        assert!(cell.flags.contains(CellFlags::BOLD));
        assert!(cell.flags.contains(CellFlags::UNDERLINE));
        assert!(cell.flags.intersects(CellFlags::ANY_UNDERLINE));
    }

    #[test]
    fn wide_characters_mark_their_spacer() {
        let mut term = emulator();
        // A CJK ideograph occupies two columns.
        term.advance("漢".as_bytes());
        let snap = term.snapshot();
        let lead = snap.cell(0, 0).expect("lead cell");
        let spacer = snap.cell(1, 0).expect("spacer cell");
        assert_eq!(lead.ch, '漢');
        assert!(lead.flags.contains(CellFlags::WIDE));
        assert!(spacer.flags.contains(CellFlags::WIDE_SPACER));
        assert!(!spacer.has_glyph(), "the spacer must not be drawn");
    }

    #[test]
    fn osc_sets_and_resets_the_title() {
        let mut term = emulator();
        assert_eq!(term.title(), None);
        term.advance(b"\x1b]0;my title\x07");
        assert_eq!(term.title(), Some("my title"));
    }

    #[test]
    fn bell_is_reported_once() {
        let mut term = emulator();
        term.advance(b"\x07");
        assert!(term.take_bell());
        assert!(!term.take_bell(), "bell must not latch");
    }

    #[test]
    fn device_attributes_query_produces_a_response() {
        let mut term = emulator();
        term.advance(b"\x1b[c");
        let responses = term.take_responses();
        assert!(
            !responses.is_empty(),
            "a DA query must be answered or remote programs hang"
        );
        assert!(responses.concat().starts_with(b"\x1b["));
        assert!(term.take_responses().is_empty(), "responses must drain");
    }

    #[test]
    fn scrollback_is_reachable_and_snapshot_reports_the_offset() {
        let mut term = emulator();
        for i in 0..20 {
            term.advance(format!("line{i}\r\n").as_bytes());
        }
        assert!(term.snapshot().history_len > 0);

        term.scroll(3);
        let scrolled = term.snapshot();
        assert_eq!(scrolled.display_offset, 3);
        assert!(scrolled.is_scrolled_back());

        term.scroll_to_bottom();
        assert_eq!(term.snapshot().display_offset, 0);
    }

    #[test]
    fn resize_preserves_the_visible_text() {
        let mut term = emulator();
        term.advance(b"keepme");
        term.resize(40, 10);
        let snap = term.snapshot();
        assert_eq!(snap.cols, 40);
        assert_eq!(row_text(&snap, 0), "keepme");
    }

    #[test]
    fn zero_dimensions_are_clamped_rather_than_panicking() {
        let mut term = AlacrittyEmulator::new(0, 0, 100, Palette::xterm());
        assert_eq!(term.size(), (1, 1));
        term.resize(0, 0);
        assert_eq!(term.size(), (1, 1));
        term.advance(b"x");
        assert_eq!(term.snapshot().cells.len(), 1);
    }
}
