//! Telnet, as a [`Transport`].
//!
//! Old, unencrypted, and still the only way to reach a great many switches, PDUs and serial
//! concentrators. It carries no credentials of its own: the server asks for a login over the same
//! stream everything else uses, so there is no authentication step here — the terminal is the
//! authentication step.
//!
//! # Why it is worth saying out loud that this is in clear text
//!
//! Because it is, and because the thing people reach telnet for is usually infrastructure. A password
//! typed into a telnet session is a password on the wire. [`TelnetTransport::open`] logs it at
//! connection time so it appears in the record, and the interface says the same thing.
//!
//! # The shape
//!
//! [`negotiate`] holds the protocol and no socket, so the option dance is tested without a network.
//! This module is the plumbing: a socket, a reader task that turns bytes into
//! [`TransportEvent`]s, and a writer task fed by a channel — the same shape `proto-ssh` uses, for the
//! same reason. Nothing here blocks the interface.

mod negotiate;

use std::sync::Arc;
use std::time::Duration;

use bestterm_transport::{
    EventReceiver, ExitInfo, GridSize, OpenTransport, Transport, TransportEvent, TransportKind,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::negotiate::Telnet;

/// How long a connection attempt is given.
///
/// A telnet port that is filtered rather than closed never answers, and without this the session
/// would sit "connecting" until the operating system gave up minutes later.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Bytes read from the socket in one go.
const READ_CHUNK: usize = 16 * 1024;

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum TelnetError {
    /// The socket failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// Nothing answered in time.
    #[error("{host}:{port} did not answer within {}s", CONNECT_TIMEOUT.as_secs())]
    Timeout {
        /// The host that was tried.
        host: String,
        /// And the port.
        port: u16,
    },
}

/// What the writer task is asked to do.
enum Command {
    /// Send these bytes, escaped.
    Data(Vec<u8>),
    /// Send these bytes exactly as they are.
    ///
    /// Protocol replies, which are already escaped where they need to be. Escaping them again would
    /// double every `IAC` in a negotiation and turn each command into nonsense.
    Raw(Vec<u8>),
    /// Tell the server the window changed size.
    Resize(GridSize),
    /// Close.
    Shutdown,
}

/// A live telnet connection.
pub struct TelnetTransport {
    commands: mpsc::UnboundedSender<Command>,
    size: GridSize,
    label: String,
    closed: bool,
}

impl std::fmt::Debug for TelnetTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelnetTransport")
            .field("label", &self.label)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl TelnetTransport {
    /// Connect to `host:port`.
    ///
    /// `term` is what the server is told this terminal is, when it asks.
    pub async fn open(
        host: &str,
        port: u16,
        term: &str,
        size: GridSize,
    ) -> Result<OpenTransport, TelnetError> {
        // Said at the moment it becomes true, so it is in the record rather than only in a manual.
        tracing::warn!(
            host,
            port,
            "telnet: this connection is not encrypted; anything typed into it, passwords included, \
             travels in clear text"
        );

        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
            .await
            .map_err(|_| TelnetError::Timeout {
                host: host.to_string(),
                port,
            })??;
        // Interactive typing in small writes: without this every keystroke waits for the previous
        // acknowledgement, which on a distant switch is the difference between usable and not.
        if let Err(error) = stream.set_nodelay(true) {
            tracing::debug!(%error, "telnet: could not disable Nagle's algorithm");
        }

        let label = format!("{host}:{port}");
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();

        let telnet = Arc::new(std::sync::Mutex::new(Telnet::new(
            term, size.cols, size.rows,
        )));
        let (read_half, write_half) = stream.into_split();

        spawn_writer(write_half, commands_rx, Arc::clone(&telnet), label.clone());
        spawn_reader(
            read_half,
            events_tx,
            telnet,
            commands_tx.clone(),
            label.clone(),
        );

        Ok(OpenTransport {
            transport: Box::new(Self {
                commands: commands_tx,
                size,
                label,
                closed: false,
            }),
            events: events_rx,
        })
    }
}

/// The task that turns what arrives into events, and answers the negotiation.
///
/// The answers go back through the writer's channel rather than being written here: one task owns the
/// write half, which is what keeps a reply from interleaving with a keystroke mid-command.
fn spawn_reader(
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    events: crossbeam_channel::Sender<TransportEvent>,
    telnet: Arc<std::sync::Mutex<Telnet>>,
    commands: mpsc::UnboundedSender<Command>,
    label: String,
) {
    tokio::spawn(async move {
        let mut buffer = vec![0u8; READ_CHUNK];
        loop {
            let read = match read_half.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => {
                    let _ = events.send(TransportEvent::Closed(ExitInfo {
                        code: None,
                        signal: None,
                        message: Some(error.to_string()),
                    }));
                    return;
                }
            };

            let parsed = {
                let Ok(mut telnet) = telnet.lock() else { break };
                telnet.receive(&buffer[..read])
            };

            // `Raw` rather than `Data`: these are protocol bytes and are already escaped where they
            // need to be. Sent through the writer's channel rather than written here, so a reply
            // cannot interleave with a keystroke in the middle of a command.
            if !parsed.reply.is_empty() && commands.send(Command::Raw(parsed.reply)).is_err() {
                break;
            }
            if !parsed.data.is_empty() && events.send(TransportEvent::Output(parsed.data)).is_err()
            {
                return;
            }
        }

        tracing::debug!(%label, "telnet: the server closed the connection");
        let _ = events.send(TransportEvent::Closed(ExitInfo {
            code: None,
            signal: None,
            message: None,
        }));
    });
}

/// The task that owns the write half.
fn spawn_writer(
    mut write_half: tokio::net::tcp::OwnedWriteHalf,
    mut commands: mpsc::UnboundedReceiver<Command>,
    telnet: Arc<std::sync::Mutex<Telnet>>,
    label: String,
) {
    tokio::spawn(async move {
        // Sent before anything else. See `Telnet::opening`.
        let opening = {
            let Ok(mut telnet) = telnet.lock() else {
                return;
            };
            telnet.opening()
        };
        if write_half.write_all(&opening).await.is_err() {
            return;
        }

        while let Some(command) = commands.recv().await {
            let bytes = match command {
                Command::Data(data) => {
                    if data.is_empty() {
                        continue;
                    }
                    let Ok(telnet) = telnet.lock() else { return };
                    // Without the binary option agreed, telnet is a seven-bit protocol, and the
                    // high-bit bytes of UTF-8 are not ours to send. Dropping them loses characters;
                    // sending them anyway corrupts the stream for a server that told us not to. The
                    // replacement character is the one answer that is honest at both ends.
                    let mut out = Vec::with_capacity(data.len());
                    if telnet.binary_out() {
                        telnet.escape(&data, &mut out);
                    } else {
                        let mut ascii = Vec::with_capacity(data.len());
                        for byte in &data {
                            ascii.push(if *byte < 0x80 { *byte } else { b'?' });
                        }
                        telnet.escape(&ascii, &mut out);
                    }
                    out
                }
                Command::Raw(bytes) => bytes,
                Command::Resize(size) => {
                    let Ok(mut telnet) = telnet.lock() else {
                        return;
                    };
                    telnet.resize(size.cols, size.rows)
                }
                Command::Shutdown => {
                    let _ = write_half.shutdown().await;
                    return;
                }
            };

            if bytes.is_empty() {
                continue;
            }
            if let Err(error) = write_half.write_all(&bytes).await {
                tracing::debug!(%label, %error, "telnet: write failed");
                return;
            }
        }
    });
}

impl Transport for TelnetTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Telnet
    }

    fn write(&mut self, data: &[u8]) -> bestterm_transport::Result<()> {
        if self.closed {
            return Err(bestterm_transport::TransportError::Closed);
        }
        self.commands
            .send(Command::Data(data.to_vec()))
            .map_err(|_| bestterm_transport::TransportError::Closed)
    }

    fn resize(&mut self, size: GridSize) -> bestterm_transport::Result<()> {
        if size == self.size {
            return Ok(());
        }
        self.size = size;
        // Dropped silently when the server never agreed to window size, which is the writer's
        // decision to make because it holds the negotiation state.
        let _ = self.commands.send(Command::Resize(size));
        Ok(())
    }

    fn size(&self) -> GridSize {
        self.size
    }

    fn shutdown(&mut self) -> bestterm_transport::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let _ = self.commands.send(Command::Shutdown);
        Ok(())
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

/// Re-exported so a caller can name the receiver half without depending on `crossbeam` directly.
pub type Events = EventReceiver<TransportEvent>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_port_nothing_listens_on_is_refused_rather_than_hung() {
        // `OpenTransport` has no `Debug` -- there is nothing in a socket worth printing -- so the
        // result is taken apart by hand rather than with `expect_err`.
        let error =
            match TelnetTransport::open("127.0.0.1", 1, "xterm", GridSize::new(80, 24)).await {
                Ok(_) => panic!("nothing listens on port 1"),
                Err(error) => error,
            };
        // Refused, not timed out: the distinction matters because one means "wrong port" and the
        // other means "wrong network".
        assert!(matches!(error, TelnetError::Io(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_server_that_says_nothing_still_gets_the_opening_offer() {
        // The whole session hangs on this: plenty of telnet servers wait to be spoken to, and one
        // that is never spoken to looks exactly like a connection that failed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("an address");

        let accepted = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("a connection");
            let mut buf = vec![0u8; 64];
            let read = stream.read(&mut buf).await.expect("the opening offer");
            buf.truncate(read);
            buf
        });

        let open = TelnetTransport::open(
            &addr.ip().to_string(),
            addr.port(),
            "xterm",
            GridSize::new(80, 24),
        )
        .await
        .expect("connects");
        assert_eq!(open.transport.kind(), TransportKind::Telnet);

        let offer = accepted.await.expect("the task finished");
        assert!(!offer.is_empty(), "the client must speak first");
        assert_eq!(offer[0], 255, "an offer starts with IAC");
    }

    // Multi-threaded on purpose. The body waits on a `crossbeam` channel, which blocks the thread it
    // is on, and `#[tokio::test]` gives a current-thread runtime -- so the reader and writer tasks
    // never get scheduled and nothing arrives. The application does not have this problem, because its
    // runtime is multi-threaded and its interface is on a thread of its own; a test that pretended
    // otherwise was testing a shape nothing uses.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn what_the_server_sends_reaches_the_terminal_and_commands_do_not() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("an address");

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("a connection");
            let mut discard = vec![0u8; 128];
            let _ = stream.read(&mut discard).await;
            // A command, then data. Only the second should be seen by a terminal.
            let _ = stream.write_all(&[255, 251, 1]).await;
            let _ = stream.write_all(b"login: ").await;
            // Held open, so the reader does not see a close before the data.
            tokio::time::sleep(Duration::from_millis(300)).await;
        });

        let open = TelnetTransport::open(
            &addr.ip().to_string(),
            addr.port(),
            "xterm",
            GridSize::new(80, 24),
        )
        .await
        .expect("connects");

        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match open.events.recv_timeout(Duration::from_millis(200)) {
                Ok(TransportEvent::Output(bytes)) => {
                    seen.extend_from_slice(&bytes);
                    if seen.ends_with(b"login: ") {
                        break;
                    }
                }
                Ok(_) => break,
                Err(_) => {}
            }
        }

        assert_eq!(seen, b"login: ", "commands must not reach the terminal");
    }

    #[test]
    fn the_label_is_the_address_somebody_typed() {
        // Not a resolved address: a session named `switch-3:23` is one somebody recognises, and
        // `10.4.0.19:23` is one they have to look up.
        let (tx, _rx) = mpsc::unbounded_channel();
        let transport = TelnetTransport {
            commands: tx,
            size: GridSize::new(80, 24),
            label: "switch-3:23".to_string(),
            closed: false,
        };
        assert_eq!(transport.label(), "switch-3:23");
        assert_eq!(transport.kind(), TransportKind::Telnet);
    }

    #[test]
    fn writing_to_a_closed_transport_is_an_error_and_not_a_silence() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut transport = TelnetTransport {
            commands: tx,
            size: GridSize::new(80, 24),
            label: "x".to_string(),
            closed: false,
        };
        transport.shutdown().expect("shutting down is allowed");
        assert!(transport.write(b"x").is_err());
        // And twice is fine.
        transport
            .shutdown()
            .expect("shutting down twice is allowed");
    }
}
