//! SFTP against a real `sshd`.
//!
//! The unit tests in this crate test path arithmetic and formatting -- our idea of what a listing
//! looks like. These test the thing itself: a genuine OpenSSH server, a genuine subsystem request on
//! a second channel of an already-authenticated connection, a genuine listing, and a transfer that
//! has to come back byte for byte.
//!
//! They take the same environment as `bestterm-proto-ssh`'s own integration tests, and CI starts the
//! server for both. To run them by hand against that same setup:
//!
//! ```sh
//! export BESTTERM_SSH_TEST_HOST=127.0.0.1
//! export BESTTERM_SSH_TEST_PORT=2222
//! export BESTTERM_SSH_TEST_USER=bestterm-test
//! export BESTTERM_SSH_TEST_PASSWORD=integration-test-password
//! export BESTTERM_SSH_TEST_HOST_KEY="$(cat /etc/bestterm-sshd/host_ed25519.pub)"
//! cargo test -p bestterm-proto-sftp --test real_sshd
//! ```
//!
//! The host key is verified rather than accepted, which costs nothing here and means these tests
//! cannot pass against a server nobody checked.
//!
//! # Why a multi-threaded runtime
//!
//! The same reason as the SSH tests: the channel that carries SFTP is driven by spawned tasks, and a
//! current-thread runtime would give a blocked test the same thread those tasks need.

use std::sync::Arc;

use bestterm_core_vault::Secret;
use bestterm_proto_sftp::{EntryKind, FileCommand, FileEvent, Sftp, join};
use bestterm_proto_ssh::known_hosts::KnownHosts;
use bestterm_proto_ssh::{Auth, SshConnection, StrictVerifier, Target};

/// How the environment describes the server.
struct Server {
    host: String,
    port: u16,
    user: String,
    password: String,
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

    /// A `known_hosts` file recording this server's real key, bracketed for the non-default port.
    fn known_hosts(&self) -> KnownHosts {
        let mut fields = self.host_key_line.split_whitespace();
        let algorithm = fields.next().expect("host key line has an algorithm");
        let blob = fields.next().expect("host key line has a key");
        KnownHosts::parse(&format!(
            "[{}]:{} {} {}",
            self.host, self.port, algorithm, blob
        ))
    }

    async fn connect(&self) -> SshConnection {
        SshConnection::connect(
            self.target(),
            Auth::Password(Secret::new(self.password.clone())),
            Arc::new(self.known_hosts()),
            Arc::new(StrictVerifier),
        )
        .await
        .expect("the real server refused the connection")
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

#[tokio::test(flavor = "multi_thread")]
async fn the_server_serves_sftp_on_a_second_channel() {
    let server = server_or_skip!();
    let connection = server.connect().await;

    // The connection is already authenticated and would carry a shell. Opening SFTP on it is the
    // claim this whole crate rests on: a file browser costs a channel, not a second login.
    let sftp = Sftp::open(&connection, "test")
        .await
        .expect("the server refused the sftp subsystem");

    let home = sftp.home().await.expect("the server could not resolve `.`");
    assert!(
        home.starts_with('/'),
        "a canonical remote path is absolute: {home}"
    );

    sftp.close().await.expect("closing the channel failed");
    connection.disconnect().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_directory_we_made_lists_the_way_we_made_it() {
    let server = server_or_skip!();
    let connection = server.connect().await;
    let sftp = Sftp::open(&connection, "test").await.expect("sftp");
    let home = sftp.home().await.expect("home");

    // Built rather than assumed: the account's home on a CI runner has whatever the distribution
    // put there, and a test that asserted on it would be asserting on Ubuntu's skeleton files.
    let root = join(&home, "bestterm-sftp-listing");
    let _ = sftp.remove_directory(&root).await;
    sftp.make_directory(&root)
        .await
        .expect("a directory could not be made in our own home");

    let inner = join(&root, "a-directory");
    sftp.make_directory(&inner).await.expect("mkdir");
    let file = join(&root, "b-file");
    sftp.upload(
        &write_scratch("bestterm-listing", b"twelve bytes"),
        &file,
        false,
        &mut |_, _| {},
    )
    .await
    .expect("upload");

    let entries = sftp.list(&root).await.expect("the listing failed");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["a-directory", "b-file"],
        "directories come first, and `.` and `..` are not entries"
    );
    assert_eq!(entries[0].kind, EntryKind::Directory);
    assert_eq!(entries[1].kind, EntryKind::File);
    assert_eq!(
        entries[1].size, 12,
        "a real server reports the size it stored"
    );
    assert!(
        entries[1].permissions.is_some(),
        "OpenSSH sends a mode, and the listing has to keep it"
    );

    // `about` on one path has to agree with what the listing said about the same thing.
    let single = sftp.about(&file).await.expect("stat failed");
    assert_eq!(single.kind, EntryKind::File);
    assert_eq!(single.size, 12);
    assert_eq!(single.name, "b-file", "the name comes from the path");

    sftp.remove_file(&file).await.expect("remove file");
    sftp.remove_directory(&inner).await.expect("remove dir");
    sftp.remove_directory(&root).await.expect("remove root");
    connection.disconnect().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transfer_comes_back_byte_for_byte_and_resumes_where_it_stopped() {
    let server = server_or_skip!();
    let connection = server.connect().await;
    let sftp = Sftp::open(&connection, "test").await.expect("sftp");
    let home = sftp.home().await.expect("home");
    let remote = join(&home, "bestterm-sftp-transfer");

    // Larger than one chunk several times over, and not a multiple of it: an off-by-one in the loop
    // shows up as a short file or a duplicated tail, and a round number would hide both.
    let payload: Vec<u8> = (0..200_003_u32).map(|i| (i % 251) as u8).collect();
    let local = write_scratch("bestterm-transfer-up", &payload);

    let mut steps = Vec::new();
    let sent = sftp
        .upload(&local, &remote, false, &mut |done, total| {
            steps.push((done, total));
        })
        .await
        .expect("the upload failed");
    assert_eq!(sent as usize, payload.len(), "every byte has to arrive");
    assert_eq!(
        steps.first().map(|(done, _)| *done),
        Some(0),
        "progress starts at nothing"
    );
    assert!(
        steps.len() > 4,
        "progress is reported per chunk, and this is several: {}",
        steps.len()
    );
    assert_eq!(
        steps.last().map(|(done, _)| *done),
        Some(payload.len() as u64),
        "and ends at the whole file"
    );

    // The server's own idea of the size, which is the only opinion that counts.
    assert_eq!(
        sftp.about(&remote).await.expect("stat").size,
        payload.len() as u64
    );

    // Resuming something already complete must do nothing. This is the boundary an off-by-one turns
    // into a file with its own tail appended twice -- and it is the assertion that caught a resumed
    // upload emptying the remote file, because `create` in SFTP means truncate.
    //
    // The returned count is checked too, and on its own it proved nothing: it was `already` plus what
    // was copied, so it read back as a whole file while the file was zero bytes. The server's opinion
    // is the only one that counts here.
    let again = sftp
        .upload(&local, &remote, true, &mut |_, _| {})
        .await
        .expect("resume failed");
    assert_eq!(again as usize, payload.len());
    assert_eq!(
        sftp.about(&remote).await.expect("stat").size,
        payload.len() as u64,
        "a completed transfer resumed has to leave the file its own length"
    );
    assert_eq!(
        read_remote(&sftp, &remote, "after-complete-resume").await,
        payload,
        "and its own contents -- a truncation that happened to preserve the size would pass on \
         length alone"
    );

    // A *partial* upload resumed, which is the case resuming exists for and the one that was not
    // covered when the truncation went in. The remote starts as the first 70,000 bytes, exactly as an
    // interrupted transfer would have left it.
    let interrupted = join(&home, "bestterm-sftp-partial-up");
    let head = write_scratch("bestterm-transfer-head", &payload[..70_000]);
    sftp.upload(&head, &interrupted, false, &mut |_, _| {})
        .await
        .expect("the first part failed");
    assert_eq!(sftp.about(&interrupted).await.expect("stat").size, 70_000);

    let finished_up = sftp
        .upload(&local, &interrupted, true, &mut |_, _| {})
        .await
        .expect("the resumed upload failed");
    assert_eq!(finished_up as usize, payload.len());
    assert_eq!(
        read_remote(&sftp, &interrupted, "after-partial-resume").await,
        payload,
        "resuming has to continue the remote file rather than restart, duplicate, or empty it"
    );
    sftp.remove_file(&interrupted).await.expect("cleanup");
    let _ = std::fs::remove_file(&head);

    let back = std::env::temp_dir().join("bestterm-transfer-down");
    let _ = std::fs::remove_file(&back);
    let received = sftp
        .download(&remote, &back, false, &mut |_, _| {})
        .await
        .expect("the download failed");
    assert_eq!(received as usize, payload.len());
    assert_eq!(
        std::fs::read(&back).expect("read back"),
        payload,
        "a round trip has to be byte for byte"
    );

    // A download interrupted halfway, then resumed: the half-file is a real one, and finishing it has
    // to produce the same bytes as never having stopped.
    let partial = std::env::temp_dir().join("bestterm-transfer-partial");
    std::fs::write(&partial, &payload[..70_000]).expect("write partial");
    let finished = sftp
        .download(&remote, &partial, true, &mut |_, _| {})
        .await
        .expect("the resumed download failed");
    assert_eq!(
        finished as usize,
        payload.len(),
        "a resumed download reports the whole file, not just the part it fetched"
    );
    assert_eq!(
        std::fs::read(&partial).expect("read resumed"),
        payload,
        "resuming has to continue the file rather than restart or duplicate it"
    );

    sftp.remove_file(&remote).await.expect("remove");
    let _ = std::fs::remove_file(&local);
    let _ = std::fs::remove_file(&back);
    let _ = std::fs::remove_file(&partial);
    connection.disconnect().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn renaming_moves_a_file_and_a_refused_operation_says_why() {
    let server = server_or_skip!();
    let connection = server.connect().await;
    let sftp = Sftp::open(&connection, "test").await.expect("sftp");
    let home = sftp.home().await.expect("home");

    let from = join(&home, "bestterm-sftp-before");
    let to = join(&home, "bestterm-sftp-after");
    let _ = sftp.remove_file(&from).await;
    let _ = sftp.remove_file(&to).await;

    sftp.upload(
        &write_scratch("bestterm-rename", b"contents"),
        &from,
        false,
        &mut |_, _| {},
    )
    .await
    .expect("upload");

    sftp.rename(&from, &to).await.expect("rename failed");
    assert_eq!(sftp.about(&to).await.expect("stat").size, 8);
    assert!(
        sftp.about(&from).await.is_err(),
        "the old name has to be gone"
    );

    // A directory is not a file, and the error has to come back rather than be swallowed.
    let error = sftp
        .remove_file(&home)
        .await
        .expect_err("removing a directory as a file has to fail");
    let message = error.to_string();
    assert!(
        message.starts_with("sftp:"),
        "a server's refusal has to arrive as one: {message}"
    );

    sftp.remove_file(&to).await.expect("cleanup");
    connection.disconnect().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_answers_commands_and_reports_a_transfer_moving() {
    // The loop an interface actually drives: commands in, events out, nothing awaited on the
    // drawing side. Run against the real server because the interesting failures are ordering and
    // throttling, and neither shows up against a mock.
    let server = server_or_skip!();
    let connection = std::sync::Arc::new(server.connect().await);

    let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let waker = {
        let woken = std::sync::Arc::clone(&woken);
        std::sync::Arc::new(move || {
            woken.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }) as bestterm_proto_sftp::session::Waker
    };

    let (session, events) =
        bestterm_proto_sftp::session::start(std::sync::Arc::clone(&connection), "test", waker);

    // Ready comes first, unasked: an interface needs somewhere to open before it can draw anything.
    let home = match next(&events) {
        FileEvent::Ready { home } => home,
        other => panic!("the first event has to say where the account starts: {other:?}"),
    };
    assert!(home.starts_with('/'));

    let root = bestterm_proto_sftp::join(&home, "bestterm-session");
    session
        .send(FileCommand::MakeDirectory(root.clone()))
        .expect("the session is open");
    match next(&events) {
        FileEvent::Done(said) => assert!(said.contains("created"), "{said}"),
        other => panic!("mkdir reported {other:?}"),
    }

    // Big enough to take more than one progress interval on a loopback connection, and an odd
    // size for the same reason as the transfer test.
    let payload: Vec<u8> = (0..3_000_007_u32).map(|i| (i % 251) as u8).collect();
    let local = write_scratch("bestterm-session-payload", &payload);
    let remote = bestterm_proto_sftp::join(&root, "payload");

    session
        .send(FileCommand::Upload {
            id: 42,
            local: local.clone(),
            remote: remote.clone(),
            resume: false,
        })
        .expect("the session is open");

    let mut progress = Vec::new();
    let bytes = loop {
        match next(&events) {
            FileEvent::Progress { id, done, total } => {
                assert_eq!(id, 42, "progress is labelled with its own transfer");
                assert_eq!(total, Some(payload.len() as u64));
                progress.push(done);
            }
            FileEvent::Finished { id, bytes } => {
                assert_eq!(id, 42);
                break bytes;
            }
            other => panic!("a transfer reported {other:?}"),
        }
    };
    assert_eq!(bytes as usize, payload.len());
    assert_eq!(progress.first(), Some(&0), "a bar starts at zero");
    assert!(
        progress.windows(2).all(|pair| pair[0] <= pair[1]),
        "progress cannot go backwards: {progress:?}"
    );
    // Three megabytes is ninety-odd chunks. Throttled, it must be far fewer events than that.
    assert!(
        progress.len() < 50,
        "{} progress events for three megabytes is the flood throttling exists to stop",
        progress.len()
    );

    session
        .send(FileCommand::List(root.clone()))
        .expect("the session is open");
    match next(&events) {
        FileEvent::Listing { path, entries } => {
            assert_eq!(path, root);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "payload");
            assert_eq!(entries[0].kind, EntryKind::File);
            assert_eq!(entries[0].size, payload.len() as u64);
        }
        other => panic!("listing reported {other:?}"),
    }

    // A refusal is an event, not a panic and not silence: deleting a directory as a file fails.
    session
        .send(FileCommand::Remove {
            path: root.clone(),
            directory: false,
        })
        .expect("the session is open");
    match next(&events) {
        FileEvent::Failed { what, why } => {
            assert!(what.contains("deleting"), "{what}");
            assert!(!why.trim().is_empty(), "a refusal has to say something");
        }
        other => panic!("deleting a directory as a file reported {other:?}"),
    }

    // And the session survives it, which is the point of reporting rather than closing.
    session
        .send(FileCommand::Remove {
            path: remote,
            directory: false,
        })
        .expect("the session is open");
    assert!(matches!(next(&events), FileEvent::Done(_)));
    session
        .send(FileCommand::Remove {
            path: root,
            directory: true,
        })
        .expect("the session is open");
    assert!(matches!(next(&events), FileEvent::Done(_)));

    assert!(
        woken.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "nothing asked for a repaint, so nothing would have drawn any of this"
    );

    session
        .send(FileCommand::Shutdown)
        .expect("the session is open");
    assert!(matches!(next(&events), FileEvent::Closed));

    let _ = std::fs::remove_file(&local);
    connection.disconnect().await;
}

/// The next event, or a failure that says what was being waited for.
///
/// A timeout rather than a blocking receive: a test that hangs tells nobody anything, and every
/// operation here is against a server on loopback.
fn next(events: &crossbeam_channel::Receiver<FileEvent>) -> FileEvent {
    events
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the session stopped reporting")
}

/// Fetch a remote file's bytes, through a download to a scratch path.
///
/// The comparison the size assertions cannot make: a wrong offset that happens to leave the right
/// length -- a tail written over itself, say -- passes on size and fails here.
async fn read_remote(sftp: &Sftp, remote: &str, tag: &str) -> Vec<u8> {
    let scratch = std::env::temp_dir().join(format!("bestterm-verify-{tag}"));
    let _ = std::fs::remove_file(&scratch);
    sftp.download(remote, &scratch, false, &mut |_, _| {})
        .await
        .expect("the verification download failed");
    let bytes = std::fs::read(&scratch).expect("the verification file could not be read");
    let _ = std::fs::remove_file(&scratch);
    bytes
}

/// Write a local scratch file and return its path.
fn write_scratch(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, bytes).expect("the local scratch file could not be written");
    path
}
