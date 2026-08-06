//! Integration tests against a real `sshd`.
//!
//! Everything else in this crate tests our idea of SSH. These test the thing itself: a genuine
//! OpenSSH server, a genuine key exchange, a genuine shell. They are what turns "the host key logic
//! is correct in isolation" into "the client actually refuses the server it should refuse".
//!
//! # Running them
//!
//! They skip themselves unless the environment describes a server, so `cargo test` stays green on a
//! machine that has none. CI starts one; to run them by hand:
//!
//! ```sh
//! export BESTTERM_SSH_TEST_HOST=127.0.0.1
//! export BESTTERM_SSH_TEST_PORT=2222
//! export BESTTERM_SSH_TEST_USER=bestterm-test
//! export BESTTERM_SSH_TEST_PASSWORD=integration-test-password
//! export BESTTERM_SSH_TEST_HOST_KEY="$(cat /etc/bestterm-sshd/host_ed25519.pub)"
//! cargo test -p bestterm-proto-ssh --test real_sshd
//! ```
//!
//! # Why every test asks for a multi-threaded runtime
//!
//! A shell channel is driven by spawned tasks, and the transport's events arrive on a synchronous
//! channel. Waiting on that channel from the test blocks the thread it runs on — which on the
//! default current-thread runtime is the same thread the reader task needs, and the test would hang
//! rather than fail.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bestterm_core_vault::Secret;
use bestterm_proto_ssh::host_key::{HostKeyDecision, HostKeyVerifier};
use bestterm_proto_ssh::known_hosts::{HostKey, KnownHosts, Verdict};
use bestterm_proto_ssh::{Auth, SshConnection, SshError, StrictVerifier, Target};
// The trait itself is not imported: `open.transport` is a trait object, and methods on one are
// callable without the trait in scope.
use bestterm_transport::{GridSize, TransportEvent};

/// How the environment describes the server to talk to.
struct Server {
    host: String,
    port: u16,
    user: String,
    password: String,
    /// The contents of the server's `.pub` file: `algorithm base64 comment`.
    host_key_line: String,
}

impl Server {
    fn from_env() -> Option<Self> {
        Some(Self {
            host: std::env::var("BESTTERM_SSH_TEST_HOST").ok()?,
            port: std::env::var("BESTTERM_SSH_TEST_PORT").ok()?.parse().ok()?,
            user: std::env::var("BESTTERM_SSH_TEST_USER").ok()?,
            password: std::env::var("BESTTERM_SSH_TEST_PASSWORD").ok()?,
            host_key_line: std::env::var("BESTTERM_SSH_TEST_HOST_KEY").ok()?,
        })
    }

    fn target(&self) -> Target {
        Target {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
        }
    }

    /// A `known_hosts` file recording this server's real key.
    ///
    /// The port is not 22, so the entry is bracketed — which makes every test here also a check that
    /// the bracketing rule is applied on the connecting side.
    fn known_hosts(&self) -> KnownHosts {
        let mut fields = self.host_key_line.split_whitespace();
        let algorithm = fields.next().expect("host key line has an algorithm");
        let blob = fields.next().expect("host key line has a key");
        KnownHosts::parse(&format!(
            "[{}]:{} {} {}",
            self.host, self.port, algorithm, blob
        ))
    }

    /// A `known_hosts` file recording a *different* key of the same algorithm.
    fn known_hosts_with_wrong_key(&self) -> KnownHosts {
        let algorithm = self
            .host_key_line
            .split_whitespace()
            .next()
            .expect("host key line has an algorithm");
        // 32 bytes of nothing, base64-encoded, is a syntactically fine key that is not this server's.
        let blob = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        KnownHosts::parse(&format!(
            "[{}]:{} {} {}",
            self.host, self.port, algorithm, blob
        ))
    }
}

/// Skips the test when no server is described.
macro_rules! server_or_skip {
    () => {
        match Server::from_env() {
            Some(server) => server,
            None => {
                eprintln!("no BESTTERM_SSH_TEST_* environment; skipping");
                return;
            }
        }
    };
}

/// Accepts anything and records what the file said, so a test can assert on the verdict.
struct Recording {
    decision: HostKeyDecision,
    seen: Mutex<Vec<Verdict>>,
}

impl Recording {
    fn new(decision: HostKeyDecision) -> Arc<Self> {
        Arc::new(Self {
            decision,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn verdicts(&self) -> Vec<Verdict> {
        self.seen.lock().expect("lock").clone()
    }
}

impl HostKeyVerifier for Recording {
    fn decide(&self, _: &str, _: u16, _: &HostKey, verdict: &Verdict) -> HostKeyDecision {
        self.seen.lock().expect("lock").push(verdict.clone());
        self.decision
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_strict_client_refuses_a_server_it_has_never_met() {
    // The safe default, proved against a real handshake rather than a unit test's idea of one.
    let server = server_or_skip!();

    let error = SshConnection::connect(
        server.target(),
        Auth::Password(Secret::new(server.password.clone())),
        Arc::new(KnownHosts::new()),
        Arc::new(StrictVerifier),
    )
    .await
    .expect_err("an unknown host must not be accepted");

    assert!(
        matches!(error, SshError::HostKeyRejected),
        "expected a host key rejection, got {error:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recorded_key_connects_without_a_question() {
    let server = server_or_skip!();

    let connection = SshConnection::connect(
        server.target(),
        Auth::Password(Secret::new(server.password.clone())),
        Arc::new(server.known_hosts()),
        Arc::new(StrictVerifier),
    )
    .await
    .expect("a recorded key should connect");

    let outcome = connection.host_key_outcome().expect("a key was checked");
    assert_eq!(outcome.verdict, Verdict::Trusted);
    assert!(!outcome.should_store(), "nothing new to record");
    connection.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_whose_key_changed_is_reported_as_changed() {
    // The case the whole known_hosts mechanism exists for, against a real server.
    let server = server_or_skip!();
    let verifier = Recording::new(HostKeyDecision::Reject);

    let error = SshConnection::connect(
        server.target(),
        Auth::Password(Secret::new(server.password.clone())),
        Arc::new(server.known_hosts_with_wrong_key()),
        verifier.clone(),
    )
    .await
    .expect_err("a changed key must not be accepted silently");
    assert!(matches!(error, SshError::HostKeyRejected), "got {error:?}");

    let verdicts = verifier.verdicts();
    assert_eq!(verdicts.len(), 1, "the key should be checked once");
    assert!(
        matches!(verdicts[0], Verdict::Changed { .. }),
        "the verifier must be told this is a change, not a first meeting; got {:?}",
        verdicts[0]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepting_a_new_key_yields_one_that_matches_the_server() {
    let server = server_or_skip!();
    let verifier = Recording::new(HostKeyDecision::AcceptAndStore);

    let connection = SshConnection::connect(
        server.target(),
        Auth::Password(Secret::new(server.password.clone())),
        Arc::new(KnownHosts::new()),
        verifier.clone(),
    )
    .await
    .expect("accepting should connect");

    let outcome = connection.host_key_outcome().expect("a key was checked");
    assert_eq!(outcome.verdict, Verdict::Unknown);
    assert!(outcome.should_store());
    connection.disconnect().await;

    // The recorded key must be the one the server actually presented: writing it down and then not
    // recognising it next time would be worse than not writing it down at all.
    let mut store = KnownHosts::new();
    let line = store
        .add(&server.host, server.port, &outcome.key, false)
        .expect("records");
    assert_eq!(
        KnownHosts::parse(&line).verify(&server.host, server.port, &outcome.key),
        Verdict::Trusted
    );

    // And it agrees with what the server's own .pub file says.
    let published = server
        .known_hosts()
        .verify(&server.host, server.port, &outcome.key);
    assert_eq!(
        published,
        Verdict::Trusted,
        "the key we stored differs from the server's published one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_password_is_an_authentication_failure_not_a_host_key_problem() {
    let server = server_or_skip!();

    let error = SshConnection::connect(
        server.target(),
        Auth::Password(Secret::new("definitely-not-the-password")),
        Arc::new(server.known_hosts()),
        Arc::new(StrictVerifier),
    )
    .await
    .expect_err("the wrong password must fail");

    assert!(
        matches!(error, SshError::AuthenticationFailed { .. }),
        "the two failures must stay distinguishable; got {error:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shell_runs_a_command_and_closes_cleanly() {
    // The end-to-end proof: connect, open a pty, type, read the output back, leave.
    let server = server_or_skip!();

    let connection = SshConnection::connect(
        server.target(),
        Auth::Password(Secret::new(server.password.clone())),
        Arc::new(server.known_hosts()),
        Arc::new(StrictVerifier),
    )
    .await
    .expect("connects");

    let open = connection
        .open_shell(GridSize::new(80, 24), "xterm-256color")
        .await
        .expect("opens a shell");
    let mut transport = open.transport;

    transport
        .write(b"echo bestterm-integration-ok\n")
        .expect("writes");

    let marker = "bestterm-integration-ok";
    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut closed = false;

    while Instant::now() < deadline {
        match open.events.recv_timeout(Duration::from_millis(500)) {
            Ok(TransportEvent::Output(bytes)) => {
                seen.push_str(&String::from_utf8_lossy(&bytes));
                // The echo of the typed line arrives first; the output is the second occurrence.
                if seen.matches(marker).count() >= 2 {
                    break;
                }
            }
            Ok(TransportEvent::Closed(_)) => {
                closed = true;
                break;
            }
            Ok(TransportEvent::Error(message)) => panic!("transport error: {message}"),
            Err(_) => continue,
        }
    }

    assert!(
        seen.contains(marker),
        "the shell never produced the output; saw:\n{seen}"
    );
    assert!(!closed, "the shell closed before running the command");

    // Resizing a live channel must be accepted by the server.
    transport.resize(GridSize::new(100, 40)).expect("resizes");
    assert_eq!(transport.size(), GridSize::new(100, 40));

    transport.write(b"exit\n").expect("writes");

    let mut exited = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match open.events.recv_timeout(Duration::from_millis(500)) {
            Ok(TransportEvent::Closed(_)) => {
                exited = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    assert!(exited, "the shell did not report that it had closed");

    transport.shutdown().expect("shuts down");
    connection.disconnect().await;
}
