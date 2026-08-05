//! Local shells as a [`Transport`].
//!
//! Wraps `portable-pty`, which abstracts ConPTY on Windows and unix ptys elsewhere behind one
//! trait. That crate lives in the WezTerm workspace, whose release cadence has slowed considerably;
//! it is small and stable enough that vendoring it is a viable fallback, and confining its use to
//! this crate is what keeps that option open.

mod shells;

pub use shells::{ShellKind, ShellProfile, discover, parse_wsl_list, which};

use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;

use bestterm_transport::{
    ExitInfo, GridSize, OpenTransport, Result, Transport, TransportError, TransportEvent,
    TransportKind,
};
use crossbeam_channel::Sender;
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Read buffer size. Large enough that `cat`-ing a big file is not syscall-bound, small enough that
/// the UI still sees output arrive incrementally rather than in visible jumps.
const READ_BUFFER: usize = 64 * 1024;

/// A shell running on this machine, attached to a pseudo-terminal.
pub struct PtyTransport {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    size: GridSize,
    label: String,
    closed: bool,
}

impl PtyTransport {
    /// Spawn `profile` in a new pty sized to `size`.
    ///
    /// The returned [`OpenTransport`] carries both halves: writes go through the transport, output
    /// arrives on the channel. A reader thread owns the child process and reports its exit status
    /// as the final [`TransportEvent::Closed`].
    pub fn spawn(profile: &ShellProfile, size: GridSize) -> Result<OpenTransport> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(to_pty_size(size)).map_err(protocol)?;

        let mut cmd = CommandBuilder::new(&profile.program);
        for arg in &profile.args {
            cmd.arg(arg);
        }
        if let Some(cwd) = default_cwd() {
            cmd.cwd(cwd);
        }
        // Advertise what we actually implement. `xterm-256color` is the honest baseline for
        // alacritty_terminal; claiming more breaks remote programs that take us at our word.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "BestTerm");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));

        let child = pair.slave.spawn_command(cmd).map_err(protocol)?;
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().map_err(protocol)?;
        let writer = pair.master.take_writer().map_err(protocol)?;

        // Release our handle on the slave end. Without this the pty never reports EOF, because this
        // process still holds it open after the child is gone.
        drop(pair.slave);

        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name(format!("pty-reader:{}", profile.id))
            .spawn(move || read_loop(reader, child, tx))
            .map_err(TransportError::Io)?;

        tracing::debug!(
            shell = %profile.id,
            program = %profile.program,
            cols = size.cols,
            rows = size.rows,
            "spawned local shell"
        );

        Ok(OpenTransport {
            transport: Box::new(Self {
                master: pair.master,
                writer,
                killer,
                size,
                label: profile.label.clone(),
                closed: false,
            }),
            events: rx,
        })
    }
}

impl Transport for PtyTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::LocalShell
    }

    fn write(&mut self, data: &[u8]) -> Result<()> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    fn resize(&mut self, size: GridSize) -> Result<()> {
        // The trait promises callers may do this every frame during a window drag.
        if size == self.size {
            return Ok(());
        }
        self.master.resize(to_pty_size(size)).map_err(protocol)?;
        self.size = size;
        Ok(())
    }

    fn size(&self) -> GridSize {
        self.size
    }

    fn shutdown(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        // A child that already exited is the normal case here, not a failure worth reporting.
        if let Err(err) = self.killer.kill() {
            tracing::debug!(%err, "killing pty child failed; it had most likely already exited");
        }
        Ok(())
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

fn read_loop(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn Child + Send + Sync>,
    tx: Sender<TransportEvent>,
) {
    let mut buf = vec![0u8; READ_BUFFER];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(TransportEvent::Output(buf[..n].to_vec())).is_err() {
                    // The receiver is gone, so the tab was closed. Take the child with us instead
                    // of leaving an orphaned shell running.
                    let _ = child.kill();
                    return;
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => {
                // A closed ConPTY surfaces as an opaque error on Windows rather than a clean EOF,
                // so this is an ordinary end-of-stream, not something to show the user.
                tracing::debug!(%err, "pty read ended");
                break;
            }
        }
    }

    let info = match child.wait() {
        Ok(status) => ExitInfo {
            code: Some(i32::try_from(status.exit_code()).unwrap_or(-1)),
            signal: status.signal().map(str::to_string),
            message: None,
        },
        Err(err) => ExitInfo {
            code: None,
            signal: None,
            message: Some(err.to_string()),
        },
    };
    let _ = tx.send(TransportEvent::Closed(info));
}

fn to_pty_size(size: GridSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

/// Where a new shell should start.
///
/// `std::env::home_dir` has a chequered deprecation history, so the environment is read directly:
/// `HOME` on unix, `USERPROFILE` on Windows.
fn default_cwd() -> Option<PathBuf> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key) {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

/// `portable-pty` reports `anyhow::Error`; keep that out of this crate's public surface.
fn protocol<E: std::fmt::Display>(err: E) -> TransportError {
    TransportError::Protocol(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Smoke test that the whole pty path works on this platform: open a pty, spawn the default
    /// shell, and confirm the reader thread delivers something. Deliberately tolerant about *what*
    /// arrives — shells differ in whether they greet, and asserting on their output would make this
    /// a test of bash's changelog.
    #[test]
    fn spawns_default_shell_and_produces_events() {
        let shells = discover();
        let profile = shells.first().expect("discover() is never empty");

        let open = PtyTransport::spawn(profile, GridSize::new(80, 24))
            .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", profile.program));

        let event = open.events.recv_timeout(Duration::from_secs(15));
        assert!(
            event.is_ok(),
            "no event from {} within 15s; the reader thread is not wired up",
            profile.program
        );

        let mut transport = open.transport;
        assert_eq!(transport.kind(), TransportKind::LocalShell);
        assert_eq!(transport.size(), GridSize::new(80, 24));
        transport.shutdown().expect("shutdown is infallible");
        // Idempotent, as the trait documents.
        transport.shutdown().expect("second shutdown is a no-op");
    }

    #[test]
    fn resize_is_a_no_op_when_unchanged() {
        let shells = discover();
        let profile = shells.first().expect("discover() is never empty");
        let open = PtyTransport::spawn(profile, GridSize::new(80, 24)).expect("spawn");
        let mut transport = open.transport;

        transport.resize(GridSize::new(80, 24)).expect("no-op resize");
        transport.resize(GridSize::new(100, 30)).expect("real resize");
        assert_eq!(transport.size(), GridSize::new(100, 30));

        transport.shutdown().expect("shutdown");
    }

    #[test]
    fn write_after_shutdown_is_refused() {
        let shells = discover();
        let profile = shells.first().expect("discover() is never empty");
        let open = PtyTransport::spawn(profile, GridSize::new(80, 24)).expect("spawn");
        let mut transport = open.transport;

        transport.shutdown().expect("shutdown");
        assert!(matches!(
            transport.write(b"echo hi\n"),
            Err(TransportError::Closed)
        ));
    }
}
