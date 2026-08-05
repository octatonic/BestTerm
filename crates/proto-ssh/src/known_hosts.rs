//! `known_hosts`: deciding whether a server is the one we connected to last time.
//!
//! This is the file that makes SSH resistant to a machine-in-the-middle, so the logic is worth being
//! exact about. It reads OpenSSH's format, because BestTerm must agree with the `known_hosts` a user
//! already has — writing our own format would silently drop years of accumulated trust and start
//! asking about every host again, which trains people to click through the warning.
//!
//! # The four answers
//!
//! Verification returns one of [`Verdict::Trusted`], [`Unknown`](Verdict::Unknown),
//! [`Changed`](Verdict::Changed) or [`Revoked`](Verdict::Revoked), and the difference between the
//! last three is the entire point:
//!
//! * *Unknown* is routine — a first connection. Ask, and record the answer.
//! * *Changed* is the one that matters. The host is known and presented a different key. It might be
//!   a rebuilt server; it might be an interception. It must never be presented as the same kind of
//!   question as *Unknown*.
//! * *Revoked* is an explicit `@revoked` marker. Never connect, never offer to accept.

use base64::prelude::{BASE64_STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Digest as _, Sha256};

/// A host key as presented by a server, or as recorded in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKey {
    /// Algorithm name, e.g. `ssh-ed25519`.
    pub algorithm: String,
    /// The key blob, already base64-decoded.
    pub blob: Vec<u8>,
}

impl HostKey {
    /// A key from its algorithm and raw blob.
    pub fn new(algorithm: impl Into<String>, blob: Vec<u8>) -> Self {
        Self {
            algorithm: algorithm.into(),
            blob,
        }
    }

    /// The fingerprint OpenSSH shows, `SHA256:` followed by unpadded base64.
    ///
    /// This is what a user is asked to compare against what their infrastructure told them, so the
    /// format has to match OpenSSH's exactly — including the absence of `=` padding.
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(&self.blob);
        let encoded = BASE64_STANDARD.encode(digest);
        format!("SHA256:{}", encoded.trim_end_matches('='))
    }
}

/// A marker at the start of a `known_hosts` line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Marker {
    /// An ordinary entry.
    #[default]
    None,
    /// `@revoked`: this key must never be accepted.
    Revoked,
    /// `@cert-authority`: the key signs host certificates rather than being a host key.
    CertAuthority,
}

/// How an entry names the hosts it applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pattern {
    /// A literal or wildcard pattern. `negated` for the leading `!` form.
    Glob { pattern: String, negated: bool },
    /// `|1|salt|hash`, where hash is HMAC-SHA1 of the host name keyed by the salt.
    Hashed { salt: Vec<u8>, hash: Vec<u8> },
}

/// One line of a `known_hosts` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Marker, if any.
    pub marker: Marker,
    /// The key on this line.
    pub key: HostKey,
    /// Line number in the source file, for error messages.
    pub line_number: usize,
    patterns: Vec<Pattern>,
}

/// What verification concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The key is recorded for this host.
    Trusted,
    /// The host is not in the file. A first connection: ask, then record.
    Unknown,
    /// The host is recorded with a **different** key.
    ///
    /// Carries what was expected so the user can be shown both fingerprints. This is not a variation
    /// of `Unknown` and must not be presented as one.
    Changed {
        /// The keys the file records for this host, of the same algorithm.
        expected: Vec<HostKey>,
    },
    /// An entry marks this key `@revoked`. Refuse, and do not offer to accept.
    Revoked,
}

/// A parsed `known_hosts` file.
#[derive(Clone, Debug, Default)]
pub struct KnownHosts {
    entries: Vec<Entry>,
}

impl KnownHosts {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a file's contents.
    ///
    /// Unparsable lines are skipped rather than failing the load: a `known_hosts` accumulated over
    /// years may contain entries written by tools that are no longer installed, and refusing to
    /// start because of one of them would be worse than ignoring it.
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match parse_entry(line, index + 1) {
                Some(entry) => entries.push(entry),
                None => tracing::debug!(line = index + 1, "skipped an unreadable known_hosts line"),
            }
        }
        Self { entries }
    }

    /// Every entry, in file order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// How many entries were understood.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Decide whether `key` is the key expected from `host`.
    pub fn verify(&self, host: &str, port: u16, key: &HostKey) -> Verdict {
        let name = host_pattern(host, port);
        let mut matched_host = false;
        let mut expected: Vec<HostKey> = Vec::new();

        for entry in &self.entries {
            if !entry.matches(&name) {
                continue;
            }
            // A certificate authority entry does not answer the question "is this the host key?".
            // Certificate verification is a separate mechanism and is not implemented yet, so such
            // entries are passed over rather than mistaken for a host key.
            if entry.marker == Marker::CertAuthority {
                continue;
            }
            if entry.key == *key {
                // Revocation wins even when the key otherwise matches — that is what revoking means.
                return if entry.marker == Marker::Revoked {
                    Verdict::Revoked
                } else {
                    Verdict::Trusted
                };
            }
            if entry.marker == Marker::Revoked {
                continue;
            }
            matched_host = true;
            if entry.key.algorithm == key.algorithm {
                expected.push(entry.key.clone());
            }
        }

        // A host recorded only under other algorithms is not evidence of tampering: servers offer
        // several, and the client may simply have negotiated one that was never recorded.
        if matched_host && !expected.is_empty() {
            Verdict::Changed { expected }
        } else {
            Verdict::Unknown
        }
    }

    /// Record a key, returning the line to append to the file.
    ///
    /// `hashed` writes the host name as an HMAC rather than in clear, which is what OpenSSH's
    /// `HashKnownHosts` does: it stops a stolen `known_hosts` from being a map of everywhere its
    /// owner connects.
    pub fn add(
        &mut self,
        host: &str,
        port: u16,
        key: &HostKey,
        hashed: bool,
    ) -> Result<String, HostsError> {
        let name = host_pattern(host, port);
        let pattern = if hashed {
            let mut salt = vec![0u8; 20];
            getrandom::fill(&mut salt).map_err(|_| HostsError::Random)?;
            let hash = hash_host(&salt, &name)?;
            Pattern::Hashed { salt, hash }
        } else {
            Pattern::Glob {
                pattern: name.clone(),
                negated: false,
            }
        };

        let rendered = format!(
            "{} {} {}",
            render_pattern(&pattern),
            key.algorithm,
            BASE64_STANDARD.encode(&key.blob)
        );

        self.entries.push(Entry {
            marker: Marker::None,
            key: key.clone(),
            line_number: 0,
            patterns: vec![pattern],
        });

        Ok(rendered)
    }
}

impl Entry {
    fn matches(&self, name: &str) -> bool {
        let mut matched = false;
        for pattern in &self.patterns {
            match pattern {
                Pattern::Glob { pattern, negated } => {
                    if glob_match(pattern, name) {
                        // A negated pattern excludes the host from the whole entry, whatever else
                        // matched: `*.example.com,!secret.example.com` must not cover the exception.
                        if *negated {
                            return false;
                        }
                        matched = true;
                    }
                }
                Pattern::Hashed { salt, hash } => {
                    if hash_host(salt, name).is_ok_and(|computed| computed == *hash) {
                        matched = true;
                    }
                }
            }
        }
        matched
    }
}

/// Errors from the store.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HostsError {
    /// The operating system would not supply randomness for a salt.
    #[error("could not obtain randomness from the operating system")]
    Random,
    /// HMAC rejected the salt length.
    #[error("invalid salt")]
    InvalidSalt,
}

/// How a host is written in `known_hosts`.
///
/// A non-default port is recorded as `[host]:port`. Getting this wrong means every session on a
/// non-standard port is treated as a first connection, forever — a steady stream of prompts that
/// teaches people to accept without looking.
fn host_pattern(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

fn hash_host(salt: &[u8], name: &str) -> Result<Vec<u8>, HostsError> {
    let mut mac = Hmac::<Sha1>::new_from_slice(salt).map_err(|_| HostsError::InvalidSalt)?;
    mac.update(name.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

fn render_pattern(pattern: &Pattern) -> String {
    match pattern {
        Pattern::Glob { pattern, negated } => {
            if *negated {
                format!("!{pattern}")
            } else {
                pattern.clone()
            }
        }
        Pattern::Hashed { salt, hash } => format!(
            "|1|{}|{}",
            BASE64_STANDARD.encode(salt),
            BASE64_STANDARD.encode(hash)
        ),
    }
}

fn parse_entry(line: &str, line_number: usize) -> Option<Entry> {
    let mut rest = line;
    let mut marker = Marker::None;

    if let Some(after) = rest.strip_prefix('@') {
        let (word, tail) = after.split_once(char::is_whitespace)?;
        marker = match word {
            "revoked" => Marker::Revoked,
            "cert-authority" => Marker::CertAuthority,
            // An unknown marker is not something to guess at: skip the line.
            _ => return None,
        };
        rest = tail.trim_start();
    }

    let mut parts = rest.split_whitespace();
    let hosts = parts.next()?;
    let algorithm = parts.next()?;
    let encoded = parts.next()?;

    let blob = BASE64_STANDARD.decode(encoded).ok()?;
    let patterns = parse_patterns(hosts)?;
    if patterns.is_empty() {
        return None;
    }

    Some(Entry {
        marker,
        key: HostKey::new(algorithm, blob),
        line_number,
        patterns,
    })
}

fn parse_patterns(field: &str) -> Option<Vec<Pattern>> {
    if let Some(rest) = field.strip_prefix("|1|") {
        let (salt, hash) = rest.split_once('|')?;
        return Some(vec![Pattern::Hashed {
            salt: BASE64_STANDARD.decode(salt).ok()?,
            hash: BASE64_STANDARD.decode(hash).ok()?,
        }]);
    }

    Some(
        field
            .split(',')
            .filter(|part| !part.is_empty())
            .map(|part| match part.strip_prefix('!') {
                Some(inner) => Pattern::Glob {
                    pattern: inner.to_string(),
                    negated: true,
                },
                None => Pattern::Glob {
                    pattern: part.to_string(),
                    negated: false,
                },
            })
            .collect(),
    )
}

/// OpenSSH host pattern matching: `*` for any run, `?` for one character.
///
/// Written out rather than pulled from a glob crate because the semantics here are narrow and fixed,
/// and because a general-purpose glob would bring path semantics — `/` handling, `**` — that do not
/// apply to host names and could only introduce differences from OpenSSH.
fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();

    // Iterative backtracking rather than recursion: a pathological pattern from a file should not be
    // able to exhaust the stack.
    let (mut p, mut n) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);

    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = n;
            p += 1;
        } else if let Some(star_at) = star {
            p = star_at + 1;
            resume += 1;
            n = resume;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(algorithm: &str, byte: u8) -> HostKey {
        HostKey::new(algorithm, vec![byte; 32])
    }

    fn line(host: &str, algorithm: &str, byte: u8) -> String {
        format!(
            "{host} {algorithm} {}",
            BASE64_STANDARD.encode(vec![byte; 32])
        )
    }

    #[test]
    fn a_recorded_key_is_trusted() {
        let hosts = KnownHosts::parse(&line("srv.int", "ssh-ed25519", 1));
        assert_eq!(
            hosts.verify("srv.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Trusted
        );
    }

    #[test]
    fn an_unrecorded_host_is_unknown() {
        let hosts = KnownHosts::parse(&line("srv.int", "ssh-ed25519", 1));
        assert_eq!(
            hosts.verify("other.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Unknown
        );
    }

    #[test]
    fn a_different_key_for_a_known_host_is_changed_not_unknown() {
        // The distinction the whole file exists for.
        let hosts = KnownHosts::parse(&line("srv.int", "ssh-ed25519", 1));
        let verdict = hosts.verify("srv.int", 22, &key("ssh-ed25519", 2));
        match verdict {
            Verdict::Changed { expected } => {
                assert_eq!(expected, vec![key("ssh-ed25519", 1)]);
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn a_host_known_only_under_another_algorithm_is_unknown_not_changed() {
        // Servers offer several host keys. Meeting one that was never recorded is not evidence of
        // tampering, and crying wolf here is how people learn to ignore the real warning.
        let hosts = KnownHosts::parse(&line("srv.int", "ssh-rsa", 1));
        assert_eq!(
            hosts.verify("srv.int", 22, &key("ssh-ed25519", 9)),
            Verdict::Unknown
        );
    }

    #[test]
    fn a_revoked_key_is_refused_even_though_it_matches() {
        let text = format!("@revoked {}", line("srv.int", "ssh-ed25519", 1));
        let hosts = KnownHosts::parse(&text);
        assert_eq!(
            hosts.verify("srv.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Revoked
        );
    }

    #[test]
    fn a_revoked_entry_does_not_make_a_different_key_look_changed() {
        // Revocation says "not this key", not "the host's key is this".
        let text = format!("@revoked {}", line("srv.int", "ssh-ed25519", 1));
        let hosts = KnownHosts::parse(&text);
        assert_eq!(
            hosts.verify("srv.int", 22, &key("ssh-ed25519", 2)),
            Verdict::Unknown
        );
    }

    #[test]
    fn a_certificate_authority_entry_is_not_treated_as_a_host_key() {
        let text = format!("@cert-authority {}", line("*.int", "ssh-ed25519", 1));
        let hosts = KnownHosts::parse(&text);
        // Neither trusted nor changed: certificates are a separate mechanism, not implemented yet.
        assert_eq!(
            hosts.verify("srv.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Unknown
        );
    }

    #[test]
    fn a_non_default_port_is_bracketed() {
        // Getting this wrong means every session on a non-standard port prompts forever.
        assert_eq!(host_pattern("srv.int", 22), "srv.int");
        assert_eq!(host_pattern("srv.int", 2222), "[srv.int]:2222");

        let hosts = KnownHosts::parse(&line("[srv.int]:2222", "ssh-ed25519", 1));
        assert_eq!(
            hosts.verify("srv.int", 2222, &key("ssh-ed25519", 1)),
            Verdict::Trusted
        );
        // And the same host on port 22 is a different entry.
        assert_eq!(
            hosts.verify("srv.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Unknown
        );
    }

    #[test]
    fn a_comma_separated_list_covers_every_name() {
        let hosts = KnownHosts::parse(&line("srv.int,10.0.0.5,srv", "ssh-ed25519", 1));
        for host in ["srv.int", "10.0.0.5", "srv"] {
            assert_eq!(
                hosts.verify(host, 22, &key("ssh-ed25519", 1)),
                Verdict::Trusted,
                "{host}"
            );
        }
    }

    #[test]
    fn wildcards_match_the_way_openssh_does() {
        assert!(glob_match("*.int", "srv.int"));
        assert!(glob_match("srv?.int", "srv1.int"));
        assert!(!glob_match("srv?.int", "srv12.int"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exactly"));
        // Trailing stars may match nothing.
        assert!(glob_match("abc*", "abc"));
        assert!(glob_match("*abc*", "abc"));
    }

    #[test]
    fn a_negated_pattern_excludes_the_host_from_the_entry() {
        let hosts = KnownHosts::parse(&line("*.int,!secret.int", "ssh-ed25519", 1));
        assert_eq!(
            hosts.verify("srv.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Trusted
        );
        assert_eq!(
            hosts.verify("secret.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Unknown,
            "the exception must not be covered by the wildcard"
        );
    }

    #[test]
    fn a_hashed_entry_matches_its_host() {
        // The format OpenSSH writes with HashKnownHosts, which most distributions turn on.
        let mut store = KnownHosts::new();
        let rendered = store
            .add("srv.int", 22, &key("ssh-ed25519", 1), true)
            .expect("adds");
        assert!(rendered.starts_with("|1|"), "got {rendered}");

        let reparsed = KnownHosts::parse(&rendered);
        assert_eq!(
            reparsed.verify("srv.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Trusted
        );
        assert_eq!(
            reparsed.verify("other.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Unknown
        );
    }

    #[test]
    fn a_hashed_entry_hides_the_host_name() {
        // The reason hashing exists: a stolen known_hosts must not be a map of where its owner goes.
        let mut store = KnownHosts::new();
        let rendered = store
            .add("secret-bastion.corp", 22, &key("ssh-ed25519", 1), true)
            .expect("adds");
        assert!(!rendered.contains("secret-bastion"), "got {rendered}");
    }

    #[test]
    fn each_hashed_entry_uses_a_fresh_salt() {
        let mut store = KnownHosts::new();
        let first = store
            .add("srv.int", 22, &key("ssh-ed25519", 1), true)
            .expect("adds");
        let second = store
            .add("srv.int", 22, &key("ssh-ed25519", 1), true)
            .expect("adds");
        assert_ne!(first, second, "a reused salt would link the two entries");
    }

    #[test]
    fn an_added_plain_entry_round_trips() {
        let mut store = KnownHosts::new();
        let rendered = store
            .add("srv.int", 2222, &key("ssh-ed25519", 7), false)
            .expect("adds");
        assert!(
            rendered.starts_with("[srv.int]:2222 ssh-ed25519 "),
            "got {rendered}"
        );
        assert_eq!(
            KnownHosts::parse(&rendered).verify("srv.int", 2222, &key("ssh-ed25519", 7)),
            Verdict::Trusted
        );
    }

    #[test]
    fn adding_updates_the_store_in_memory_too() {
        let mut store = KnownHosts::new();
        assert!(store.is_empty());
        store
            .add("srv.int", 22, &key("ssh-ed25519", 1), false)
            .expect("adds");
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.verify("srv.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Trusted
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let text = format!(
            "# a comment\n\n   \n{}\n",
            line("srv.int", "ssh-ed25519", 1)
        );
        assert_eq!(KnownHosts::parse(&text).len(), 1);
    }

    #[test]
    fn an_unreadable_line_does_not_discard_the_rest_of_the_file() {
        // Years-old files contain entries written by tools nobody has any more.
        let text = format!(
            "garbage\nsrv.int ssh-ed25519 not!base64\n@nonsense host alg AAAA\n{}\n",
            line("good.int", "ssh-ed25519", 1)
        );
        let hosts = KnownHosts::parse(&text);
        assert_eq!(hosts.len(), 1);
        assert_eq!(
            hosts.verify("good.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Trusted
        );
    }

    #[test]
    fn a_trailing_comment_on_an_entry_is_tolerated() {
        let text = format!("{} added by someone", line("srv.int", "ssh-ed25519", 1));
        assert_eq!(
            KnownHosts::parse(&text).verify("srv.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Trusted
        );
    }

    #[test]
    fn fingerprints_match_openssh_format() {
        // Unpadded base64 after "SHA256:", which is what `ssh-keygen -lf` prints and therefore what
        // a user will be comparing against.
        let fingerprint = HostKey::new("ssh-ed25519", b"hello".to_vec()).fingerprint();
        assert!(fingerprint.starts_with("SHA256:"), "got {fingerprint}");
        assert!(!fingerprint.ends_with('='), "padding must be stripped");
        // SHA-256 of "hello" is well known; base64 of it, unpadded.
        assert_eq!(
            fingerprint,
            "SHA256:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ"
        );
    }

    #[test]
    fn an_empty_store_says_unknown_rather_than_trusting() {
        let hosts = KnownHosts::new();
        assert_eq!(
            hosts.verify("srv.int", 22, &key("ssh-ed25519", 1)),
            Verdict::Unknown
        );
    }
}
