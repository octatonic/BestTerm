//! A real RDP server, when one is offered.
//!
//! Everything else about RDP is tested against itself: the codec round-trips, the active stage
//! compiles against IronRDP's own types, the process boundary is exercised against a closed port.
//! None of that can catch what the parts assume about each other, and until this test has run against
//! a live server, "RDP works" is a statement about code that has never met a server.
//!
//! # It takes its target from the environment
//!
//! Deliberately, and not from a constant. A host name in a test file is somebody's infrastructure
//! committed to a public repository, and this one is run against machines belonging to whoever is
//! running it. Set `BESTTERM_RDP_TEST_HOST` to enable it; `BESTTERM_RDP_TEST_USER`,
//! `BESTTERM_RDP_TEST_PASSWORD`, `BESTTERM_RDP_TEST_DOMAIN` and `BESTTERM_RDP_TEST_PORT` are
//! optional.
//!
//! # What it accepts as success
//!
//! Two different things, and it says which happened.
//!
//! With credentials: a frame. That is the whole path — TCP, TLS, the server's key, CredSSP, capability
//! exchange, channel joining, a decoded bitmap, shared memory, and the host reading it back.
//!
//! Without them: reaching authentication and being refused there. That still proves everything up to
//! the credential, which is most of what has never been exercised, and it is the only thing a test
//! can do when nobody has handed it a password. Being refused at the login is reported as a pass with
//! a note, because failing it would mean the test could only ever run on one person's machine.
//!
//! What is *not* success is silence, a helper that dies without explaining itself, or a frame that
//! arrives before the server's key was settled.

use std::time::{Duration, Instant};

use bestterm_ipc_frame::ConnectRequest;
use bestterm_surface::{FrameSize, GraphicalSurface, SurfaceEvent, SurfaceKind};

/// How long the whole exchange gets.
///
/// Generous: a cold helper start, a TLS handshake and a first full-screen bitmap on a loaded network
/// is seconds, and a flaky test about a real server is worse than a slow one.
const PATIENCE: Duration = Duration::from_secs(45);

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

#[test]
fn a_real_server_hands_over_a_desktop() {
    let Some(host) = env("BESTTERM_RDP_TEST_HOST") else {
        println!(
            "SKIPPED: set BESTTERM_RDP_TEST_HOST (and optionally _USER, _PASSWORD, _DOMAIN, _PORT) \
             to run this against a real server."
        );
        return;
    };
    let Some(helper) = find_helper() else {
        println!(
            "SKIPPED: bestterm-rdp is not built. \
             Run `cargo build --manifest-path helpers/rdp/Cargo.toml --workspace`."
        );
        return;
    };

    let port: u16 = env("BESTTERM_RDP_TEST_PORT")
        .and_then(|p| p.parse().ok())
        .unwrap_or(3389);
    let username = env("BESTTERM_RDP_TEST_USER").unwrap_or_default();
    let password = env("BESTTERM_RDP_TEST_PASSWORD").unwrap_or_default();
    let have_credentials = !username.is_empty() && !password.is_empty();

    let request = ConnectRequest {
        host: host.clone(),
        port,
        username,
        domain: env("BESTTERM_RDP_TEST_DOMAIN"),
        password: bestterm_core_vault::Secret::new(password),
        desktop_size: FrameSize::new(1280, 800),
        enable_credssp: true,
        keyboard_layout: 0,
        client_name: "bestterm-test".to_string(),
        // Nothing on record, so the helper asks and this test answers. That is the path a first
        // connection takes, which is the one worth exercising.
        known_server_key: None,
    };

    let (mut surface, events) = bestterm_helper_surface::connect(
        &helper,
        SurfaceKind::Rdp,
        format!("live {host}"),
        request,
        std::sync::Arc::new(|| {}),
    )
    .expect("the helper starts");

    let mut asked_about_key = false;
    let mut settled_key = false;
    let mut resized = None;
    let mut errors: Vec<String> = Vec::new();
    let started = Instant::now();

    let outcome = loop {
        let left = PATIENCE.saturating_sub(started.elapsed());
        if left.is_zero() {
            panic!("nothing conclusive in {PATIENCE:?}; errors so far: {errors:?}");
        }

        match events.recv_timeout(left) {
            Ok(SurfaceEvent::AskAboutServerKey {
                fingerprint,
                expected,
                ..
            }) => {
                println!("server key: {fingerprint} (expected {expected:?})");
                asked_about_key = true;
                // Accepted, because this is a first connection to a machine the operator chose. A
                // reconnect must not do this; see `docs/ROADMAP.md`.
                surface
                    .answer_server_key(true)
                    .expect("the helper is listening");
            }
            Ok(SurfaceEvent::ServerKeySettled { fingerprint, store }) => {
                println!("settled on {fingerprint} (store: {store})");
                settled_key = true;
            }
            Ok(SurfaceEvent::Resized(size)) => resized = Some(size),
            Ok(SurfaceEvent::Frame(meta)) => break Some(meta),
            Ok(SurfaceEvent::Cursor(_) | SurfaceEvent::ClipboardOffer(_)) => {}
            Ok(SurfaceEvent::Error(detail)) => {
                println!("error: {detail}");
                errors.push(detail);
            }
            Ok(SurfaceEvent::Closed { reason }) => {
                let reason = reason.unwrap_or_else(|| "no reason given".to_string());
                println!("closed: {reason}");
                break None;
            }
            Err(_) => panic!("nothing conclusive in {PATIENCE:?}; errors: {errors:?}"),
        }
    };

    // The order matters and is the reason this is asserted rather than merely observed: the key is
    // settled before a credential is sent, so a frame that arrived without one would mean a password
    // went to a server nobody looked at.
    if outcome.is_some() {
        assert!(
            asked_about_key || settled_key,
            "a frame arrived without the server's key ever being settled"
        );
    }

    match outcome {
        Some(meta) => {
            println!(
                "FRAME: {}x{} stride {} {:?}, generation {}, {} damage rect(s)",
                meta.size.width,
                meta.size.height,
                meta.stride,
                meta.format,
                meta.generation,
                meta.damage.len()
            );
            assert!(meta.size.width > 0 && meta.size.height > 0);
            assert!(
                meta.stride >= meta.size.width * 4,
                "a row cannot be shorter than its pixels"
            );

            // And the pixels are actually there, in the mapping, readable from this process.
            let mut seen = 0usize;
            let mut non_zero = 0usize;
            surface.with_frame(&mut |actual, pixels| {
                seen = pixels.len();
                non_zero = pixels.iter().filter(|byte| **byte != 0).count();
                assert_eq!(actual.generation, meta.generation);
            });
            println!("{seen} bytes readable, {non_zero} of them non-zero");
            assert!(
                seen > 0,
                "the frame was announced but the mapping was empty"
            );
            assert!(
                non_zero > 0,
                "every byte was zero, so nothing was actually decoded"
            );
            if let Some(size) = resized {
                println!("server renegotiated to {}x{}", size.width, size.height);
            }
        }
        None => {
            // Refused is a pass without credentials and a failure with them: with a password, being
            // turned away means the password path is broken, which is exactly what this would be
            // testing.
            assert!(
                !have_credentials,
                "credentials were supplied and the session still closed: {errors:?}"
            );
            println!(
                "PARTIAL PASS: reached the server and was refused before a desktop. Everything up \
                 to the credential worked; set BESTTERM_RDP_TEST_USER and _PASSWORD to go further."
            );
            assert!(
                asked_about_key,
                "the helper never asked about the server's key, so TLS never came up"
            );
        }
    }

    surface.shutdown().expect("shutting down is allowed");
}

/// Find the built helper, wherever the two profiles put it.
fn find_helper() -> Option<std::path::PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join("helpers/rdp/target");
    let name = format!("bestterm-rdp{}", std::env::consts::EXE_SUFFIX);
    ["debug", "release"]
        .iter()
        .map(|profile| root.join(profile).join(&name))
        .find(|path| path.is_file())
}
