//! What a tab holds.
//!
//! Two things, so far: a terminal over a byte stream, and a desktop over a frame stream. The plan
//! called for this from the start — "a `Pane` knows how to hold either of the two from day one" — and
//! until now there was only ever one of them, so the enum did not exist and every caller said
//! `TerminalTab`.
//!
//! The point of it is not the two variants. It is that everything above this — the tab bar, the
//! status bar, closing a tab, choosing which is active — asks a `Pane` a question and does not
//! branch. The moment a caller starts matching on the variant to decide what to draw, this has
//! stopped earning its place; the one place that legitimately does is the central panel, where a grid
//! of glyphs and a texture genuinely are different work.

use crate::surface_tab::SurfaceTab;
use crate::tab::TerminalTab;
use crate::tunnels::ConnectionId;

/// A tab's contents.
///
/// `TerminalTab` has no `Debug` — it owns a transport and an emulator, neither of which prints
/// usefully — so neither does this.
pub(crate) enum Pane {
    /// A terminal: a shell, an SSH session, later a serial line.
    Terminal(Box<TerminalTab>),
    /// A remote desktop.
    Surface(Box<SurfaceTab>),
}

impl Pane {
    /// What goes on the tab.
    pub(crate) fn title(&self) -> String {
        match self {
            Self::Terminal(tab) => tab.title(),
            Self::Surface(tab) => tab.title().to_string(),
        }
    }

    /// What the program inside announced, where anything did.
    ///
    /// A desktop has no equivalent: the remote end has many windows and no single title, and putting
    /// one of them on the tab would be arbitrary.
    pub(crate) fn program_title(&self) -> Option<String> {
        match self {
            Self::Terminal(tab) => tab.program_title(),
            Self::Surface(_) => None,
        }
    }

    /// The protocol, for the tab's icon.
    pub(crate) fn protocol(&self) -> String {
        match self {
            Self::Terminal(tab) => tab.protocol().to_string(),
            Self::Surface(tab) => tab.kind().id().to_string(),
        }
    }

    /// One line for the status bar.
    pub(crate) fn status_line(&self) -> String {
        match self {
            Self::Terminal(tab) => tab.status_line(),
            Self::Surface(tab) => tab.status_line(),
        }
    }

    /// The terminal grid, where there is one.
    ///
    /// Zero for a desktop, which the status bar renders as nothing rather than as `0×0`: a desktop's
    /// size is in its own status line, in pixels, where it means something.
    pub(crate) fn grid(&self) -> (usize, usize) {
        match self {
            Self::Terminal(tab) => tab.grid(),
            Self::Surface(_) => (0, 0),
        }
    }

    /// Which SSH connection this tab belongs to, where it belongs to one.
    pub(crate) fn connection(&self) -> Option<ConnectionId> {
        match self {
            Self::Terminal(tab) => tab.connection,
            Self::Surface(_) => None,
        }
    }

    /// Move whatever has arrived. Returns true if anything changed.
    pub(crate) fn pump(&mut self, ctx: &egui::Context) -> bool {
        match self {
            Self::Terminal(tab) => tab.pump(),
            Self::Surface(tab) => tab.pump(ctx),
        }
    }

    /// End the session.
    pub(crate) fn shutdown(&mut self) {
        match self {
            Self::Terminal(tab) => tab.shutdown(),
            Self::Surface(tab) => tab.shutdown(),
        }
    }

    /// The desktop inside, when this is one.
    ///
    /// The one accessor that admits which variant it wants, because the questions only a desktop can
    /// answer — is there a key to confirm, what did it settle on — have no terminal counterpart to
    /// generalise over.
    pub(crate) fn surface_mut(&mut self) -> Option<&mut SurfaceTab> {
        match self {
            Self::Surface(tab) => Some(tab),
            Self::Terminal(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property this enum exists for, checked on the shape of the code rather than at run time:
    /// every question above is answerable by both variants, so nothing above `Pane` has to know
    /// which it is holding. A new variant that cannot answer one of them will not compile, which is
    /// the whole mechanism.
    #[test]
    fn both_variants_answer_every_question() {
        fn assert_total(pane: &Pane) {
            let _ = pane.title();
            let _ = pane.program_title();
            let _ = pane.protocol();
            let _ = pane.status_line();
            let _ = pane.grid();
            let _ = pane.connection();
        }
        // Not called: constructing either variant needs a live transport or a live helper, and this
        // is a statement about the interface rather than about a value.
        let _ = assert_total;
    }
}
