//! Terminal emulation, behind a trait.
//!
//! The VT engine is the component most likely to be replaced during this project's life:
//! `libghostty-vt` has the strongest correctness record in the field and already builds for Windows,
//! Linux and Wasm behind a C API. [`TerminalEmulator`] is the seam that makes evaluating it a
//! contained experiment instead of a rewrite of the UI.
//!
//! The trait's output, [`GridSnapshot`], contains plain `char`s and resolved RGB. Colour
//! resolution — named colours, the 256-colour cube, OSC overrides, dim, inverse and hidden — happens
//! on this side of the boundary because it is terminal semantics. The renderer never sees a colour
//! reference it has to interpret.

mod emulator;
mod palette;
mod snapshot;

pub use emulator::AlacrittyEmulator;
pub use palette::{Palette, Rgb};
pub use snapshot::{CellFlags, CursorKind, CursorSnapshot, GridSnapshot, RenderCell};

/// A VT-compatible terminal emulator.
///
/// Implementations are driven from one thread: bytes in through [`advance`](Self::advance), state out
/// through [`snapshot`](Self::snapshot). Nothing here blocks or performs I/O — the caller owns the
/// transport and decides when to feed it.
pub trait TerminalEmulator: Send {
    /// Feed output received from the peer.
    ///
    /// Partial escape sequences are fine: the parser is a state machine and resumes across calls, so
    /// there is no need to buffer until a sequence looks complete.
    fn advance(&mut self, bytes: &[u8]);

    /// Change the grid size. A resize to the current size does nothing.
    fn resize(&mut self, cols: usize, rows: usize);

    /// Current `(cols, rows)`. Both are always at least 1.
    fn size(&self) -> (usize, usize);

    /// The visible content, ready to draw.
    ///
    /// Allocates a fresh grid on each call. That is deliberate for now — it keeps the boundary a
    /// plain value with no lifetimes, which is what makes the renderer testable. Damage-tracked
    /// incremental snapshots are a phase 1 optimisation, behind this same signature.
    fn snapshot(&self) -> GridSnapshot;

    /// Scroll the view by `lines`; positive moves back into history.
    fn scroll(&mut self, lines: i32);

    /// Return the view to the live edge of the output.
    fn scroll_to_bottom(&mut self);

    /// Bytes the terminal wants written back to the peer, oldest first.
    ///
    /// Device-attribute queries, cursor-position reports and colour queries all expect an answer.
    /// A caller that ignores this will eventually meet a program that waits forever for a reply.
    fn take_responses(&mut self) -> Vec<Vec<u8>>;

    /// Whether the bell rang since the last call.
    fn take_bell(&mut self) -> bool;

    /// The title the remote program set, if any.
    fn title(&self) -> Option<&str>;

    /// A counter that changes whenever the visible content might have changed.
    ///
    /// Cheaper to compare than a snapshot, so the UI uses it to decide whether to repaint.
    fn generation(&self) -> u64;
}
