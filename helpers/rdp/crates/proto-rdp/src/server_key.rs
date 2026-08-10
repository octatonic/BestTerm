//! Deciding whether the RDP server on the other end is the one we spoke to last time.
//!
//! # Why this exists at all
//!
//! `ironrdp-tls` upgrades the connection with certificate verification switched off — it accepts
//! whatever certificate arrives. That is not an oversight: RDP servers overwhelmingly present
//! self-signed certificates, no certificate authority is involved, and the protocol's own defence is
//! CredSSP, which binds the authentication exchange to the server's public key. A client that
//! insisted on a chain of trust would refuse almost every real server.
//!
//! But "no chain of trust" is not the same as "no identity". Nothing above this layer notices when the
//! machine answering on port 3389 is a different machine than yesterday, and that is precisely the
//! event worth noticing. So this module does for RDP what `known_hosts` does for SSH: remember the key
//! the first time, and say so loudly when it changes.
//!
//! # What is remembered
//!
//! The server's subject public key, not its certificate. Three reasons:
//!
//! * It is what CredSSP binds, so it is the thing whose substitution actually matters.
//! * It survives a certificate being reissued for the same key, which happens on renewal and is not
//!   an event a person should be asked about.
//! * It arrives as bytes already — `ironrdp_tls::extract_tls_server_public_key` hands it over — so
//!   nothing here has to parse or re-encode a certificate, and no re-encoding can change the digest.
//!
//! This is deliberately the same shape as the SSH side, down to the names of the verdicts, and for now
//! deliberately a second copy of it rather than a shared abstraction. The two file formats have
//! nothing in common — one is OpenSSH's, with its globs and hashed hostnames — and two similar things
//! are not yet a pattern. When VNC needs a third, the common part will be worth extracting.

use std::collections::HashMap;

use sha2::{Digest as _, Sha256};

/// The marker that retires a key without deleting the line.
///
/// Kept from OpenSSH's vocabulary, and for the same reason: a key that turned out to be someone
/// else's should keep being refused, and deleting the entry would make it merely unknown again.
const REVOKED: &str = "@revoked";

/// The only digest this file format uses.
const ALGORITHM: &str = "sha256";

/// A SHA-256 digest of a server's subject public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyFingerprint([u8; 32]);

impl KeyFingerprint {
    /// Digest `spki`, the server's subject public key as it arrived.
    pub fn of(spki: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(spki);
        Self(hasher.finalize().into())
    }

    /// The digest as stored: sixty-four lowercase hex characters, no separators.
    pub fn to_hex(self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0F));
        }
        out
    }

    /// Read a digest back from its stored form.
    ///
    /// Case-insensitive, because a person who copies a fingerprint out of a certificate viewer gets
    /// upper case and should not have to know that.
    pub fn from_hex(text: &str) -> Option<Self> {
        // Colons are accepted on input as well as produced by `Display`, so a fingerprint pasted from
        // anywhere at all round-trips.
        let digits: Vec<u8> = text
            .bytes()
            .filter(|byte| *byte != b':')
            .map(hex_value)
            .collect::<Option<Vec<u8>>>()?;

        if digits.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (byte, pair) in bytes.iter_mut().zip(digits.chunks_exact(2)) {
            *byte = (pair[0] << 4) | pair[1];
        }
        Some(Self(bytes))
    }
}

impl std::fmt::Display for KeyFingerprint {
    /// Colon-separated pairs, which is how every certificate viewer shows a thumbprint.
    ///
    /// Different from the stored form on purpose: a fingerprint is read aloud, compared by eye and
    /// pasted into a ticket, and `ab:cd:ef` survives all three better than an unbroken run of hex.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for KeyFingerprint {
    /// The same as `Display`. A fingerprint is a public value; there is nothing to redact and every
    /// reason for it to be readable in a test failure.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SHA256:{self}")
    }
}

fn hex_digit(nibble: u8) -> char {
    char::from(match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + (nibble - 10),
    })
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// What the store says about a key a server just presented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Exactly the key recorded for this server.
    Trusted,
    /// Nothing is recorded for this server.
    Unknown,
    /// Something is recorded, and it is not this.
    ///
    /// The one verdict that must never be waved through quietly: it is what a machine-in-the-middle
    /// looks like, and it is also what a rebuilt server looks like, and only the person can tell those
    /// apart.
    Changed {
        /// The key that was expected.
        expected: KeyFingerprint,
    },
    /// This key was retired, and must stay refused.
    Revoked,
}

/// A server's address as the store keys it.
///
/// The port is always part of it. RDP's default is 3389 but moving it is common, and two services on
/// one host are two different servers.
fn key_for(host: &str, port: u16) -> String {
    // Host names are case-insensitive; the stored form is lower case so a session configured as
    // "RDP.int" matches an entry written as "rdp.int".
    format!("{}:{port}", host.to_ascii_lowercase())
}

/// The remembered keys of RDP servers.
#[derive(Clone, Debug, Default)]
pub struct KnownServers {
    trusted: HashMap<String, KeyFingerprint>,
    revoked: HashMap<String, Vec<KeyFingerprint>>,
}

impl KnownServers {
    /// Read a store from the text of its file.
    ///
    /// Unreadable lines are skipped rather than refused. A file with one bad line in it still holds
    /// every other entry, and refusing to load any of them would leave a person unable to connect
    /// anywhere — which is the kind of failure that gets solved by deleting the file, losing every
    /// pinned key at once.
    pub fn parse(text: &str) -> Self {
        let mut store = Self::default();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let (revoked, rest) = match line.strip_prefix(REVOKED) {
                Some(rest) => (true, rest.trim_start()),
                None => (false, line),
            };

            let mut fields = rest.split_whitespace();
            let (Some(address), Some(algorithm), Some(digest)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            // Only one algorithm is defined. An entry naming another is from a future version and is
            // left alone rather than guessed at.
            if !algorithm.eq_ignore_ascii_case(ALGORITHM) {
                continue;
            }
            let Some(fingerprint) = KeyFingerprint::from_hex(digest) else {
                continue;
            };

            let address = address.to_ascii_lowercase();
            if revoked {
                store.revoked.entry(address).or_default().push(fingerprint);
            } else {
                // First entry wins, as in `known_hosts`: a file is appended to, so the older line is
                // the one that was deliberately accepted.
                store.trusted.entry(address).or_insert(fingerprint);
            }
        }

        store
    }

    /// What this store says about `presented`, offered by `host:port`.
    pub fn verify(&self, host: &str, port: u16, presented: KeyFingerprint) -> Verdict {
        let address = key_for(host, port);

        // Revocation is decided first, and without consulting anything else. A key that is both
        // recorded and revoked is revoked: the recorded line may be the very one that turned out to
        // be wrong.
        if let Some(revoked) = self.revoked.get(&address)
            && revoked.contains(&presented)
        {
            return Verdict::Revoked;
        }

        match self.trusted.get(&address) {
            None => Verdict::Unknown,
            Some(expected) if *expected == presented => Verdict::Trusted,
            Some(expected) => Verdict::Changed {
                expected: *expected,
            },
        }
    }

    /// The line to append when a key is accepted.
    ///
    /// Returned rather than written: which file to touch, and whether to touch one at all, is the
    /// application's decision, and a protocol crate that wrote to a person's home directory would be
    /// impossible to test and unpleasant to trust.
    pub fn line_for(host: &str, port: u16, fingerprint: KeyFingerprint) -> String {
        format!(
            "{} {ALGORITHM} {}",
            key_for(host, port),
            fingerprint.to_hex()
        )
    }

    /// How many servers are trusted. For tests and for a settings screen that shows a count.
    pub fn len(&self) -> usize {
        self.trusted.len()
    }

    /// Whether nothing is trusted yet.
    pub fn is_empty(&self) -> bool {
        self.trusted.is_empty()
    }
}

/// What to do about a key the store could not simply confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Go ahead, and remember this key.
    AcceptAndStore,
    /// Go ahead this once, without remembering.
    AcceptOnce,
    /// Do not connect.
    Reject,
}

/// Who answers when a key needs a decision.
///
/// A trait so the policy — ask the person, refuse everything new, answer a fixed way in a test —
/// lives outside the connection code. The verifier is *only* consulted for verdicts that are not
/// already settled: see [`ServerKeyChecker`].
pub trait Verifier: Send + Sync {
    /// Decide about `presented`, given what the store said.
    fn decide(
        &self,
        host: &str,
        port: u16,
        presented: KeyFingerprint,
        verdict: &Verdict,
    ) -> Decision;
}

/// Accepts nothing that is not already recorded.
///
/// The right default for anything unattended, and the wrong one for a person sitting in front of the
/// application — they need to be asked. Named for what it does so a call site reads honestly.
#[derive(Clone, Copy, Debug, Default)]
pub struct StrictVerifier;

impl Verifier for StrictVerifier {
    fn decide(&self, _: &str, _: u16, _: KeyFingerprint, _: &Verdict) -> Decision {
        Decision::Reject
    }
}

/// Accepts anything, for tests only.
///
/// Named at length so that it cannot appear in shipping code without somebody noticing in review.
#[derive(Clone, Copy, Debug, Default)]
pub struct AcceptAnyVerifierForTests;

impl Verifier for AcceptAnyVerifierForTests {
    fn decide(&self, _: &str, _: u16, _: KeyFingerprint, _: &Verdict) -> Decision {
        Decision::AcceptAndStore
    }
}

/// What happened when a key was checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// What the store said.
    pub verdict: Verdict,
    /// What was decided, or `None` when the store settled it without asking.
    pub decision: Option<Decision>,
    /// The key the server presented.
    pub presented: KeyFingerprint,
}

impl Outcome {
    /// Whether the connection may proceed.
    pub fn allows_connection(&self) -> bool {
        match self.decision {
            None => self.verdict == Verdict::Trusted,
            Some(Decision::AcceptAndStore | Decision::AcceptOnce) => true,
            Some(Decision::Reject) => false,
        }
    }

    /// Whether the caller should append this key to its store.
    pub fn should_store(&self) -> bool {
        self.decision == Some(Decision::AcceptAndStore)
    }
}

/// Puts the store and the verifier together, in the right order.
#[derive(Clone)]
pub struct ServerKeyChecker<V> {
    known: KnownServers,
    verifier: V,
}

impl<V: Verifier> ServerKeyChecker<V> {
    /// Check against `known`, asking `verifier` only when the store cannot settle it.
    pub fn new(known: KnownServers, verifier: V) -> Self {
        Self { known, verifier }
    }

    /// Decide about the key `host:port` presented.
    ///
    /// A revoked key is refused without the verifier being asked. That ordering is the point of this
    /// type: a verifier that prompts a person would otherwise offer them the chance to accept a key
    /// that was already established to be somebody else's, and "are you sure?" is not a question that
    /// should have a yes.
    pub fn check(&self, host: &str, port: u16, spki: &[u8]) -> Outcome {
        let presented = KeyFingerprint::of(spki);
        let verdict = self.known.verify(host, port, presented);

        let decision = match verdict {
            Verdict::Trusted => None,
            Verdict::Revoked => Some(Decision::Reject),
            Verdict::Unknown | Verdict::Changed { .. } => {
                Some(self.verifier.decide(host, port, presented, &verdict))
            }
        };

        Outcome {
            verdict,
            decision,
            presented,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A key that is stable across a test run but is nobody's real key.
    fn spki(seed: u8) -> Vec<u8> {
        vec![seed; 64]
    }

    fn fingerprint(seed: u8) -> KeyFingerprint {
        KeyFingerprint::of(&spki(seed))
    }

    fn store_with(host: &str, port: u16, seed: u8) -> KnownServers {
        KnownServers::parse(&KnownServers::line_for(host, port, fingerprint(seed)))
    }

    #[test]
    fn a_fingerprint_round_trips_through_its_stored_form() {
        let original = fingerprint(1);
        let hex = original.to_hex();
        assert_eq!(hex.len(), 64, "{hex}");
        assert_eq!(KeyFingerprint::from_hex(&hex), Some(original));
    }

    #[test]
    fn a_fingerprint_reads_back_from_the_form_a_person_pasted() {
        // Certificate viewers show upper case with colons. Someone comparing by eye and then pasting
        // must not be told their own server's fingerprint is malformed.
        let original = fingerprint(2);
        let displayed = original.to_string();
        assert!(displayed.contains(':'), "{displayed}");

        let shouted = displayed.to_uppercase();
        assert_eq!(KeyFingerprint::from_hex(&shouted), Some(original));
    }

    #[test]
    fn the_displayed_and_stored_forms_are_the_same_digest() {
        let original = fingerprint(3);
        let from_display = KeyFingerprint::from_hex(&original.to_string());
        let from_storage = KeyFingerprint::from_hex(&original.to_hex());
        assert_eq!(from_display, from_storage);
        assert_eq!(from_display, Some(original));
    }

    #[test]
    fn a_digest_of_the_wrong_length_is_not_accepted() {
        // Half a fingerprint compared against a whole one would match a prefix and read as trusted.
        assert_eq!(KeyFingerprint::from_hex(""), None);
        assert_eq!(KeyFingerprint::from_hex(&"ab".repeat(31)), None);
        assert_eq!(KeyFingerprint::from_hex(&"ab".repeat(33)), None);
        assert_eq!(KeyFingerprint::from_hex(&"z".repeat(64)), None);
    }

    #[test]
    fn different_keys_have_different_fingerprints() {
        assert_ne!(fingerprint(1), fingerprint(2));
    }

    #[test]
    fn a_recorded_key_is_trusted() {
        let store = store_with("rdp.int", 3389, 7);
        let verdict = store.verify("rdp.int", 3389, fingerprint(7));
        assert_eq!(verdict, Verdict::Trusted);
    }

    #[test]
    fn a_server_nobody_has_met_is_unknown_not_changed() {
        // The distinction the whole module exists for: one is a question, the other is an alarm.
        let store = store_with("rdp.int", 3389, 7);
        let verdict = store.verify("other.int", 3389, fingerprint(7));
        assert_eq!(verdict, Verdict::Unknown);
    }

    #[test]
    fn a_different_key_from_a_known_server_is_reported_as_changed() {
        let store = store_with("rdp.int", 3389, 7);
        match store.verify("rdp.int", 3389, fingerprint(8)) {
            Verdict::Changed { expected } => assert_eq!(expected, fingerprint(7)),
            other => panic!("expected a change, got {other:?}"),
        }
    }

    #[test]
    fn the_port_is_part_of_the_identity() {
        // Two services on one host are two servers. Matching on the host alone would let a session on
        // one port silently vouch for another.
        let store = store_with("rdp.int", 3389, 7);
        let verdict = store.verify("rdp.int", 13389, fingerprint(7));
        assert_eq!(verdict, Verdict::Unknown);
    }

    #[test]
    fn a_host_name_matches_whatever_case_it_was_written_in() {
        let store = store_with("RDP.Int", 3389, 7);
        assert_eq!(
            store.verify("rdp.INT", 3389, fingerprint(7)),
            Verdict::Trusted
        );
    }

    #[test]
    fn a_revoked_key_stays_refused_even_when_it_is_also_recorded() {
        // The case the ordering exists for. The recorded line may be the very one that turned out to
        // be somebody else's, so revocation cannot be something the trusted entry overrides.
        let text = format!(
            "{}\n{REVOKED} {}\n",
            KnownServers::line_for("rdp.int", 3389, fingerprint(7)),
            KnownServers::line_for("rdp.int", 3389, fingerprint(7)),
        );
        let store = KnownServers::parse(&text);
        let verdict = store.verify("rdp.int", 3389, fingerprint(7));
        assert_eq!(verdict, Verdict::Revoked);
    }

    #[test]
    fn revoking_one_key_does_not_revoke_the_server() {
        let text = format!(
            "{}\n{REVOKED} {}\n",
            KnownServers::line_for("rdp.int", 3389, fingerprint(9)),
            KnownServers::line_for("rdp.int", 3389, fingerprint(8)),
        );
        let store = KnownServers::parse(&text);
        assert_eq!(
            store.verify("rdp.int", 3389, fingerprint(9)),
            Verdict::Trusted
        );
    }

    #[test]
    fn the_first_entry_for_a_server_wins() {
        // A store is appended to, so the earlier line is the one somebody deliberately accepted.
        let text = format!(
            "{}\n{}\n",
            KnownServers::line_for("rdp.int", 3389, fingerprint(1)),
            KnownServers::line_for("rdp.int", 3389, fingerprint(2)),
        );
        let store = KnownServers::parse(&text);
        match store.verify("rdp.int", 3389, fingerprint(2)) {
            Verdict::Changed { expected } => assert_eq!(expected, fingerprint(1)),
            other => panic!("the later line won: {other:?}"),
        }
    }

    #[test]
    fn one_unreadable_line_does_not_cost_the_whole_file() {
        // A store that refuses to load leaves somebody unable to connect anywhere, and the fix people
        // reach for is deleting it — losing every pinned key at once.
        let good = KnownServers::line_for("rdp.int", 3389, fingerprint(4));
        let text = format!(
            "# a comment\n\nnonsense\nrdp.int:3389 md5 abcdef\nshort:1 sha256 ab\n{good}\n"
        );
        let store = KnownServers::parse(&text);

        assert_eq!(store.len(), 1);
        assert_eq!(
            store.verify("rdp.int", 3389, fingerprint(4)),
            Verdict::Trusted
        );
    }

    #[test]
    fn an_empty_store_is_empty() {
        let store = KnownServers::parse("# nothing here\n\n");
        assert!(store.is_empty());
        assert_eq!(
            store.verify("rdp.int", 3389, fingerprint(1)),
            Verdict::Unknown
        );
    }

    #[test]
    fn a_trusted_server_never_reaches_the_verifier() {
        // Asking about a key that is already recorded would train a person to click through prompts.
        struct Counting(Mutex<usize>);
        impl Verifier for Counting {
            fn decide(&self, _: &str, _: u16, _: KeyFingerprint, _: &Verdict) -> Decision {
                *self.0.lock().expect("not poisoned") += 1;
                Decision::Reject
            }
        }

        let verifier = Counting(Mutex::new(0));
        let checker = ServerKeyChecker::new(store_with("rdp.int", 3389, 7), verifier);
        let outcome = checker.check("rdp.int", 3389, &spki(7));

        assert_eq!(outcome.verdict, Verdict::Trusted);
        assert_eq!(outcome.decision, None, "the verifier was consulted");
        assert!(outcome.allows_connection());
        assert!(!outcome.should_store(), "it is already stored");
    }

    #[test]
    fn a_revoked_key_is_refused_without_asking_anyone() {
        // "Are you sure?" is not a question that should have a yes here.
        let text = format!(
            "{REVOKED} {}\n",
            KnownServers::line_for("rdp.int", 3389, fingerprint(7))
        );
        let checker = ServerKeyChecker::new(KnownServers::parse(&text), AcceptAnyVerifierForTests);
        let outcome = checker.check("rdp.int", 3389, &spki(7));

        assert_eq!(outcome.verdict, Verdict::Revoked);
        assert_eq!(outcome.decision, Some(Decision::Reject));
        assert!(!outcome.allows_connection());
    }

    #[test]
    fn a_strict_checker_refuses_a_server_it_has_never_met() {
        let checker = ServerKeyChecker::new(KnownServers::default(), StrictVerifier);
        let outcome = checker.check("rdp.int", 3389, &spki(1));

        assert_eq!(outcome.verdict, Verdict::Unknown);
        assert!(!outcome.allows_connection());
        assert!(!outcome.should_store());
    }

    #[test]
    fn accepting_a_new_key_asks_the_caller_to_store_it() {
        let checker = ServerKeyChecker::new(KnownServers::default(), AcceptAnyVerifierForTests);
        let outcome = checker.check("rdp.int", 3389, &spki(5));

        assert!(outcome.allows_connection());
        assert!(outcome.should_store());
        assert_eq!(outcome.presented, fingerprint(5));

        // And the line it produces is one the store reads back as trusted.
        let line = KnownServers::line_for("rdp.int", 3389, outcome.presented);
        let reloaded = KnownServers::parse(&line);
        assert_eq!(
            reloaded.verify("rdp.int", 3389, fingerprint(5)),
            Verdict::Trusted
        );
    }

    #[test]
    fn accepting_once_connects_without_recording_anything() {
        struct Once;
        impl Verifier for Once {
            fn decide(&self, _: &str, _: u16, _: KeyFingerprint, _: &Verdict) -> Decision {
                Decision::AcceptOnce
            }
        }

        let checker = ServerKeyChecker::new(KnownServers::default(), Once);
        let outcome = checker.check("rdp.int", 3389, &spki(6));

        assert!(outcome.allows_connection());
        assert!(!outcome.should_store(), "nothing was meant to be written");
    }

    #[test]
    fn a_changed_key_reaches_the_verifier_with_the_one_that_was_expected() {
        // What the prompt needs in order to be worth showing: both fingerprints, so a person can see
        // whether this is the rebuild they did on Tuesday.
        struct Capture(Mutex<Option<Verdict>>);
        impl Verifier for Capture {
            fn decide(&self, _: &str, _: u16, _: KeyFingerprint, verdict: &Verdict) -> Decision {
                *self.0.lock().expect("not poisoned") = Some(verdict.clone());
                Decision::Reject
            }
        }

        let checker =
            ServerKeyChecker::new(store_with("rdp.int", 3389, 7), Capture(Mutex::new(None)));
        let outcome = checker.check("rdp.int", 3389, &spki(8));

        match outcome.verdict {
            Verdict::Changed { expected } => assert_eq!(expected, fingerprint(7)),
            other => panic!("expected a change, got {other:?}"),
        }
        assert!(!outcome.allows_connection());
    }
}
