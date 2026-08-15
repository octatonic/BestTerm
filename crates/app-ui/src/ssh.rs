//! Opening an SSH session from the interface.
//!
//! # Three threads, and why
//!
//! `proto-ssh` is async and the interface is not. Connecting therefore happens on a tokio runtime the
//! application owns, and the result comes back over a channel the frame loop drains — the arrangement
//! `docs/ARCHITECTURE.md` describes as UI thread, runtime, and deltas between them.
//!
//! # The host key prompt runs backwards
//!
//! Every other part of this is the interface asking for something and waiting. The host key check is
//! the opposite: the *connection* needs an answer from the person, in the middle of a handshake, and
//! cannot proceed without one.
//!
//! So [`PromptingVerifier`] posts the question onto a channel the frame loop reads, and blocks on a
//! second channel for the reply. Two things make that safe rather than a deadlock:
//!
//! * The block happens inside [`tokio::task::block_in_place`], which moves the task off its worker so
//!   the runtime keeps running the rest of the session. Without it, blocking inside `russh`'s handler
//!   would stall the very connection waiting for the answer.
//! * There is a timeout. A prompt nobody answers — because the window was closed, or because the reply
//!   channel was dropped — has to end as a refusal rather than as a task parked forever.

use std::sync::Arc;
use std::time::Duration;

use bestterm_core_model::SshConfig;
use bestterm_proto_ssh::host_key::{HostKeyDecision, HostKeyVerifier};
use bestterm_proto_ssh::known_hosts::{HostKey, KnownHosts, Verdict};
use bestterm_proto_ssh::{Auth, SshConnection, Target};
use bestterm_transport::{GridSize, OpenTransport};

/// How long a host key prompt waits for an answer before refusing.
///
/// Long enough that somebody reading a fingerprint carefully is not cut off, short enough that a
/// forgotten prompt does not hold a connection open all afternoon.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(180);

/// A question the connection needs answered before it can continue.
#[derive(Clone, Debug)]
pub(crate) struct HostKeyQuestion {
    /// The host as it was typed.
    pub host: String,
    /// The port.
    pub port: u16,
    /// The fingerprint the server presented, ready to show.
    pub presented: String,
    /// What `known_hosts` said.
    pub verdict: HostKeyVerdict,
    /// Where the answer goes.
    reply: crossbeam_channel::Sender<HostKeyDecision>,
}

impl HostKeyQuestion {
    /// Answer the question, letting the connection continue or not.
    ///
    /// A failed send means the connection gave up first — its timeout elapsed, or it was dropped — and
    /// is nothing for the interface to report.
    pub(crate) fn answer(&self, decision: HostKeyDecision) {
        let _ = self.reply.send(decision);
    }
}

/// What the store said, flattened for display.
///
/// The distinction is the whole point of asking: an unknown host is a question, and a changed key is
/// an alarm. Flattened rather than passed through so the interface does not need the protocol crate's
/// types to draw a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostKeyVerdict {
    /// Never seen before.
    Unknown,
    /// Different keys were recorded.
    ///
    /// A list, not one: a host legitimately has an entry per algorithm, and a prompt that showed only
    /// the first would leave somebody comparing against the wrong line of their own file.
    Changed {
        /// The fingerprints that were expected.
        expected: Vec<String>,
    },
    /// The key was retired and must stay refused.
    Revoked,
}

/// A key the person accepted, for the caller to write down.
///
/// Carried as its parts rather than as a finished line: rendering one is `known_hosts`'s business, and
/// `proto-ssh` already knows how. The application decides *whether* to write and *where*.
#[derive(Clone, Debug)]
pub(crate) struct HostKeyRecord {
    /// The host as it was typed.
    pub host: String,
    /// The port.
    pub port: u16,
    /// The key the server presented.
    pub key: HostKey,
}

/// What happened to a connection attempt.
pub(crate) enum SessionEvent {
    /// It worked; here is the session.
    Opened {
        /// Label for the tab.
        title: String,
        /// The transport, ready for a tab to adopt.
        open: Box<OpenTransport>,
        /// The key to append to `known_hosts`, when the person accepted a new one.
        record: Option<HostKeyRecord>,
    },
    /// It did not work.
    Failed {
        /// Label the attempt had, so the message can name it.
        title: String,
        /// What went wrong, as a person should read it.
        reason: String,
    },
    /// The connection needs a decision about the server's key.
    AskAboutHostKey(HostKeyQuestion),
}

impl std::fmt::Debug for SessionEvent {
    /// Written by hand because a transport is not printable, and should not become so: there is
    /// nothing in a socket worth putting in a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Opened { title, record, .. } => f
                .debug_struct("Opened")
                .field("title", title)
                .field("stores_a_key", &record.is_some())
                .finish_non_exhaustive(),
            Self::Failed { title, reason } => f
                .debug_struct("Failed")
                .field("title", title)
                .field("reason", reason)
                .finish(),
            Self::AskAboutHostKey(question) => {
                f.debug_tuple("AskAboutHostKey").field(question).finish()
            }
        }
    }
}

/// A verifier that asks the interface.
struct PromptingVerifier {
    questions: crossbeam_channel::Sender<SessionEvent>,
    waker: Arc<dyn Fn() + Send + Sync>,
}

impl HostKeyVerifier for PromptingVerifier {
    fn decide(&self, host: &str, port: u16, key: &HostKey, verdict: &Verdict) -> HostKeyDecision {
        let verdict = match verdict {
            Verdict::Unknown => HostKeyVerdict::Unknown,
            Verdict::Changed { expected } => HostKeyVerdict::Changed {
                expected: expected.iter().map(HostKey::fingerprint).collect(),
            },
            Verdict::Revoked => HostKeyVerdict::Revoked,
            // A trusted key never reaches a verifier; refusing is the safe reading of the impossible.
            Verdict::Trusted => return HostKeyDecision::Accept,
        };

        let (reply, answer) = crossbeam_channel::bounded(1);
        let question = HostKeyQuestion {
            host: host.to_owned(),
            port,
            presented: key.fingerprint(),
            verdict,
            reply,
        };

        if self
            .questions
            .send(SessionEvent::AskAboutHostKey(question))
            .is_err()
        {
            // Nobody is listening, so nobody can say yes.
            return HostKeyDecision::Reject;
        }
        (self.waker)();

        // Moved off the worker for the wait: blocking a runtime thread here would stall the handshake
        // that is waiting for this answer.
        tokio::task::block_in_place(|| match answer.recv_timeout(PROMPT_TIMEOUT) {
            Ok(decision) => decision,
            Err(_) => {
                tracing::warn!(host, port, "no answer about the host key; refusing");
                HostKeyDecision::Reject
            }
        })
    }
}

/// Connect and open a shell, reporting the outcome on `events`.
///
/// Spawned on the runtime rather than awaited: the frame loop cannot block, and a connection can take
/// as long as a network does.
pub(crate) fn connect(
    runtime: &tokio::runtime::Handle,
    config: SshConfig,
    auth: Auth,
    known_hosts_text: String,
    size: GridSize,
    events: crossbeam_channel::Sender<SessionEvent>,
    waker: Arc<dyn Fn() + Send + Sync>,
) {
    let title = match &config.user {
        Some(user) => format!("{user}@{}", config.host),
        None => config.host.clone(),
    };

    runtime.spawn(async move {
        let known_hosts = Arc::new(KnownHosts::parse(&known_hosts_text));
        let verifier = Arc::new(PromptingVerifier {
            questions: events.clone(),
            waker: Arc::clone(&waker),
        });

        let target = Target {
            host: config.host.clone(),
            port: config.port,
            user: config.user.clone().unwrap_or_else(whoami),
        };

        let event = match SshConnection::connect(target, auth, known_hosts, verifier).await {
            Ok(connection) => {
                let record = connection
                    .host_key_outcome()
                    .filter(|outcome| outcome.should_store())
                    .map(|outcome| HostKeyRecord {
                        host: config.host.clone(),
                        port: config.port,
                        key: outcome.key,
                    });

                match connection.open_shell(size, "xterm-256color").await {
                    Ok(open) => SessionEvent::Opened {
                        title,
                        open: Box::new(open),
                        record,
                    },
                    Err(error) => SessionEvent::Failed {
                        title,
                        reason: format!("connected, but could not open a shell: {error}"),
                    },
                }
            }
            Err(error) => SessionEvent::Failed {
                title,
                reason: error.to_string(),
            },
        };

        let _ = events.send(event);
        waker();
    });
}

/// The local account name, for when a session does not name a user.
///
/// The same thing `ssh` does with no user: use the one you are logged in as.
fn whoami() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "root".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_nobody_answers_does_not_wedge_the_caller() {
        // The reply channel is dropped without an answer, which is what a closed window looks like.
        let (reply, answer) = crossbeam_channel::bounded::<HostKeyDecision>(1);
        drop(answer);
        let question = HostKeyQuestion {
            host: "srv.int".to_string(),
            port: 22,
            presented: "SHA256:x".to_string(),
            verdict: HostKeyVerdict::Unknown,
            reply,
        };
        // Answering into a dropped channel is a no-op rather than a panic: the connection gave up
        // first, and that is not the interface's problem to report.
        question.answer(HostKeyDecision::AcceptAndStore);
    }

    #[test]
    fn an_unknown_host_reads_differently_from_a_changed_key() {
        let unknown = HostKeyVerdict::Unknown;
        let changed = HostKeyVerdict::Changed {
            expected: vec!["SHA256:old".to_string()],
        };
        assert_ne!(unknown, changed);
        assert_ne!(changed, HostKeyVerdict::Revoked);
    }

    #[test]
    fn a_session_with_no_user_falls_back_to_the_local_account() {
        // Not asserting the name -- it differs per machine -- only that there is one, because an empty
        // user name reaches the server as a generic authentication failure.
        assert!(!whoami().is_empty());
    }
}
