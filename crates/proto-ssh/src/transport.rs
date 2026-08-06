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

use std::sync::Arc;

use bestterm_transport::{
    ExitInfo, GridSize, OpenTransport, Result as TransportResult, Transport, TransportError,
    TransportEvent, TransportKind,
};
use russh::client::{self, Handle};
use russh::keys::ssh_key;
use russh::{ChannelMsg, Disconnect};
use tokio::sync::mpsc;

use crate::auth::Auth;
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
    via: Vec<Box<SshConnection>>,
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
        let config = Arc::new(client::Config::default());
        let handler = Handler {
            checker: checker.clone(),
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
                Err(other) => return Err(other),
            };

        crate::auth::authenticate(&mut handle, &target.user, &auth).await?;

        tracing::info!(host = %target.host, port = target.port, user = %target.user, "ssh connected");

        Ok(Self {
            handle,
            checker,
            target,
            via: Vec::new(),
        })
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
        let mut via: Vec<Box<SshConnection>> = Vec::new();

        for hop in chain {
            let next = match via.last() {
                None => {
                    Self::connect(hop.target, hop.auth, known_hosts.clone(), hop.verifier).await?
                }
                Some(previous) => previous.connect_onward(hop, known_hosts.clone()).await?,
            };
            via.push(Box::new(next));
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
        let handler = Handler {
            checker: checker.clone(),
        };
        let config = Arc::new(client::Config::default());

        let mut handle = match client::connect_stream(config, channel.into_stream(), handler).await
        {
            Ok(handle) => handle,
            Err(SshError::Ssh(russh::Error::UnknownKey)) => {
                return Err(SshError::HostKeyRejected);
            }
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
            let _ = events_tx.send(TransportEvent::Closed(ExitInfo {
                code: exit_code,
                signal: None,
                message: None,
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

    /// Close the connection and every channel on it.
    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "", "en")
            .await;
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

/// Bridges `russh`'s callbacks to the host key checker.
pub(crate) struct Handler {
    checker: HostKeyChecker,
}

impl client::Handler for Handler {
    type Error = SshError;

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_never_reaches_a_log_through_debug() {
        let auth = Auth::Password("hunter2".to_string());
        let printed = format!("{auth:?}");
        assert_eq!(printed, "Password(<redacted>)");
        assert!(!printed.contains("hunter2"));
    }

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
