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
    /// Which SSH connection this tab runs over, when it runs over one.
    ///
    /// Beside `_owner` rather than derived from it: the owner is erased to `dyn Any` because a tab
    /// genuinely does not care what it is holding, and this is the one thing about it a tab does have
    /// to know -- so that closing the last window on a connection can take the connection's tunnels
    /// with it. `None` for a local shell, which has no connection to belong to.
    pub(crate) connection: Option<crate::tunnels::ConnectionId>,
    /// What it would take to open this session again, or why it cannot be.
    ///
    /// Held by the tab because the tab is what somebody points at when they say "get that back", and
    /// because neither half can be recovered later: the credential was consumed by the handshake that
    /// used it, and the server's key is only knowable while the connection that saw it exists.
    ///
    /// `Err` for a local shell, which has no session to reopen, and for a login whose credential was
    /// a one-time code.
    pub(crate) reopen: Result<Reopen, bestterm_proto_ssh::NotReconnectable>,
    /// Whatever the transport needs kept alive underneath it.
    ///
    /// For an SSH tab this is the connection the shell channel hangs off; dropping it would close the
    /// session and, with it, this tab's transport. A local shell has nothing here. It is `dyn Any`
    /// because a tab genuinely does not care what it is holding -- only that it must not be dropped
    /// first.
    _owner: Option<Box<dyn std::any::Any + Send + Sync>>,
}

/// Something that asks the interface to draw a frame.
///
/// A closure rather than the windowing type, so this module says what it needs — "wake up" — without
/// naming who provides it, and so a test can count the wake-ups.
pub(crate) type Waker = Arc<dyn Fn() + Send + Sync>;

/// What is needed to open a dead session again.
pub(crate) struct Reopen {
    /// The credential and the pin.
    pub(crate) ready: Box<bestterm_proto_ssh::Reconnectable>,
    /// Where it was, which is where it goes again.
    pub(crate) target: Box<bestterm_core_model::SshConfig>,
}

/// Everything a tab needs to exist.
///
/// A struct rather than eight parameters, which is both what clippy asked for and what reads better:
/// at a call site the names are visible, and `owner` in particular is the kind of argument that is
/// invisible and load-bearing when it is the eighth positional one.
pub(crate) struct NewTab {
    /// The transport, already open.
    pub(crate) open: OpenTransport,
    /// The session's name, which is what the tab is labelled with.
    pub(crate) title: String,
    /// Initial grid width.
    pub(crate) cols: usize,
    /// Initial grid height.
    pub(crate) rows: usize,
    /// Scrollback lines to keep.
    pub(crate) scrollback: usize,
    /// Colours for the emulator.
    pub(crate) palette: Palette,
    /// What wakes the interface when output arrives.
    pub(crate) waker: Waker,
    /// Whatever must be kept alive underneath the transport; see [`TerminalTab`].
    pub(crate) owner: Option<Box<dyn std::any::Any + Send + Sync>>,
}

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
        Ok(Self::adopt(NewTab {
            open,
            title: profile.label.clone(),
            cols,
            rows,
            scrollback,
            palette,
            waker,
            // A local shell's process is owned by the transport itself.
            owner: None,
        }))
    }

    /// Take over a transport somebody else opened.
    ///
    /// A local shell and an SSH session differ only in how the transport comes into being; once it
    /// exists, a tab treats them identically, which is the point of [`Transport`] being a trait. This
    /// is the constructor SSH uses, and [`TerminalTab::spawn`] is a thin wrapper over it.
    pub(crate) fn adopt(spec: NewTab) -> Self {
        let NewTab {
            open,
            title,
            cols,
            rows,
            scrollback,
            palette,
            waker,
            owner,
        } = spec;
        let emulator = AlacrittyEmulator::new(cols, rows, scrollback, palette);
        let events = relay(open.events, waker, title.clone());

        Self {
            transport: open.transport,
            events,
            emulator,
            fallback_title: title,
            exit: None,
            grid: (cols, rows),
            connection: None,
            // Replaced by the caller for an SSH session. A local shell keeps this, because reopening
            // one is `spawn`, not a reconnect.
            reopen: Err(bestterm_proto_ssh::NotReconnectable::Interactive),
            _owner: owner,
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
                // The message first, because it is the only one of the three written for a person:
                // "the connection failed: Keepalive timeout" says what to do next, and "exited (1)"
                // does not.
                let detail = info
                    .message
                    .clone()
                    .or_else(|| info.signal.clone())
                    .or_else(|| info.code.map(|code| code.to_string()))
                    .unwrap_or_else(|| "unknown".to_string());
                format!("{} — exited ({detail})", self.transport.label())
            }
            None => format!("{} {}", self.transport.kind(), self.transport.label()),
        }
    }

    /// Whether this tab is a dead SSH session that could be opened again.
    ///
    /// Both halves matter. A live session has nothing to reconnect; a local shell and a session whose
    /// credential cannot be replayed have nothing to reconnect *with*, and offering a button that
    /// then explains why it will not work is worse than not offering it.
    pub(crate) fn can_reconnect(&self) -> bool {
        self.exit.is_some() && self.reopen.is_ok()
    }

    /// Terminate the peer.
    pub(crate) fn shutdown(&mut self) {
        if let Err(err) = self.transport.shutdown() {
            tracing::debug!(%err, "shutting down the transport failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bestterm_transport::TransportKind;

    use super::*;

    /// A transport that does nothing, so a tab can be built without a shell or a network.
    struct Inert;

    impl Transport for Inert {
        fn kind(&self) -> TransportKind {
            TransportKind::Ssh
        }
        fn write(&mut self, _data: &[u8]) -> TransportResult<()> {
            Ok(())
        }
        fn resize(&mut self, _size: GridSize) -> TransportResult<()> {
            Ok(())
        }
        fn size(&self) -> GridSize {
            GridSize::new(80, 24)
        }
        fn shutdown(&mut self) -> TransportResult<()> {
            Ok(())
        }
        fn label(&self) -> String {
            "inert".to_string()
        }
    }

    /// Counts its own destruction, which is the whole point of the test below.
    struct Tattletale(Arc<AtomicUsize>);

    impl Drop for Tattletale {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn inert_tab(owner: Option<Box<dyn std::any::Any + Send + Sync>>) -> TerminalTab {
        let (_sender, events) = crossbeam_channel::unbounded();
        // Leaked deliberately: dropping the sender would close the channel and stop the relay thread
        // before the test has finished looking at the tab.
        std::mem::forget(_sender);
        TerminalTab::adopt(NewTab {
            open: OpenTransport {
                transport: Box::new(Inert),
                events,
            },
            title: "test".to_string(),
            cols: 80,
            rows: 24,
            scrollback: 100,
            palette: Palette::default(),
            waker: Arc::new(|| {}),
            owner,
        })
    }

    /// The regression this exists for: an SSH tab's transport is a channel on a connection, and the
    /// connection owns the sender the session loop reads from. A tab that does not hold the connection
    /// lets it drop as soon as the caller's binding goes out of scope, which closes the session and
    /// kills the tab's own transport a moment after it was opened. Nothing observable fails at compile
    /// time, and a live test only catches it if authentication succeeds -- so it is pinned here.
    #[test]
    fn a_tab_keeps_its_owner_alive() {
        let drops = Arc::new(AtomicUsize::new(0));
        let tab = inert_tab(Some(Box::new(Tattletale(Arc::clone(&drops)))));

        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "the owner must outlive the call that built the tab"
        );

        drop(tab);
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "and must be released when the tab closes, not leaked"
        );
    }

    #[test]
    fn a_local_shell_needs_no_owner() {
        // A PTY transport owns its process, so `None` is the honest answer rather than a placeholder.
        let tab = inert_tab(None);
        assert!(tab._owner.is_none());
    }
}
