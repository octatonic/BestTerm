//! Deciding what to do about a server's host key.
//!
//! [`known_hosts`](crate::known_hosts) works out *what the situation is*; this module is about *who
//! decides what to do* — and keeps that decision out of the connection code, so the policy can be
//! "ask the user", "accept nothing new" or a fixed answer in a test, without any of them knowing
//! about the others.

use std::sync::Arc;

use russh::keys::ssh_key;

use crate::known_hosts::{HostKey, KnownHosts, Verdict};

/// What to do with a key the server presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// Continue, but do not write anything down.
    Accept,
    /// Continue, and record the key so it is recognised next time.
    AcceptAndStore,
    /// Refuse the connection.
    Reject,
}

/// Who decides.
///
/// Implemented by the UI to raise a prompt, and by [`StrictVerifier`] for the unattended case.
pub trait HostKeyVerifier: Send + Sync {
    /// Decide, given what the file says.
    ///
    /// Called from the SSH handshake, so it must return rather than wait indefinitely; a UI
    /// implementation posts a prompt and blocks on the answer with a timeout.
    fn decide(&self, host: &str, port: u16, key: &HostKey, verdict: &Verdict) -> HostKeyDecision;
}

/// Accepts only keys `known_hosts` already records.
///
/// The right policy for anything running unattended, and the safe default: an unknown host is a
/// refusal rather than a silent yes.
#[derive(Clone, Copy, Debug, Default)]
pub struct StrictVerifier;

impl HostKeyVerifier for StrictVerifier {
    fn decide(
        &self,
        _host: &str,
        _port: u16,
        _key: &HostKey,
        verdict: &Verdict,
    ) -> HostKeyDecision {
        match verdict {
            Verdict::Trusted => HostKeyDecision::Accept,
            _ => HostKeyDecision::Reject,
        }
    }
}

/// Accepts anything, for tests only.
///
/// Never wire this to a real connection: it is exactly the behaviour that makes host key checking
/// pointless. It is named to be obvious in a diff.
#[derive(Clone, Copy, Debug, Default)]
pub struct AcceptAnyVerifierForTests;

impl HostKeyVerifier for AcceptAnyVerifierForTests {
    fn decide(
        &self,
        _host: &str,
        _port: u16,
        _key: &HostKey,
        _verdict: &Verdict,
    ) -> HostKeyDecision {
        HostKeyDecision::Accept
    }
}

/// The result of checking one key, for the caller to act on after connecting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyOutcome {
    /// What the file said before the decision.
    pub verdict: Verdict,
    /// What was decided.
    pub decision: HostKeyDecision,
    /// The key the server presented.
    pub key: HostKey,
}

impl HostKeyOutcome {
    /// Whether the caller should append this key to `known_hosts`.
    pub fn should_store(&self) -> bool {
        self.decision == HostKeyDecision::AcceptAndStore
    }

    /// Whether the connection was allowed to continue.
    pub fn decision_allows(&self) -> bool {
        self.decision != HostKeyDecision::Reject
    }
}

/// Runs a verdict past a verifier and remembers what happened.
///
/// Shared with the `russh` handler, which cannot own it outright: the caller needs the outcome after
/// the handshake, to write the key down if that is what was chosen.
#[derive(Clone)]
pub struct HostKeyChecker {
    host: String,
    port: u16,
    known_hosts: Arc<KnownHosts>,
    verifier: Arc<dyn HostKeyVerifier>,
    outcome: Arc<std::sync::Mutex<Option<HostKeyOutcome>>>,
}

impl std::fmt::Debug for HostKeyChecker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostKeyChecker")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl HostKeyChecker {
    /// A checker for one connection.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        known_hosts: Arc<KnownHosts>,
        verifier: Arc<dyn HostKeyVerifier>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            known_hosts,
            verifier,
            outcome: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Check a key, returning whether the connection may continue.
    pub fn check(&self, key: &HostKey) -> bool {
        let verdict = self.known_hosts.verify(&self.host, self.port, key);

        // A revoked key is never a question. Asking would offer a way to say yes to a key someone
        // went to the trouble of marking as compromised.
        let decision = if verdict == Verdict::Revoked {
            HostKeyDecision::Reject
        } else {
            self.verifier.decide(&self.host, self.port, key, &verdict)
        };

        tracing::debug!(
            host = %self.host,
            port = self.port,
            fingerprint = %key.fingerprint(),
            ?verdict,
            ?decision,
            "checked a host key"
        );

        if let Ok(mut slot) = self.outcome.lock() {
            *slot = Some(HostKeyOutcome {
                verdict,
                decision,
                key: key.clone(),
            });
        }

        decision != HostKeyDecision::Reject
    }

    /// What was decided, once a key has been checked.
    pub fn outcome(&self) -> Option<HostKeyOutcome> {
        self.outcome.lock().ok().and_then(|slot| slot.clone())
    }
}

/// Convert the key `russh` hands us into the form `known_hosts` records.
///
/// `to_bytes` produces the SSH wire encoding, which is exactly what is base64-encoded in the file, so
/// the two representations compare directly rather than through a re-encoding that could differ.
pub fn host_key_from_ssh(key: &ssh_key::PublicKey) -> Result<HostKey, ssh_key::Error> {
    Ok(HostKey::new(key.algorithm().as_str(), key.to_bytes()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> HostKey {
        HostKey::new("ssh-ed25519", vec![byte; 32])
    }

    fn checker_with(text: &str, verifier: Arc<dyn HostKeyVerifier>) -> HostKeyChecker {
        HostKeyChecker::new("srv.int", 22, Arc::new(KnownHosts::parse(text)), verifier)
    }

    fn recorded(byte: u8) -> String {
        use base64::prelude::{BASE64_STANDARD, Engine as _};
        format!(
            "srv.int ssh-ed25519 {}",
            BASE64_STANDARD.encode(vec![byte; 32])
        )
    }

    #[test]
    fn a_strict_verifier_accepts_only_what_is_recorded() {
        let checker = checker_with(&recorded(1), Arc::new(StrictVerifier));
        assert!(checker.check(&key(1)));

        let outcome = checker.outcome().expect("recorded");
        assert_eq!(outcome.verdict, Verdict::Trusted);
        assert_eq!(outcome.decision, HostKeyDecision::Accept);
        assert!(outcome.decision_allows());
        assert!(!outcome.should_store());
    }

    #[test]
    fn a_strict_verifier_refuses_an_unknown_host() {
        // The safe direction for anything unattended.
        let checker = checker_with("", Arc::new(StrictVerifier));
        assert!(!checker.check(&key(1)));

        let outcome = checker.outcome().expect("recorded");
        assert_eq!(outcome.decision, HostKeyDecision::Reject);
        assert!(!outcome.decision_allows());
    }

    #[test]
    fn a_strict_verifier_refuses_a_changed_key() {
        let checker = checker_with(&recorded(1), Arc::new(StrictVerifier));
        assert!(!checker.check(&key(2)));
        let outcome = checker.outcome().expect("recorded");
        assert!(matches!(outcome.verdict, Verdict::Changed { .. }));
    }

    #[test]
    fn a_revoked_key_is_refused_even_by_a_verifier_that_accepts_everything() {
        // Asking would offer a way to say yes to a key someone deliberately marked as compromised,
        // so revocation is decided before the verifier is consulted at all.
        let text = format!("@revoked {}", recorded(1));
        let checker = checker_with(&text, Arc::new(AcceptAnyVerifierForTests));
        assert!(!checker.check(&key(1)));

        let outcome = checker.outcome().expect("recorded");
        assert_eq!(outcome.verdict, Verdict::Revoked);
        assert_eq!(outcome.decision, HostKeyDecision::Reject);
    }

    #[test]
    fn a_verifier_that_stores_is_reported_to_the_caller() {
        struct Storing;
        impl HostKeyVerifier for Storing {
            fn decide(&self, _: &str, _: u16, _: &HostKey, _: &Verdict) -> HostKeyDecision {
                HostKeyDecision::AcceptAndStore
            }
        }

        let checker = checker_with("", Arc::new(Storing));
        assert!(checker.check(&key(9)));

        let outcome = checker.outcome().expect("recorded");
        assert!(outcome.should_store());
        assert_eq!(outcome.key, key(9));
        assert_eq!(outcome.verdict, Verdict::Unknown);
    }

    #[test]
    fn nothing_is_recorded_before_a_key_is_checked() {
        let checker = checker_with("", Arc::new(StrictVerifier));
        assert!(checker.outcome().is_none());
    }

    #[test]
    fn the_verifier_sees_the_verdict_it_needs_to_phrase_the_question() {
        // A UI must be able to tell "first connection" from "the key changed" — they are different
        // questions and only one of them is routine.
        struct Recording(std::sync::Mutex<Vec<String>>);
        impl HostKeyVerifier for Recording {
            fn decide(&self, _: &str, _: u16, _: &HostKey, verdict: &Verdict) -> HostKeyDecision {
                if let Ok(mut seen) = self.0.lock() {
                    seen.push(format!("{verdict:?}"));
                }
                HostKeyDecision::Reject
            }
        }

        let recorder = Arc::new(Recording(std::sync::Mutex::new(Vec::new())));
        let checker = checker_with(&recorded(1), recorder.clone());
        checker.check(&key(2));

        let seen = recorder.0.lock().expect("lock").clone();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].starts_with("Changed"), "got {seen:?}");
    }
}
