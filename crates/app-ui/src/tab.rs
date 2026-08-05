//! One tab: a transport, an emulator, and the pumping between them.

use bestterm_core_pty::{PtyTransport, ShellProfile};
use bestterm_core_terminal::{AlacrittyEmulator, Palette, TerminalEmulator};
use bestterm_transport::{
    EventReceiver, ExitInfo, GridSize, Result as TransportResult, Transport, TransportEvent,
};

/// Bytes accepted from one transport in a single frame.
///
/// A command that dumps a gigabyte would otherwise keep the UI thread inside `pump` indefinitely and
/// freeze the window. Hitting the cap simply defers the rest to the next frame, which is why `pump`
/// reports that something changed and the caller requests a repaint.
const OUTPUT_BUDGET: usize = 4 * 1024 * 1024;

/// A terminal tab.
pub(crate) struct TerminalTab {
    transport: Box<dyn Transport>,
    events: EventReceiver<TransportEvent>,
    emulator: AlacrittyEmulator,
    /// Used until the remote program sets a title of its own.
    fallback_title: String,
    exit: Option<ExitInfo>,
    grid: (usize, usize),
}

impl TerminalTab {
    /// Open a tab running `profile`.
    pub(crate) fn spawn(
        profile: &ShellProfile,
        cols: usize,
        rows: usize,
        scrollback: usize,
        palette: Palette,
    ) -> TransportResult<Self> {
        let open = PtyTransport::spawn(profile, GridSize::new(cols as u16, rows as u16))?;
        let emulator = AlacrittyEmulator::new(cols, rows, scrollback, palette);

        Ok(Self {
            transport: open.transport,
            events: open.events,
            emulator,
            fallback_title: profile.label.clone(),
            exit: None,
            grid: (cols, rows),
        })
    }

    /// Drain pending output into the emulator and send back anything it owes the peer.
    ///
    /// Returns true if the visible state may have changed.
    pub(crate) fn pump(&mut self) -> bool {
        let mut changed = false;
        let mut budget = OUTPUT_BUDGET;

        while budget > 0 {
            match self.events.try_recv() {
                Ok(TransportEvent::Output(bytes)) => {
                    budget = budget.saturating_sub(bytes.len().max(1));
                    self.emulator.advance(&bytes);
                    changed = true;
                }
                Ok(TransportEvent::Closed(info)) => {
                    tracing::debug!(?info, "session ended");
                    self.exit = Some(info);
                    changed = true;
                    break;
                }
                Ok(TransportEvent::Error(message)) => {
                    tracing::warn!(%message, "transport reported an error");
                    changed = true;
                }
                // Empty or disconnected: nothing more to do this frame either way.
                Err(_) => break,
            }
        }

        // Device-attribute and colour queries must be answered or the remote program waits forever.
        for response in self.emulator.take_responses() {
            if let Err(err) = self.transport.write(&response) {
                tracing::debug!(%err, "could not answer a terminal query");
                break;
            }
        }

        if self.emulator.take_bell() {
            // Phase 1 turns this into a visual bell; repainting is the honest minimum.
            changed = true;
        }
        // Clipboard integration lands in phase 1; draining keeps the queue from growing without
        // bound in the meantime.
        let _ = self.emulator.take_clipboard_stores();

        changed
    }

    /// Match the emulator and the peer to a new grid size.
    pub(crate) fn resize(&mut self, cols: usize, rows: usize, cell: (u16, u16)) {
        // Always refresh the cell size: the font can change without the grid changing.
        self.emulator.set_cell_size(cell.0, cell.1);

        if (cols, rows) == self.grid {
            return;
        }
        self.grid = (cols, rows);
        self.emulator.resize(cols, rows);

        if let Err(err) = self.transport.resize(crate::grid_size(cols, rows, cell)) {
            tracing::debug!(%err, "resizing the transport failed");
        }
    }

    /// Send bytes to the peer.
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        if self.exit.is_some() {
            return;
        }
        if let Err(err) = self.transport.write(bytes) {
            tracing::debug!(%err, "writing to the transport failed");
        }
    }

    pub(crate) fn emulator(&self) -> &AlacrittyEmulator {
        &self.emulator
    }

    pub(crate) fn emulator_mut(&mut self) -> &mut AlacrittyEmulator {
        &mut self.emulator
    }

    /// The tab's label: whatever the remote program set, else the shell's name.
    pub(crate) fn title(&self) -> String {
        self.emulator
            .title()
            .unwrap_or(&self.fallback_title)
            .to_string()
    }

    /// Protocol identifier, for icon selection.
    pub(crate) fn protocol(&self) -> &'static str {
        self.transport.kind().id()
    }

    pub(crate) fn grid(&self) -> (usize, usize) {
        self.grid
    }

    /// One line describing this tab for the status bar.
    pub(crate) fn status_line(&self) -> String {
        match &self.exit {
            Some(info) if info.is_success() => format!("{} — exited", self.transport.label()),
            Some(info) => {
                let detail = info
                    .signal
                    .clone()
                    .or_else(|| info.code.map(|code| code.to_string()))
                    .unwrap_or_else(|| "unknown".to_string());
                format!("{} — exited ({detail})", self.transport.label())
            }
            None => format!("{} {}", self.transport.kind(), self.transport.label()),
        }
    }

    /// Terminate the peer.
    pub(crate) fn shutdown(&mut self) {
        if let Err(err) = self.transport.shutdown() {
            tracing::debug!(%err, "shutting down the transport failed");
        }
    }
}
