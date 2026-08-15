//! An SSH connection, and a shell on it as a [`Transport`].
//!
//! # Shape
//!
//! One [`SshConnection`] is one TCP connection and one authentication. Channels are opened on it
//! afterwards, and each is independent: a terminal, later an SFTP browser, later port forwards. That
//! is the reason this crate implements SSH in process rather than shelling out to `ssh` — see
//! `docs/ARCHITECTURE.md`.
//!
//! Keeping the connection alive is the caller's job. Dropping [`SshConnection`] tears down every
//! channel on it, so the session layer holds it while any tab is still open.
//!
//! # Threads
//!
//! A shell channel is driven by two tasks, not one: `russh` splits a channel into halves, so reading
//! and writing never contend for the same borrow. Writes reach the writer task through a queue, which
//! is what lets [`Transport::write`] stay synchronous and callable from the UI thread.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bestterm_transport::{
    ExitInfo, GridSize, OpenTransport, Result as TransportResult, Transport, TransportError,
    TransportEvent, TransportKind,
};
use russh::client::{self, Handle};
use russh::keys::ssh_key;
use russh::{ChannelMsg, Disconnect};
use tokio::sync::{mpsc, oneshot};

use crate::auth::Auth;
use crate::forward::{ForwardRegistry, Incoming};
use crate::host_key::{HostKeyChecker, HostKeyOutcome, HostKeyVerifier, host_key_from_ssh};
use crate::known_hosts::KnownHosts;

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// The `russh` layer failed.
    #[error("ssh error: {0}")]
    Ssh(#[from] russh::Error),

    /// The server's host key could not be read.
    #[error("the server's host key could not be decoded")]
    UnreadableHostKey,

    /// Verification refused the server.
    ///
    /// Distinct from an authentication failure: this one means the *server* was not accepted, which
    /// is a very different thing to tell a user.
    #[error("the server's host key was not accepted")]
    HostKeyRejected,

    /// The server rejected every credential offered.
    #[error("authentication failed; the server will still accept: {}", remaining.join(", "))]
    AuthenticationFailed {
        /// Methods the server said it would still consider.
        remaining: Vec<String>,
    },

    /// The server accepted the credential but wants another factor.
    ///
    /// Reported separately because it is not a failure — it is a prompt for the next step.
    #[error("another authentication factor is required")]
    FurtherAuthenticationRequired {
        /// Methods the server will accept next.
        remaining: Vec<String>,
    },

    /// A private key could not be read.
    ///
    /// Names the file, because the usual causes — wrong path, wrong passphrase, a key in a format
    /// this build cannot parse — are all things the user fixes by looking at that file.
    #[error("could not read the private key {path}: {detail}")]
    PrivateKey {
        /// The key file.
        path: std::path::PathBuf,
        /// What went wrong.
        detail: String,
    },

    /// The SSH agent could not be reached or refused.
    #[error("the ssh agent: {0}")]
    Agent(String),

    /// The agent is running but holds nothing to offer.
    #[error("the ssh agent is running but holds no keys")]
    AgentHasNoKeys,

    /// The user abandoned an interactive prompt.
    ///
    /// Not a wrong answer: nothing was rejected, so there is nothing to retry differently.
    #[error("authentication was cancelled")]
    AuthenticationCancelled,

    /// A responder returned the wrong number of answers.
    #[error("the server asked {asked} question(s) and got {answered} answer(s)")]
    InteractiveAnswerCount {
        /// Prompts the server sent.
        asked: usize,
        /// Answers supplied.
        answered: usize,
    },

    /// The server kept asking questions.
    #[error("the server asked too many rounds of questions")]
    TooManyInteractiveRounds,

    /// A local socket could not be opened.
    ///
    /// Almost always a forward asking for a port something else already has.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The server would not listen on our behalf.
    ///
    /// Usually a port below 1024, a port already taken on the server, or `GatewayPorts no` refusing
    /// a bind address other than loopback. The server does not say which, so the message says where
    /// to look rather than guessing.
    #[error("the server refused to listen on {address}:{port}")]
    ForwardDenied {
        /// The address asked for.
        address: String,
        /// The port asked for.
        port: u16,
    },

    /// The server allocated a port outside the range a port number can hold.
    ///
    /// Cannot happen against a correct server; checked because the wire type is 32 bits and
    /// truncating would silently point a forward at the wrong port.
    #[error("the server allocated an impossible port number {0}")]
    ImpossiblePort(u32),
}

/// Where to connect.
#[derive(Clone, Debug)]
pub struct Target {
    /// Host name or address.
    pub host: String,
    /// Port.
    pub port: u16,
    /// Login name.
    pub user: String,
}

/// One link in a connection: where to go, how to prove who we are, and who decides about its key.
///
/// A jump host is a full SSH connection with its own everything. OpenSSH is explicit that "the
/// configuration for the destination host is not generally applied to jump hosts", and treating a
/// bastion as a mere address — reusing the destination's credentials and skipping its host key
/// check — is how a chain quietly stops being verified.
pub struct Hop {
    /// Where this link goes.
    pub target: Target,
    /// How to authenticate to it.
    pub auth: Auth,
    /// Who decides about its host key.
    pub verifier: Arc<dyn HostKeyVerifier>,
}

impl std::fmt::Debug for Hop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hop")
            .field("target", &self.target)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

/// How often the client asks the server whether it is still there.
///
/// `russh`'s client config sets none of the liveness fields by default, and `russh` never enables
/// `SO_KEEPALIVE` on the socket either. Without this, a connection whose peer vanished -- a laptop
/// carried out of range, a firewall that forgot the flow, a server that lost power -- parks the
/// session task inside `read` and stays there. Nothing times out, the tab looks connected, and typing
/// into it does nothing forever.
///
/// # The hazard this comes with
///
/// `russh` 0.62.6 skips the whole keepalive branch unless the session has reached
/// `EncryptedState::Authenticated`, *and does not rearm the timer when it skips it*
/// (`russh-0.62.6/src/client/mod.rs:1237-1249` for the branch, `:1321-1328` for the rearm, which is
/// conditional on having sent a keepalive or received data). An elapsed `tokio::time::Sleep` polls
/// ready forever, so from the moment this interval first elapses before authentication finishes
/// until it does finish, that session's select loop spins a core.
///
/// In practice that window is milliseconds: a password, a key or an agent all answer immediately.
/// It is only reachable through keyboard-interactive authentication with a person reading a token
/// off a device, which is why this is set well above how long the other methods take rather than as
/// low as detection alone would want.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// How long a closing shell waits to learn why its connection ended.
///
/// See [`SshConnection::open_shell`]. Bounded because the answer may never come: a connection closed
/// deliberately drops its notification rather than sending one.
const DEATH_REPORT_GRACE: Duration = Duration::from_millis(250);

/// How many unanswered keepalives before the connection is called dead.
///
/// `russh` increments its counter and then compares, so the error arrives on expiry number
/// `KEEPALIVE_MAX + 1` -- about two minutes with the interval above. Any inbound traffic at all
/// resets the counter, so this is a bound on silence rather than on unanswered probes.
const KEEPALIVE_MAX: usize = 3;

/// How a session ended, when it ended on its own.
#[derive(Debug)]
pub enum Death {
    /// The server sent `SSH_MSG_DISCONNECT`.
    ///
    /// Deliberate: an idle policy, an administrator, a session limit. Told apart from a transport
    /// failure because it is the one kind that must never be retried -- reconnecting after being
    /// asked to leave is how a client argues with an operator.
    ByServer {
        /// The server's own words for it.
        message: String,
    },
    /// The transport failed.
    ///
    /// A reconnect candidate, with one caveat worth remembering: `KeepaliveTimeout` also fires when
    /// a laptop wakes from sleep, and the session on the other end may still be alive. Reconnecting
    /// there orphans it.
    Transport(String),
}

impl std::fmt::Display for Death {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ByServer { message } if message.is_empty() => {
                f.write_str("the server closed the connection")
            }
            Self::ByServer { message } => write!(f, "the server closed the connection: {message}"),
            Self::Transport(detail) => write!(f, "the connection failed: {detail}"),
        }
    }
}

/// A client configuration with liveness detection turned on.
///
/// One function rather than two call sites, because a jump host whose link dies unnoticed is worse
/// than a destination whose link dies unnoticed: everything tunnelled through it dies with it, and
/// the failure surfaces at the far end as the destination hanging up.
fn keepalive_config() -> client::Config {
    client::Config {
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: KEEPALIVE_MAX,
        // Left off deliberately. It is a hard kill with no probe first, and `russh` rearms it on
        // outbound traffic as well as inbound -- so it measures "nothing happened at all", not "the
        // peer went quiet", and a session left at a prompt overnight would be closed for it.
        inactivity_timeout: None,
        ..client::Config::default()
    }
}

/// An authenticated SSH connection.
pub struct SshConnection {
    handle: Handle<Handler>,
    checker: HostKeyChecker,
    target: Target,
    /// Connections this one is carried over, outermost first.
    ///
    /// Held because dropping a link closes everything tunnelled through it. Without this the chain
    /// would collapse the moment the intermediate connections went out of scope, and the failure
    /// would look like the destination hanging up.
    via: Vec<SshConnection>,
    /// Where channels the *server* opens to us are delivered.
    ///
    /// Shared with the handler, which is the only thing `russh` gives a look at those channels.
    forwards: ForwardRegistry,
    /// Fires once, when the session ends by itself.
    ///
    /// A `Mutex<Option<..>>` because [`SshConnection::on_death`] takes `&self` -- the connection is
    /// behind an `Arc` by the time anybody wants to watch it -- and because there is exactly one
    /// death to hand out. The second caller gets `None`, which is honest: two things cannot both own
    /// the notification.
    death: Mutex<Option<oneshot::Receiver<Death>>>,
}

impl std::fmt::Debug for SshConnection {
    /// Written by hand: the target is useful in a test failure, and nothing else here should ever
    /// be printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshConnection")
            .field("host", &self.target.host)
            .field("port", &self.target.port)
            .field("user", &self.target.user)
            .finish_non_exhaustive()
    }
}

impl SshConnection {
    /// Connect, verify the host key, and authenticate.
    pub async fn connect(
        target: Target,
        auth: Auth,
        known_hosts: Arc<KnownHosts>,
        verifier: Arc<dyn HostKeyVerifier>,
    ) -> Result<Self, SshError> {
        let checker = HostKeyChecker::new(&target.host, target.port, known_hosts, verifier);
        let config = Arc::new(keepalive_config());
        let forwards = ForwardRegistry::default();
        let (died, death) = oneshot::channel();
        let handler = Handler {
            checker: checker.clone(),
            forwards: forwards.clone(),
            died: Some(died),
        };

        let mut handle =
            match client::connect(config, (target.host.as_str(), target.port), handler).await {
                Ok(handle) => handle,
                // `russh` turns a `check_server_key` refusal into exactly this error. Left as-is it
                // would reach the user as "ssh error: unknown key", which reads like a library
                // problem rather than "this server is not the one you trusted". The integration
                // tests caught this: the previous attempt to recover the distinction afterwards
                // never ran, because `?` had already returned.
                Err(SshError::Ssh(russh::Error::UnknownKey)) => {
                    return Err(SshError::HostKeyRejected);
                }
                // A refused or unreachable socket is not an SSH problem, and calling it one sends
                // whoever reads the message to check their SSH configuration when the actual answer is
                // that nothing is listening. `russh` wraps it, so it has to be unwrapped here.
                Err(SshError::Ssh(russh::Error::IO(error))) => return Err(SshError::Io(error)),
                Err(other) => return Err(other),
            };

        crate::auth::authenticate(&mut handle, &target.user, &auth).await?;

        tracing::info!(host = %target.host, port = target.port, user = %target.user, "ssh connected");

        Ok(Self {
            handle,
            checker,
            target,
            via: Vec::new(),
            forwards,
            death: Mutex::new(Some(death)),
        })
    }

    /// Take the notification that this session has ended.
    ///
    /// Available once. Awaiting it yields how the session died, or nothing if the task was torn down
    /// without going through `disconnected` -- which is what happens when the connection is dropped
    /// deliberately, and is precisely the case that must not look like a failure worth reconnecting
    /// from.
    pub fn on_death(&self) -> Option<oneshot::Receiver<Death>> {
        self.death.lock().ok()?.take()
    }

    /// Connect to `destination` through a chain of jump hosts, nearest first.
    ///
    /// Every hop is a full SSH connection: its own host key check, its own credentials. An empty
    /// chain is the same as [`SshConnection::connect`].
    ///
    /// The intermediate connections are kept inside the returned one, so the caller holds the whole
    /// chain by holding the destination.
    pub async fn connect_via(
        chain: Vec<Hop>,
        destination: Hop,
        known_hosts: Arc<KnownHosts>,
    ) -> Result<Self, SshError> {
        let mut via: Vec<SshConnection> = Vec::new();

        for hop in chain {
            let next = match via.last() {
                None => {
                    Self::connect(hop.target, hop.auth, known_hosts.clone(), hop.verifier).await?
                }
                Some(previous) => previous.connect_onward(hop, known_hosts.clone()).await?,
            };
            via.push(next);
        }

        let mut connection = match via.last() {
            None => {
                Self::connect(
                    destination.target,
                    destination.auth,
                    known_hosts.clone(),
                    destination.verifier,
                )
                .await?
            }
            Some(previous) => previous.connect_onward(destination, known_hosts).await?,
        };

        connection.via = via;
        Ok(connection)
    }

    /// Open an SSH connection to `hop` tunnelled through this one.
    async fn connect_onward(
        &self,
        hop: Hop,
        known_hosts: Arc<KnownHosts>,
    ) -> Result<Self, SshError> {
        let channel = self
            .handle
            .channel_open_direct_tcpip(
                hop.target.host.clone(),
                u32::from(hop.target.port),
                // The originator fields describe who asked for the forward. Servers log them; none
                // of them route anything, so loopback and zero are both truthful and uninformative.
                "127.0.0.1",
                0,
            )
            .await?;

        let checker =
            HostKeyChecker::new(&hop.target.host, hop.target.port, known_hosts, hop.verifier);
        let forwards = ForwardRegistry::default();
        let (died, death) = oneshot::channel();
        let handler = Handler {
            checker: checker.clone(),
            forwards: forwards.clone(),
            died: Some(died),
        };
        let config = Arc::new(keepalive_config());

        let mut handle = match client::connect_stream(config, channel.into_stream(), handler).await
        {
            Ok(handle) => handle,
            Err(SshError::Ssh(russh::Error::UnknownKey)) => {
                return Err(SshError::HostKeyRejected);
            }
            Err(SshError::Ssh(russh::Error::IO(error))) => return Err(SshError::Io(error)),
            Err(other) => return Err(other),
        };

        crate::auth::authenticate(&mut handle, &hop.target.user, &hop.auth).await?;

        tracing::info!(
            host = %hop.target.host,
            port = hop.target.port,
            through = %self.target.host,
            "ssh connected through a jump host"
        );

        Ok(Self {
            handle,
            checker,
            target: hop.target,
            via: Vec::new(),
            forwards,
            death: Mutex::new(Some(death)),
        })
    }

    /// What was decided about the server's host key.
    ///
    /// The caller writes the key to `known_hosts` when [`HostKeyOutcome::should_store`] says so; this
    /// crate does not touch the file.
    pub fn host_key_outcome(&self) -> Option<HostKeyOutcome> {
        self.checker.outcome()
    }

    /// Open a shell with a pseudo-terminal.
    pub async fn open_shell(&self, size: GridSize, term: &str) -> Result<OpenTransport, SshError> {
        let channel = self.handle.channel_open_session().await?;

        channel
            .request_pty(
                true,
                term,
                u32::from(size.cols),
                u32::from(size.rows),
                u32::from(size.pixel_width),
                u32::from(size.pixel_height),
                &[],
            )
            .await?;
        channel.request_shell(true).await?;

        let (mut read_half, write_half) = channel.split();
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let (commands_tx, mut commands_rx) = mpsc::unbounded_channel::<Command>();

        let label = format!("{}@{}", self.target.user, self.target.host);
        // Taken here rather than watched separately: a shell channel closing and its connection
        // dying look identical from inside the reader loop, and this is the only thing that can tell
        // them apart. Only the first shell on a connection gets it, which is right -- the second
        // would be reporting a death it does not own.
        let death = self.on_death();

        tokio::spawn(async move {
            let mut exit_code: Option<i32> = None;
            while let Some(message) = read_half.wait().await {
                match message {
                    // stderr is merged into the same stream: a terminal shows both, and separating
                    // them would reorder output that the remote program interleaved on purpose.
                    ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                        if events_tx
                            .send(TransportEvent::Output(data.to_vec()))
                            .is_err()
                        {
                            return;
                        }
                    }
                    ChannelMsg::ExitStatus { exit_status } => {
                        exit_code = Some(i32::try_from(exit_status).unwrap_or(-1));
                    }
                    ChannelMsg::Eof | ChannelMsg::Close => break,
                    _ => {}
                }
            }
            // The loop ends for two quite different reasons -- the remote shell exited, or the
            // connection underneath it died -- and `wait()` returns `None` for both. Without this,
            // a dropped network arrived at the interface as a shell that exited with no status,
            // which is the same thing it says when somebody types `exit`.
            //
            // Waited for, briefly, rather than polled: `disconnected` runs on the session task and
            // may not have got there yet when the channel's receiver closes. A quarter of a second
            // is far longer than that ordering needs and short enough that a clean exit does not
            // visibly pause.
            let message = match death {
                Some(death) => {
                    match tokio::time::timeout(DEATH_REPORT_GRACE, death).await {
                        Ok(Ok(death)) => Some(death.to_string()),
                        // Dropped without firing: the connection was closed on purpose. A clean
                        // shutdown, and it must not read as a failure.
                        Ok(Err(_)) | Err(_) => None,
                    }
                }
                None => None,
            };

            let _ = events_tx.send(TransportEvent::Closed(ExitInfo {
                code: exit_code,
                signal: None,
                message,
            }));
        });

        tokio::spawn(async move {
            while let Some(command) = commands_rx.recv().await {
                let outcome = match command {
                    Command::Data(bytes) => write_half.data_bytes(bytes).await,
                    Command::Resize(size) => {
                        write_half
                            .window_change(
                                u32::from(size.cols),
                                u32::from(size.rows),
                                u32::from(size.pixel_width),
                                u32::from(size.pixel_height),
                            )
                            .await
                    }
                    Command::Shutdown => {
                        let _ = write_half.eof().await;
                        let _ = write_half.close().await;
                        return;
                    }
                };
                if let Err(error) = outcome {
                    tracing::debug!(%error, "ssh channel write failed");
                    return;
                }
            }
        });

        Ok(OpenTransport {
            transport: Box::new(SshTransport {
                commands: commands_tx,
                size,
                label,
                closed: false,
            }),
            events: events_rx,
        })
    }

    /// Open a `direct-tcpip` channel: ask the server to connect somewhere on our behalf.
    ///
    /// The building block of both jump chains and local forwards.
    pub(crate) async fn open_direct_tcpip(
        &self,
        host: impl Into<String>,
        port: u16,
        originator_address: impl Into<String>,
        originator_port: u16,
    ) -> Result<russh::Channel<client::Msg>, SshError> {
        Ok(self
            .handle
            .channel_open_direct_tcpip(
                host.into(),
                u32::from(port),
                originator_address.into(),
                u32::from(originator_port),
            )
            .await?)
    }

    /// Ask the server to listen on `address:port` and send us what arrives.
    ///
    /// Port `0` asks the server to choose; the chosen port is returned. Delivery is arranged
    /// separately through the forward registry — this call only makes the request.
    pub(crate) async fn request_remote_forward(
        &self,
        address: &str,
        port: u16,
    ) -> Result<u16, SshError> {
        let allocated = self
            .handle
            .tcpip_forward(address.to_string(), u32::from(port))
            .await
            .map_err(|error| forward_denied(error, address, port))?;

        // The server returns the allocated port only when one was asked to be chosen; otherwise it
        // returns 0 and the port is the one we named.
        if port != 0 {
            return Ok(port);
        }
        u16::try_from(allocated).map_err(|_| SshError::ImpossiblePort(allocated))
    }

    /// Ask the server to stop listening.
    pub(crate) async fn cancel_remote_forward(
        &self,
        address: &str,
        port: u16,
    ) -> Result<(), SshError> {
        let cancelled = self
            .handle
            .cancel_tcpip_forward(address.to_string(), u32::from(port))
            .await;
        cancelled.map_err(|error| forward_denied(error, address, port))
    }

    /// Where server-opened channels are delivered.
    pub(crate) fn forwards(&self) -> &ForwardRegistry {
        &self.forwards
    }

    /// Close the connection and every channel on it.
    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "", "en")
            .await;
    }
}

/// Name what a refused forward request was refused *for*.
///
/// `russh` reports the refusal as a bare `RequestDenied`, which on its own tells a user nothing they
/// can act on. The server does not say why either, so the message names the address and port and
/// leaves the diagnosis — a privileged port, one already taken, `GatewayPorts no` — to whoever can
/// look at the server.
fn forward_denied(error: russh::Error, address: &str, port: u16) -> SshError {
    match error {
        russh::Error::RequestDenied => SshError::ForwardDenied {
            address: address.to_string(),
            port,
        },
        other => SshError::Ssh(other),
    }
}

enum Command {
    Data(Vec<u8>),
    Resize(GridSize),
    Shutdown,
}

/// A shell channel presented as a [`Transport`].
struct SshTransport {
    commands: mpsc::UnboundedSender<Command>,
    size: GridSize,
    label: String,
    closed: bool,
}

impl Transport for SshTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Ssh
    }

    fn write(&mut self, data: &[u8]) -> TransportResult<()> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        self.commands
            .send(Command::Data(data.to_vec()))
            .map_err(|_| TransportError::Closed)
    }

    fn resize(&mut self, size: GridSize) -> TransportResult<()> {
        if size == self.size {
            return Ok(());
        }
        self.commands
            .send(Command::Resize(size))
            .map_err(|_| TransportError::Closed)?;
        self.size = size;
        Ok(())
    }

    fn size(&self) -> GridSize {
        self.size
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        // The peer may already be gone, which is the ordinary case here rather than a failure.
        let _ = self.commands.send(Command::Shutdown);
        Ok(())
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

/// Bridges `russh`'s callbacks to the host key checker and to remote forwards.
pub(crate) struct Handler {
    checker: HostKeyChecker,
    forwards: ForwardRegistry,
    /// Taken on the one call `disconnected` ever gets.
    ///
    /// `Option` because the sender is consumed by sending, and `disconnected` has `&mut self` rather
    /// than `self` -- so the type has to say "this happens once" where the signature will not.
    died: Option<oneshot::Sender<Death>>,
}

impl client::Handler for Handler {
    type Error = SshError;

    /// The only place the concrete reason a session ended is ever visible.
    ///
    /// `russh` calls this with the real error and then, if the override returns `Ok`, replaces it
    /// with a generic `Disconnect` for whoever awaits the session task
    /// (`russh-0.62.6/src/client/mod.rs:1152-1167`). Nothing in BestTerm awaits that task, so
    /// without this the reason a connection died was constructed and then thrown away: every kind of
    /// death arrived at the interface as a shell that had exited with no status.
    fn disconnected(
        &mut self,
        reason: client::DisconnectReason<Self::Error>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        let died = self.died.take();
        async move {
            let death = match reason {
                client::DisconnectReason::ReceivedDisconnect(info) => {
                    tracing::info!(
                        code = ?info.reason_code,
                        message = %info.message,
                        "ssh: the server disconnected us"
                    );
                    Death::ByServer {
                        message: info.message,
                    }
                }
                client::DisconnectReason::Error(error) => {
                    tracing::warn!(%error, "ssh: the connection failed");
                    Death::Transport(error.to_string())
                }
            };

            // Nobody listening is normal: a connection closed on purpose is dropped along with its
            // receiver, and that is exactly the case that must not be reported as a failure.
            if let Some(died) = died {
                let _ = died.send(death);
            }

            // Returning `Ok` costs the session task's own error, which is fine *because* the reason
            // was captured above. Doing both -- re-returning the error and sending it -- would be
            // two ways to learn one thing, and they would disagree the moment one of them changed.
            Ok(())
        }
    }

    fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        // Converted before the async block so the future owns everything and borrows neither the key
        // nor `self`.
        let converted = host_key_from_ssh(server_public_key);
        let checker = self.checker.clone();
        async move {
            let key = converted.map_err(|_| SshError::UnreadableHostKey)?;
            Ok(checker.check(&key))
        }
    }

    /// Someone connected to a port the server is holding open for us.
    ///
    /// This runs on the session's event loop, so it does nothing that can block: the channel and the
    /// unanswered `reply` are handed to whichever forward owns the port, and that forward decides —
    /// off this task — whether the local connection can be made. Answering here instead would stall
    /// every other channel on the session behind one slow local connect.
    #[allow(clippy::too_many_arguments)]
    fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        let sink = self.forwards.sink(connected_port);
        // Formatted eagerly: the future must not borrow the parameters.
        let connected = format!("{connected_address}:{connected_port}");
        let originator = format!("{originator_address}:{originator_port}");

        async move {
            let handed_over = match sink {
                Some(sink) => sink.send(Incoming { channel, reply }).is_ok(),
                None => false,
            };

            if !handed_over {
                // Reached when a forward was dropped between the server accepting a connection and
                // this arriving. Nothing is wrong; the far end sees a refusal. `Incoming` was not
                // sent, so `reply` was dropped, which `russh` turns into exactly this rejection.
                tracing::debug!(
                    %connected,
                    %originator,
                    "declined a forwarded connection nobody is listening for"
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Credential redaction is tested next to the type it belongs to, in `auth`.

    #[test]
    fn a_host_key_rejection_reads_differently_from_an_auth_failure() {
        // The two are told apart on purpose: one means the server is not who it claimed, the other
        // means the credential was wrong, and sending a user to check the wrong one wastes their day.
        let rejected = SshError::HostKeyRejected.to_string();
        let failed = SshError::AuthenticationFailed {
            remaining: vec!["publickey".to_string()],
        }
        .to_string();

        assert!(rejected.contains("host key"), "got {rejected}");
        assert!(failed.contains("authentication"), "got {failed}");
        assert_ne!(rejected, failed);
    }

    #[test]
    fn a_further_factor_is_not_reported_as_a_failure() {
        let message = SshError::FurtherAuthenticationRequired {
            remaining: vec!["keyboard-interactive".to_string()],
        }
        .to_string();
        assert!(
            message.contains("another authentication factor"),
            "got {message}"
        );
        assert!(!message.contains("failed"), "got {message}");
    }

    #[test]
    fn the_failure_message_lists_what_the_server_will_still_take() {
        let message = SshError::AuthenticationFailed {
            remaining: vec!["publickey".to_string(), "keyboard-interactive".to_string()],
        }
        .to_string();
        assert!(message.contains("publickey"), "got {message}");
        assert!(message.contains("keyboard-interactive"), "got {message}");
    }
}
