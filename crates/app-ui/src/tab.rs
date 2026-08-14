//! One tab: a transport, an emulator, and the pumping between them.
//!
//! # Why a relay thread sits between the transport and the emulator
//!
//! [`TerminalTab::pump`] runs inside a frame, and frames happen when the windowing layer decides to
//! draw one — which it does on input, or when something asks it to. Output arriving on a channel is
//! neither. The first version of this had the pump request a repaint only when it *had* found
//! output, which cannot work: the shell's first prompt arrives a moment after the first frame, by
//! which time nothing is drawing frames any more and nothing will call the pump again. The window
//! stayed blank until the mouse moved over it.
//!
//! So each tab owns a thread that blocks on the transport's channel, forwards what arrives, and then
//! wakes the interface. Polling on a timer would also have worked and would have kept a core busy
//! redrawing an unchanged screen forever; waiting on the channel costs one parked thread per tab and
//! wakes exactly when there is something new to show.

use std::sync::Arc;

use bestterm_core_pty::{PtyTransport, ShellProfile};
use bestterm_core_terminal::{AlacrittyEmulator, Palette, TerminalEmulator};
use bestterm_transport::{
    EventReceiver, ExitInfo, GridSize, OpenTransport, Result as TransportResult, Transport,
    TransportEvent,
};

/// Bytes accepted from one transport in a single frame.
///
/// A command that dumps a gigabyte would otherwise keep the UI thread inside `pump` indefinitely and
/// freeze the window. Hitting the cap simply defers the rest to the next frame, which is why `pump`
/// reports that something changed and the caller requests a repaint.
const OUTPUT_BUDGET: usize = 4 * 1024 * 1024;

/// Forward events from `source` to a channel the pump reads, waking the interface after each one.
///
/// The wake happens *after* the send, so a frame that starts in response to it always finds the event
/// already queued.
fn relay(
    source: EventReceiver<TransportEvent>,
    waker: Waker,
    label: String,
) -> EventReceiver<TransportEvent> {
    let (sender, receiver) = crossbeam_channel::unbounded();

    std::thread::Builder::new()
        .name(format!("relay: {label}"))
        .spawn(move || {
            // Ends when the transport closes its side or the tab is dropped, which is what makes this
            // a parked thread rather than a leaked one.
            while let Ok(event) = source.recv() {
                let last = matches!(event, TransportEvent::Closed(_));
                if sender.send(event).is_err() {
                    break;
                }
                waker();
                if last {
                    break;
                }
            }
        })
        // A tab whose output cannot be delivered is not worth opening, but failing to spawn a thread
        // means the machine is in no state to report it either. Carrying on with a dead relay would
        // give a window that looks fine and shows nothing, so this is the one place a panic is
        // clearer than the alternative.
        .expect("a thread for a terminal tab");

    receiver
}

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

/// Something that asks the interface to draw a frame.
///
/// A closure rather than the windowing type, so this module says what it needs — "wake up" — without
/// naming who provides it, and so a test can count the wake-ups.
pub(crate) type Waker = Arc<dyn Fn() + Send + Sync>;

impl TerminalTab {
    /// Open a tab running `profile`.
    pub(crate) fn spawn(
        profile: &ShellProfile,
        cols: usize,
        rows: usize,
        scrollback: usize,
        palette: Palette,
        waker: Waker,
    ) -> TransportResult<Self> {
        let open = PtyTransport::spawn(profile, GridSize::new(cols as u16, rows as u16))?;
        Ok(Self::adopt(
            open,
            profile.label.clone(),
            cols,
            rows,
            scrollback,
            palette,
            waker,
        ))
    }

    /// Take over a transport somebody else opened.
    ///
    /// A local shell and an SSH session differ only in how the transport comes into being; once it
    /// exists, a tab treats them identically, which is the point of [`Transport`] being a trait. This
    /// is the constructor SSH uses, and [`TerminalTab::spawn`] is a thin wrapper over it.
    pub(crate) fn adopt(
        open: OpenTransport,
        title: String,
        cols: usize,
        rows: usize,
        scrollback: usize,
        palette: Palette,
        waker: Waker,
    ) -> Self {
        let emulator = AlacrittyEmulator::new(cols, rows, scrollback, palette);
        let events = relay(open.events, waker, title.clone());

        Self {
            transport: open.transport,
            events,
            emulator,
            fallback_title: title,
            exit: None,
            grid: (cols, rows),
        }
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
    /// The session's own name, which is what the tab is labelled with.
    pub(crate) fn title(&self) -> String {
        self.fallback_title.clone()
    }

    /// What the program inside last announced, when it announced something else.
    ///
    /// Kept apart from [`TerminalTab::title`] deliberately. PowerShell announces its own executable
    /// path, which is both useless and long enough to swallow the tab bar; `vim` announces the file
    /// being edited, which is genuinely worth seeing. Showing it on hover keeps the second without
    /// the first.
    pub(crate) fn program_title(&self) -> Option<String> {
        self.emulator
            .title()
            .filter(|title| *title != self.fallback_title)
            .map(str::to_owned)
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
