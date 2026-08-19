//! BestTerm.
//!
//! A native remote-access workspace for Linux and Windows.

// A release build must not open a console window behind the GUI on Windows. Debug builds keep it,
// because that is where the log goes during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use bestterm_app_ui::{BestTermApp, DEFAULT_WINDOW_SIZE, Startup};

/// Smallest window that still shows a usable grid alongside the chrome.
const MIN_WINDOW_SIZE: [f32; 2] = [720.0, 460.0];

fn main() -> eframe::Result {
    init_tracing();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting BestTerm");

    if let Err(missing) = check_shared_libraries() {
        // Deliberately on stderr and not through `tracing`: this is the one message somebody who
        // cannot start the program at all has to see, and the log may be going anywhere.
        eprintln!("{missing}");
        std::process::exit(1);
    }

    let startup = parse_arguments();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("BestTerm")
            .with_inner_size(DEFAULT_WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE),
        ..Default::default()
    };

    eframe::run_native(
        "BestTerm",
        options,
        Box::new(move |_cc| Ok(Box::new(BestTermApp::with_startup(startup)))),
    )
}

/// Check for the shared libraries whose absence aborts rather than degrades.
///
/// This exists because of what happens without it. On a Linux machine running X11 without
/// `libxkbcommon-x11`, `winit` panics inside a dependency while the event loop is being built --
/// and because release builds abort on panic, what a person sees is:
///
/// ```text
/// Aborted (core dumped)
/// ```
///
/// A window that never appears and a core file. Nothing about which library, nothing about which
/// package. It cannot be caught: `catch_unwind` does not see an abort, and the panic is three crates
/// down inside code we do not call directly. So it is checked before anything can touch it.
///
/// Only the libraries that are loaded by name at run time, and only for the display server that is
/// actually going to be used: on a Wayland session the X11 library is never opened, and demanding it
/// there would refuse to start a program that would have worked.
#[cfg(target_os = "linux")]
fn check_shared_libraries() -> Result<(), String> {
    // Wayland wins when both are set, which is what `winit` does: a session with `WAYLAND_DISPLAY`
    // set is a Wayland session, and `DISPLAY` beside it is XWayland for programs that need it.
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = !wayland && std::env::var_os("DISPLAY").is_some();

    let mut needed: Vec<(&str, &str)> = vec![("libxkbcommon.so.0", "libxkbcommon0")];
    if x11 {
        needed.push(("libxkbcommon-x11.so.0", "libxkbcommon-x11-0"));
    }

    let mut missing: Vec<(&str, &str)> = Vec::new();
    for (library, package) in needed {
        // SAFETY: opening a library runs its initialisers, which for these is nothing but setting up
        // tables. They are the same libraries the windowing layer is about to open by the same names;
        // doing it here first only moves the failure somewhere it can be explained.
        let opened = unsafe { libloading::Library::new(library) };
        if opened.is_err() {
            missing.push((library, package));
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    let mut message =
        String::from("BestTerm cannot start: a shared library it needs is not installed.\n");
    for (library, package) in &missing {
        message.push_str(&format!("  {library} (Debian and Ubuntu: {package})\n"));
    }
    // Named per distribution because the package names differ and the plan commits to Ubuntu, Debian
    // and Arch. Somebody reading this is stuck, and "install the right package" is not help.
    message.push_str(
        "\nInstall it with one of:\n\
         \x20 Debian, Ubuntu:  sudo apt install libxkbcommon0 libxkbcommon-x11-0\n\
         \x20 Arch:            sudo pacman -S libxkbcommon libxkbcommon-x11\n\
         \x20 Fedora:          sudo dnf install libxkbcommon libxkbcommon-x11\n",
    );
    Err(message)
}

/// Nothing to check.
///
/// Windows resolves what it needs at load time: a missing DLL is reported by the loader, with the
/// name of the file, before `main` runs -- which is the message this function exists to produce on
/// the platform that does not.
#[cfg(not(target_os = "linux"))]
fn check_shared_libraries() -> Result<(), String> {
    Ok(())
}

/// Read the command line.
///
/// One positional argument, a session to open: `bestterm admin@srv.int:2222`, and two options:
/// `--import <file>`, which reads a `.mxtsessions` export into the session tree, and `--self-check`,
/// which opens the window, paints a few frames and exits. Anything unrecognised is reported and
/// ignored rather than refused, because a terminal that will not start because of a typo in its
/// arguments is worse than one that starts without the session.
fn parse_arguments() -> Startup {
    let mut startup = Startup::default();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--self-check" {
            // Enough frames that the first one -- which opens a shell, measures the font and builds
            // the theme -- is not the only one counted. A renderer that fails does so on the first
            // draw; a layout that panics on a second pass needs a second pass to show it.
            startup.self_check = Some(5);
        } else if argument == "--import" {
            match arguments.next() {
                Some(path) => startup.import = Some(std::path::PathBuf::from(path)),
                None => tracing::warn!("--import needs a path"),
            }
        } else if argument.starts_with('-') {
            tracing::warn!(argument, "unknown option; ignored");
        } else if startup.connect.is_none() {
            startup.connect = Some(argument);
        } else {
            tracing::warn!(
                argument,
                "only one session can be opened from the command line"
            );
        }
    }
    startup
}

/// Set up logging.
///
/// Defaults to `info` for BestTerm's own crates and `warn` for everything else, so the graphics
/// stack does not drown the log. Override with `RUST_LOG`.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,bestterm=info,bestterm_app_ui=info"));

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_level(true)
        .init();
}
