//! The process boundary, exercised end to end against the real helper binary.
//!
//! Everything else about this boundary is tested on one side or the other: the codec round-trips in
//! `ipc-frame`, the framing round-trips in `ipc-frame::wire`, the active stage is tested against
//! IronRDP's own types. None of that proves the two processes agree, because agreement is exactly
//! what unit tests on either side cannot see — both can be self-consistent and wrong together.
//!
//! So this launches the actual `bestterm-rdp` and talks to it.
//!
//! # Why it aims at a closed port
//!
//! Because that is the only thing about a server that can be arranged from a test. It still covers
//! everything the boundary is made of: the helper starts, decodes a `Connect` written by this crate,
//! fails to reach anything, encodes a `Closed` this crate decodes, and exits. A working RDP server
//! would test IronRDP, which is not what is in doubt here.
//!
//! # Skipping is loud
//!
//! The helper is a different cargo workspace, so it cannot be a dependency and may simply not be
//! built. When it is missing the test says so and passes, because failing would mean nobody could run
//! `cargo test` without building both trees — but it says so on stdout rather than quietly, since a
//! test that silently stops testing is worse than one that is absent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bestterm_ipc_frame::ConnectRequest;
use bestterm_surface::{FrameSize, GraphicalSurface, SurfaceEvent, SurfaceKind};

/// How long the helper gets to fail to connect.
///
/// Generous: a refused connection is immediate, but a cold process start on a loaded CI runner is
/// not, and a flaky test about process startup would be worse than a slow one.
const PATIENCE: Duration = Duration::from_secs(30);

/// Find the built helper, wherever the two profiles put it.
fn find_helper() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join("helpers/rdp/target");
    let name = format!("bestterm-rdp{}", std::env::consts::EXE_SUFFIX);
    ["debug", "release"]
        .iter()
        .map(|profile| root.join(profile).join(&name))
        .find(|path| path.is_file())
}

fn request(port: u16) -> ConnectRequest {
    ConnectRequest {
        // Loopback, and a port nothing listens on. Nothing leaves this machine.
        host: "127.0.0.1".to_string(),
        port,
        username: "nobody".to_string(),
        domain: None,
        password: bestterm_core_vault::Secret::new("not a real password".to_string()),
        desktop_size: FrameSize::new(1024, 768),
        enable_credssp: true,
        keyboard_layout: 0,
        client_name: "bestterm-test".to_string(),
        known_server_key: None,
    }
}

#[test]
fn the_helper_starts_reads_our_request_and_reports_back() {
    let Some(helper) = find_helper() else {
        println!(
            "SKIPPED: bestterm-rdp is not built. \
             Run `cargo build --manifest-path helpers/rdp/Cargo.toml --workspace` to include this."
        );
        return;
    };

    let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (mut surface, events) = bestterm_helper_surface::connect(
        &helper,
        SurfaceKind::Rdp,
        "boundary test".to_string(),
        // Port 1 is reserved and nothing binds it, so this is refused rather than answered.
        request(1),
        // The wake-ups are counted rather than drawn, which is the one thing a test can check about
        // them: a surface that never asks for a repaint is a tab that stays blank until the mouse
        // moves over it, and that bug has already happened once on the terminal side.
        {
            let wakes = std::sync::Arc::clone(&wakes);
            std::sync::Arc::new(move || {
                wakes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        },
    )
    .expect("the helper starts");

    assert_eq!(surface.kind(), SurfaceKind::Rdp);
    assert_eq!(surface.label(), "boundary test");

    // Everything the helper says, until it says it is closed.
    let mut seen = Vec::new();
    let closed = loop {
        match events.recv_timeout(PATIENCE) {
            Ok(SurfaceEvent::Closed { reason }) => break reason,
            Ok(other) => seen.push(other),
            Err(_) => panic!("the helper said nothing in {PATIENCE:?}; saw {seen:?}"),
        }
    };

    // The point of the assertion: the helper's failure travelled across the boundary as words, not
    // as a silence. A helper that dies without explaining itself is a tab that looks alive forever.
    let reason = closed.expect("a refused connection has a reason");
    assert!(
        !reason.trim().is_empty(),
        "the reason must say something: {reason:?}"
    );
    assert!(
        seen.iter()
            .all(|event| !matches!(event, SurfaceEvent::Frame(_))),
        "nothing was connected, so nothing should have drawn: {seen:?}"
    );

    assert!(
        wakes.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the surface has to ask for a repaint, or nothing would ever draw what it reported"
    );

    // Idempotent, and safe on a helper that has already exited.
    surface.shutdown().expect("shutting down twice is allowed");
    surface.shutdown().expect("shutting down twice is allowed");
}

#[test]
fn a_missing_helper_is_an_error_and_not_a_panic() {
    // The path is built from `current_exe`, so it is normally right; this is about the installation
    // where it is not, which must produce something a person can read rather than a crash.
    let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("no-such-helper");
    let error = bestterm_helper_surface::connect(
        &missing,
        SurfaceKind::Rdp,
        "missing".to_string(),
        request(1),
        std::sync::Arc::new(|| {}),
    )
    .expect_err("there is no helper there");
    assert!(
        error.to_string().to_lowercase().contains("not found")
            || error.to_string().to_lowercase().contains("cannot find"),
        "{error}"
    );
}
