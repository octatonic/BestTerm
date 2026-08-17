//! Reopening a session that died.
//!
//! # A reconnect is not a retry
//!
//! What is being retried is *authentication to a host named by a string*, and that string is resolved
//! afresh on every attempt. Between the connection that died and the one replacing it, DNS,
//! `/etc/hosts`, DHCP, a VPN coming up or going down, or somebody with control of a resolver can point
//! the name at a different machine. A reconnect that then offered the password or the private key to
//! whatever answered — unattended, with nobody reading a fingerprint — would hand the credential to
//! the wrong host, and it would look exactly like a network hiccup recovering.
//!
//! So the question a reconnect asks is not "does `known_hosts` trust this key?" but "is this the same
//! machine I was talking to a moment ago?". Those differ precisely when it matters. Re-running the
//! `known_hosts` policy would also re-run its *prompt*, and a key accepted by prompt during this
//! session is not in the snapshot the connection was verifying against — so a reconnect would ask
//! again, and training somebody to click through a host key dialog on every network blip is the exact
//! failure host key checking exists to prevent.
//!
//! [`PinnedVerifier`] answers the second question and refuses to answer the first. A mismatch is
//! fatal, never a prompt.
//!
//! # What cannot be carried over
//!
//! Nothing about the old session survives. `russh` has no resumption of any kind, so a reconnect is a
//! fresh handshake and a fresh shell: the working directory, the shell's history, whatever program was
//! running, the environment and the scrollback are all gone. That has to be visible to whoever asked
//! for it rather than papered over — a terminal that silently comes back empty looks like it crashed.
//!
//! The credential does not survive either. [`crate::transport::SshConnection::connect`] takes an
//! [`crate::Auth`] by value and drops it, which is deliberate; a caller that wants to reconnect has to
//! have kept its own copy. For a keyboard-interactive login that is not possible at all — the answer
//! was a one-time code — and [`Reconnectable::of`] says so rather than letting the attempt fail later
//! for a reason nobody would connect to the cause.
//!
//! # What must not be retried
//!
//! A server-sent disconnect. An idle policy, an administrator, a session limit: all deliberate acts,
//! and a client that reconnects after being asked to leave is a client arguing with an operator.
//! [`crate::transport::Death`] already tells the two apart, and [`should_retry`] is where that
//! distinction is spent.

use std::sync::Arc;

use crate::auth::Auth;
use crate::host_key::{HostKeyDecision, HostKeyVerifier};
use crate::known_hosts::{HostKey, Verdict};
use crate::transport::Death;

/// Accepts one key and nothing else.
///
/// The key is the one observed on the connection being replaced. `known_hosts` is not consulted: this
/// is not asking whether the host is trusted, it is asking whether it is the same host.
#[derive(Clone, Debug)]
pub struct PinnedVerifier {
    /// The exact key the previous connection presented.
    expected: HostKey,
}

impl PinnedVerifier {
    /// Pin `expected`.
    pub fn new(expected: HostKey) -> Self {
        Self { expected }
    }

    /// The key this will accept.
    pub fn expected(&self) -> &HostKey {
        &self.expected
    }
}

impl HostKeyVerifier for PinnedVerifier {
    fn decide(&self, host: &str, port: u16, key: &HostKey, _verdict: &Verdict) -> HostKeyDecision {
        if key == &self.expected {
            // Never `AcceptAndStore`. A reconnect learns nothing new about a host -- the key it
            // accepted is the key it already had -- so writing it down would only add a line saying
            // what the previous line said.
            return HostKeyDecision::Accept;
        }

        tracing::warn!(
            host,
            port,
            expected = %self.expected.algorithm,
            presented = %key.algorithm,
            "reconnect: the host key changed; refusing"
        );
        HostKeyDecision::Reject
    }
}

/// Whether a death is worth reconnecting from.
///
/// Deliberate closures are not. Beyond that this is a judgement about intent rather than about
/// networks, which is why it lives beside the pinning rather than inside the transport.
pub fn should_retry(death: &Death) -> bool {
    match death {
        // The operator meant it. Reconnecting is arguing.
        Death::ByServer { .. } => false,
        Death::Transport(_) => true,
    }
}

/// Why an automatic reconnect is impossible for a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotReconnectable {
    /// The credential was a one-time answer from a person.
    ///
    /// A keyboard-interactive login cannot be replayed: the code has been used, and the next one has
    /// to come from whoever holds the device.
    Interactive,
}

impl std::fmt::Display for NotReconnectable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interactive => f.write_str(
                "this session was opened with a one-time code, which cannot be used twice",
            ),
        }
    }
}

/// Everything needed to open the replacement, and the pin that makes it safe.
///
/// Held by the caller for the life of the session, because none of it can be recovered from a dead
/// connection: the credential was consumed by the handshake, and the key is only knowable while the
/// connection that saw it still exists.
pub struct Reconnectable {
    /// The credential, kept so it can be offered again.
    pub auth: Auth,
    /// What the previous connection's server presented.
    pub verifier: Arc<PinnedVerifier>,
}

impl std::fmt::Debug for Reconnectable {
    /// By hand, and without the credential. `Auth` redacts itself, but a derived implementation would
    /// print its field name next to whatever somebody adds beside it later.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reconnectable")
            .field("pinned", &self.verifier.expected().algorithm)
            .finish_non_exhaustive()
    }
}

impl Reconnectable {
    /// Prepare to reconnect, given the credential and the key that was observed.
    ///
    /// Refuses the credentials that cannot be offered a second time, so a session says up front that
    /// it will not come back rather than failing later in a way nobody would trace to the cause.
    pub fn of(auth: Auth, observed: HostKey) -> Result<Self, NotReconnectable> {
        if matches!(auth, Auth::KeyboardInteractive(_)) {
            return Err(NotReconnectable::Interactive);
        }
        Ok(Self {
            auth,
            verifier: Arc::new(PinnedVerifier::new(observed)),
        })
    }

    /// The verifier to hand to a fresh connection.
    ///
    /// A fresh [`crate::host_key::HostKeyChecker`] must be built around it rather than reusing the old
    /// one: `check` overwrites its recorded outcome, so a reused checker would silently replace the
    /// record of the key somebody approved with whatever the new server offered, and
    /// `host_key_outcome` would then report the new key as the accepted one.
    pub fn verifier(&self) -> Arc<dyn HostKeyVerifier> {
        Arc::clone(&self.verifier) as Arc<dyn HostKeyVerifier>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(algorithm: &str, byte: u8) -> HostKey {
        HostKey::new(algorithm, vec![byte; 32])
    }

    #[test]
    fn the_same_key_is_accepted_and_not_written_down_again() {
        let pinned = PinnedVerifier::new(key("ssh-ed25519", 1));
        let decision = pinned.decide("h", 22, &key("ssh-ed25519", 1), &Verdict::Unknown);

        assert_eq!(decision, HostKeyDecision::Accept);
        assert_ne!(
            decision,
            HostKeyDecision::AcceptAndStore,
            "a reconnect learns nothing new about a host, so it records nothing"
        );
    }

    #[test]
    fn a_different_key_is_fatal_and_never_a_question() {
        // The whole point. A prompt here would be a prompt nobody is watching, on every network blip,
        // for a machine that may not be the one the password belongs to.
        let pinned = PinnedVerifier::new(key("ssh-ed25519", 1));
        for presented in [key("ssh-ed25519", 2), key("ssh-rsa", 1)] {
            assert_eq!(
                pinned.decide("h", 22, &presented, &Verdict::Unknown),
                HostKeyDecision::Reject
            );
        }
    }

    #[test]
    fn a_trusted_verdict_does_not_override_the_pin() {
        // The difference between the two questions, in one assertion: `known_hosts` may well trust the
        // key a *different* machine is presenting, because trust is recorded per address and the
        // address is what moved. Only the pin catches that.
        let pinned = PinnedVerifier::new(key("ssh-ed25519", 1));
        assert_eq!(
            pinned.decide("h", 22, &key("ssh-ed25519", 9), &Verdict::Trusted),
            HostKeyDecision::Reject
        );
    }

    #[test]
    fn a_revoked_verdict_does_not_override_the_pin_either() {
        // The other direction, which is subtler: the pinned key is the one this session has been
        // talking to all along, so refusing it because the file was edited mid-session would drop a
        // working connection on a decision nobody applied to it. Continuing is right; the file is
        // honoured on the next *deliberate* connect, where a person is watching.
        let pinned = PinnedVerifier::new(key("ssh-ed25519", 1));
        assert_eq!(
            pinned.decide("h", 22, &key("ssh-ed25519", 1), &Verdict::Revoked),
            HostKeyDecision::Accept
        );
    }

    #[test]
    fn a_server_that_sent_us_away_is_not_reconnected_to() {
        assert!(!should_retry(&Death::ByServer {
            message: "idle timeout".to_string()
        }));
        assert!(!should_retry(&Death::ByServer {
            message: String::new()
        }));
        assert!(should_retry(&Death::Transport(
            "Keepalive timeout".to_string()
        )));
    }

    #[test]
    fn a_one_time_code_cannot_be_offered_twice() {
        // Said now rather than discovered later: an automatic reconnect using a used OTP fails at
        // authentication, which reads as a wrong password and sends somebody to check the wrong thing.
        struct Silent;
        impl crate::auth::PromptResponder for Silent {
            fn respond(
                &self,
                _name: &str,
                _instructions: &str,
                _prompts: &[crate::auth::InteractivePrompt],
            ) -> Option<Vec<bestterm_core_vault::Secret>> {
                None
            }
        }

        let interactive = Auth::KeyboardInteractive(Arc::new(Silent));
        assert_eq!(
            Reconnectable::of(interactive, key("ssh-ed25519", 1)).err(),
            Some(NotReconnectable::Interactive)
        );
    }

    #[test]
    fn an_agent_or_a_password_can_be_offered_again() {
        for auth in [
            Auth::Agent,
            Auth::Password(bestterm_core_vault::Secret::new("x".to_string())),
        ] {
            let ready = Reconnectable::of(auth, key("ssh-ed25519", 7)).expect("reconnectable");
            assert_eq!(ready.verifier.expected(), &key("ssh-ed25519", 7));

            // And the pin travels as the verifier a fresh connection will consult.
            let verifier = ready.verifier();
            assert_eq!(
                verifier.decide("h", 22, &key("ssh-ed25519", 7), &Verdict::Unknown),
                HostKeyDecision::Accept
            );
            assert_eq!(
                verifier.decide("h", 22, &key("ssh-ed25519", 8), &Verdict::Unknown),
                HostKeyDecision::Reject
            );
        }
    }

    #[test]
    fn the_debug_form_carries_no_credential() {
        let ready = Reconnectable::of(
            Auth::Password(bestterm_core_vault::Secret::new("hunter2".to_string())),
            key("ssh-ed25519", 1),
        )
        .expect("reconnectable");
        let printed = format!("{ready:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
    }
}
