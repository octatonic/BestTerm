//! The byte-stream protocol boundary.
//!
//! Everything that presents itself to the user as *a terminal* — a local shell, an SSH channel, a
//! telnet connection, a serial port — reduces to the same thing: a bidirectional stream of bytes
//! attached to a resizable character grid. [`Transport`] is that reduction.
//!
//! Frame-based protocols (RDP, VNC, X11 windows) do **not** belong here; they live behind
//! `bestterm-surface`. Keeping the two apart is deliberate — see `docs/ARCHITECTURE.md`.
//!
//! # Shape of the API
//!
//! Writes are synchronous and cheap, because they happen on the UI thread in response to a
//! keystroke and must not require an async context. Output is asynchronous and arrives as
//! [`TransportEvent`]s on a [`crossbeam_channel::Receiver`], because it is produced by a reader
//! thread or a tokio task that the UI never waits on.

use std::fmt;

pub use crossbeam_channel::Receiver as EventReceiver;

/// Errors a transport can report.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The underlying handle failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The peer is gone; the transport cannot be used again.
    #[error("transport is closed")]
    Closed,

    /// Authentication was rejected or could not be completed.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// A protocol-level failure that is not an I/O error.
    #[error("{0}")]
    Protocol(String),
}

/// Result alias used throughout the transport layer.
pub type Result<T> = std::result::Result<T, TransportError>;

/// Which protocol is behind a transport.
///
/// Used for display, for choosing an icon, and for deciding which capabilities to offer — not for
/// branching on behaviour, which is what the trait is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransportKind {
    /// A shell running on this machine through a pty.
    LocalShell,
    /// A channel on an SSH connection.
    Ssh,
    /// A telnet connection.
    Telnet,
    /// A serial port.
    Serial,
    /// An rlogin connection.
    Rlogin,
}

impl TransportKind {
    /// Short lowercase identifier, stable enough to persist in configuration.
    pub fn id(self) -> &'static str {
        match self {
            Self::LocalShell => "shell",
            Self::Ssh => "ssh",
            Self::Telnet => "telnet",
            Self::Serial => "serial",
            Self::Rlogin => "rlogin",
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// Size of the character grid presented to the remote end.
///
/// Pixel dimensions are reported because some remote programs use them (sixel, image protocols,
/// `TIOCGWINSZ` consumers that care). They are allowed to be zero when unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    /// Columns of text. Never zero.
    pub cols: u16,
    /// Rows of text. Never zero.
    pub rows: u16,
    /// Width of the text area in pixels, or zero if unknown.
    pub pixel_width: u16,
    /// Height of the text area in pixels, or zero if unknown.
    pub pixel_height: u16,
}

impl GridSize {
    /// A grid of `cols` × `rows`, with unknown pixel dimensions.
    ///
    /// Both dimensions are clamped to at least 1: a zero-sized pty makes several platforms
    /// misbehave, and a zero-column grid divides by zero in every renderer ever written.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// A grid with pixel dimensions attached.
    pub fn with_pixels(cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) -> Self {
        Self {
            pixel_width,
            pixel_height,
            ..Self::new(cols, rows)
        }
    }
}

impl Default for GridSize {
    fn default() -> Self {
        Self::new(80, 24)
    }
}

/// Why a transport ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExitInfo {
    /// Process exit code, where one exists.
    pub code: Option<i32>,
    /// Signal name, where the process was signalled.
    pub signal: Option<String>,
    /// Human-readable detail to show the user.
    pub message: Option<String>,
}

impl ExitInfo {
    /// Whether this represents a clean exit.
    ///
    /// A `message` counts against it. A missing exit code is ordinary -- an interactive shell often
    /// closes without sending one -- so `None` alone means nothing went wrong; but a transport that
    /// went to the trouble of explaining itself is explaining a failure. Without this, a connection
    /// that died mid-session reads exactly like one where somebody typed `exit`.
    pub fn is_success(&self) -> bool {
        self.signal.is_none() && self.message.is_none() && matches!(self.code, Some(0) | None)
    }
}

/// Something that happened on a transport, delivered to the session layer.
#[derive(Clone, Debug)]
pub enum TransportEvent {
    /// Bytes arrived from the peer. Feed straight into the terminal emulator.
    Output(Vec<u8>),
    /// The peer closed. No further events will arrive.
    Closed(ExitInfo),
    /// A recoverable problem worth surfacing to the user.
    Error(String),
}

/// A live byte-stream connection.
///
/// Implementations are owned by the session layer and used from the UI thread. Nothing here blocks
/// for longer than a single `write` syscall.
pub trait Transport: Send {
    /// Which protocol this is.
    fn kind(&self) -> TransportKind;

    /// Send bytes to the peer.
    fn write(&mut self, data: &[u8]) -> Result<()>;

    /// Tell the peer the grid changed size.
    ///
    /// Callers may invoke this on every frame during an interactive window drag; implementations
    /// must therefore make a no-op resize cheap.
    fn resize(&mut self, size: GridSize) -> Result<()>;

    /// The size last successfully applied.
    fn size(&self) -> GridSize;

    /// Close the connection. Idempotent.
    fn shutdown(&mut self) -> Result<()>;

    /// A label for the UI: the shell name, `user@host`, the serial device path.
    fn label(&self) -> String;
}

/// A transport plus the channel its events arrive on.
///
/// Returned by every `open`-style constructor so that the two halves cannot be separated by
/// accident.
pub struct OpenTransport {
    /// The write and control half.
    pub transport: Box<dyn Transport>,
    /// The read half.
    pub events: EventReceiver<TransportEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(code: Option<i32>, signal: Option<&str>, message: Option<&str>) -> ExitInfo {
        ExitInfo {
            code,
            signal: signal.map(str::to_string),
            message: message.map(str::to_string),
        }
    }

    #[test]
    fn a_shell_that_simply_ended_is_a_clean_exit() {
        assert!(exit(Some(0), None, None).is_success());
        // Interactive shells routinely close without sending a status. That is not a failure.
        assert!(exit(None, None, None).is_success());
    }

    #[test]
    fn a_transport_that_explained_itself_was_explaining_a_failure() {
        // The distinction this exists for: a dropped network and `exit` both end a shell with no
        // status, and only one of them is worth telling somebody about.
        assert!(!exit(None, None, Some("the connection failed: Keepalive timeout")).is_success());
        assert!(!exit(Some(0), None, Some("the server closed the connection")).is_success());
    }

    #[test]
    fn a_status_or_a_signal_still_counts() {
        assert!(!exit(Some(1), None, None).is_success());
        assert!(!exit(None, Some("KILL"), None).is_success());
    }

    #[test]
    fn grid_size_never_zero() {
        let g = GridSize::new(0, 0);
        assert_eq!((g.cols, g.rows), (1, 1));
    }

    #[test]
    fn with_pixels_keeps_clamping() {
        let g = GridSize::with_pixels(0, 5, 640, 480);
        assert_eq!(
            (g.cols, g.rows, g.pixel_width, g.pixel_height),
            (1, 5, 640, 480)
        );
    }

    #[test]
    fn exit_info_success_cases() {
        assert!(ExitInfo::default().is_success());
        assert!(
            ExitInfo {
                code: Some(0),
                ..Default::default()
            }
            .is_success()
        );
        assert!(
            !ExitInfo {
                code: Some(1),
                ..Default::default()
            }
            .is_success()
        );
        assert!(
            !ExitInfo {
                signal: Some("SIGTERM".into()),
                ..Default::default()
            }
            .is_success()
        );
    }

    #[test]
    fn kind_ids_are_unique() {
        let kinds = [
            TransportKind::LocalShell,
            TransportKind::Ssh,
            TransportKind::Telnet,
            TransportKind::Serial,
            TransportKind::Rlogin,
        ];
        let mut ids: Vec<_> = kinds.iter().map(|k| k.id()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }
}
