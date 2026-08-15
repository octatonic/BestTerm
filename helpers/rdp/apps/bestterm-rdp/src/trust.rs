//! Deciding whether the server that answered is the one that was meant.
//!
//! The helper does not own this decision and must not. The store of known keys is configuration, it
//! lives with the host, and the person who can actually answer "is that the right fingerprint" is
//! sitting in front of the host's window. What the helper owns is the *moment*: the only point at
//! which the question can still be asked usefully is after TLS comes up and before the credential
//! goes out, and that moment is inside a handshake this process is running.
//!
//! So the question travels. The helper writes [`HelperMessage::AskAboutServerKey`] to its parent and
//! blocks the handshake until the answer comes back as a `HostMessage::ServerKeyAnswer`.
//!
//! # Why it blocks a thread
//!
//! [`Verifier::decide`] is synchronous, because it is called from inside IronRDP's state machine.
//! The wait therefore happens under [`tokio::task::block_in_place`], which moves the rest of the
//! runtime onto another worker rather than stalling it — the same shape the SSH side uses for the
//! same reason.
//!
//! # No answer is a refusal
//!
//! The wait has a timeout, and running out of it rejects. A parent that has crashed, or a window
//! nobody is looking at, must not become an accepted key: silence is the one answer that can be
//! produced without a person, so it is the one that has to be safe.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bestterm_ipc_frame::HelperMessage;
use bestterm_proto_rdp::server_key::{Decision, KeyFingerprint, Verdict, Verifier};

use crate::Reporter;

/// How long a question waits before it is treated as a refusal.
///
/// Long enough for somebody to walk back to their desk, read a fingerprint off a screen and compare
/// it. Short enough that a helper whose parent died does not sit on a socket forever.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(180);

/// Asks the host, and waits.
pub(crate) struct AskingVerifier {
    /// How the question gets out. Shared with the session loop, which is why it is behind a lock —
    /// though in practice they never overlap: no frame exists until the key is settled.
    out: Arc<Mutex<Reporter>>,
    /// Where answers arrive. One at a time, and only ever in reply to a question.
    answers: Mutex<Receiver<bool>>,
}

impl AskingVerifier {
    /// Build one over an already-running command reader.
    pub(crate) fn new(out: Arc<Mutex<Reporter>>, answers: Receiver<bool>) -> Self {
        Self {
            out,
            answers: Mutex::new(answers),
        }
    }
}

impl Verifier for AskingVerifier {
    fn decide(
        &self,
        host: &str,
        port: u16,
        presented: KeyFingerprint,
        verdict: &Verdict,
    ) -> Decision {
        // Only the unsettled verdicts reach a verifier at all; `Trusted` never does. `Revoked` does,
        // and is refused here rather than passed on: a revoked key is a decision somebody already
        // made, and asking again is how it gets undone by accident.
        let expected = match verdict {
            Verdict::Revoked => {
                tracing::warn!(host, port, "rdp: the server offered a revoked key");
                return Decision::Reject;
            }
            Verdict::Changed { expected } => Some(expected.to_string()),
            Verdict::Trusted | Verdict::Unknown => None,
        };

        let question = HelperMessage::AskAboutServerKey {
            host: host.to_string(),
            port,
            fingerprint: presented.to_string(),
            expected,
        };

        // `block_in_place` and not a plain blocking wait: this runs on a runtime worker, and holding
        // one for three minutes would stop everything else on it.
        tokio::task::block_in_place(|| {
            {
                let Ok(mut out) = self.out.lock() else {
                    tracing::error!("rdp: cannot reach the host to ask about a key");
                    return Decision::Reject;
                };
                out.send(&question);
                out.flush();
            }

            let Ok(answers) = self.answers.lock() else {
                return Decision::Reject;
            };
            match answers.recv_timeout(ANSWER_TIMEOUT) {
                Ok(true) => {
                    // Stored, not accepted once. The host asked a person, and the point of asking is
                    // that they are not asked again for the same machine. What actually gets written
                    // is the host's business; this only says the key is worth writing.
                    Decision::AcceptAndStore
                }
                Ok(false) => {
                    tracing::info!(host, port, "rdp: the host refused the server's key");
                    Decision::Reject
                }
                Err(_) => {
                    tracing::warn!(host, port, "rdp: nobody answered about the server's key");
                    Decision::Reject
                }
            }
        })
    }
}
