//! The application shell: window layout, tabs, and the wiring between input, transport and emulator.
//!
//! This is the only crate that knows about all the others. Everything below it is independently
//! testable, which is the point — see `docs/ARCHITECTURE.md`.

mod keymap;
mod pane;
mod ssh;
mod surface_tab;
mod tab;
mod tunnels;
mod vault;

use bestterm_config::ConfigStore;
use bestterm_core_model::{NodeId, NodeKind, ProtocolConfig, SessionTree};
use bestterm_core_pty::{ShellProfile, discover};
use bestterm_core_terminal::{Palette, TerminalEmulator};
use bestterm_term_render::keys::{self, TermKey};
use bestterm_term_render::{TerminalMetrics, TerminalStyle};
use bestterm_transport::GridSize;
use bestterm_ui_chrome::{
    ChromeAction, ChromeState, ChromeTheme, DialogOutcome, SessionDialog, SidebarPanel, StatusInfo,
    TabInfo, apply_theme, macros_panel, menu_bar, quick_connect_field, ribbon, session_dialog,
    sidebar_strip, status_bar, tab_bar, tools_panel,
};
use egui::{CentralPanel, CornerRadius, EventFilter, Frame, Panel, Sense, Stroke};

use crate::ssh::{HostKeyQuestion, HostKeyRecord, HostKeyVerdict, SessionEvent};
use crate::tab::TerminalTab;
use crate::vault::{PendingUnlock, Prompt, VaultState};

/// Environment variable that puts the interface into a named state at startup.
///
/// Parity is judged by comparing screenshots against the reference, and a screenshot of the session
/// dialog needs the session dialog open. Driving synthetic mouse clicks at somebody's desktop to get
/// there is both unreliable and rude, so the state is nameable instead:
///
/// ```sh
/// BESTTERM_UI_STATE=session-dialog bestterm
/// ```
///
/// Understood values are `session-dialog`, `tools` and `macros`. Anything else is ignored, silently,
/// because a typo here should not stop the application from starting.
const UI_STATE_VARIABLE: &str = "BESTTERM_UI_STATE";

/// Scrollback lines kept per tab.
///
/// 10 000 is `alacritty_terminal`'s own default and a reasonable compromise; it becomes a
/// configuration setting in phase 1.
const SCROLLBACK: usize = 10_000;

/// The RDP helper's file name, without a platform suffix.
const RDP_HELPER: &str = "bestterm-rdp";

/// The VNC helper's.
const VNC_HELPER: &str = "bestterm-vnc";

/// Where accepted RDP server keys are recorded.
///
/// Its own file rather than a section of the configuration: it is a log that is appended to, people
/// edit it by hand when a server is rebuilt, and mixing it into a file this program rewrites would
/// mean their edits competing with its serialisation.
const RDP_KNOWN_SERVERS: &str = "known_servers";

/// The application.
pub struct BestTermApp {
    theme: ChromeTheme,
    term_style: TerminalStyle,
    metrics: TerminalMetrics,
    chrome: ChromeState,
    tabs: Vec<pane::Pane>,
    shells: Vec<ShellProfile>,
    palette: Palette,
    theme_installed: bool,
    /// Whether the shell that opens at startup has been opened.
    ///
    /// It cannot happen in the constructor: a tab needs something to wake when its output arrives,
    /// and that only exists once there is an interface. See [`BestTermApp::open_shell`].
    opened_first_shell: bool,
    /// What the command line asked for, acted on once the window exists.
    startup: Startup,
    /// Where configuration lives, or `None` when no home directory could be found.
    ///
    /// Absent is survivable: the application runs and forgets, which is better than refusing to start
    /// because a directory could not be created.
    store: Option<ConfigStore>,
    /// Saved sessions.
    tree: SessionTree,

    /// The Session settings dialog, whether or not it is on screen.
    dialog: SessionDialog,
    /// Where network work happens.
    ///
    /// Held for the whole life of the application: dropping a runtime waits for its tasks, which on the
    /// interface thread would freeze the window.
    runtime: tokio::runtime::Runtime,
    /// Outcomes of connection attempts, drained each frame.
    sessions: (
        crossbeam_channel::Sender<SessionEvent>,
        crossbeam_channel::Receiver<SessionEvent>,
    ),
    /// The host key question currently on screen, if any.
    ///
    /// One at a time. Two prompts about two different servers, stacked, is how somebody accepts the
    /// wrong one.
    pending_host_key: Option<HostKeyQuestion>,
    /// Connection failures worth showing, newest last.
    notices: Vec<String>,
    /// Stored credentials, and the state of the prompt over them.
    vault: VaultState,
    /// A session that was waiting for the vault to open.
    pending_session: Option<bestterm_core_model::SshConfig>,
    /// SSH connections with at least one tab open, and what to call them.
    ///
    /// Held here as well as in the tabs because a tunnel outlives the tab that started it and has to
    /// be able to name the connection it runs over. Pruned in [`BestTermApp::close_tab`], which is
    /// the only thing that can know a connection has no windows left.
    connections: Vec<tunnels::LiveConnection>,
    /// Where the next connection's id comes from.
    ///
    /// Never reused, so a tunnel cannot end up pointing at a different session than the one it was
    /// opened over.
    next_connection: u64,
    /// Port forwarding: the window, the form and what is running.
    tunnels: tunnels::TunnelState,
    /// The Configuration dialog, whether or not it is on screen.
    configuration: bestterm_ui_chrome::configuration::Configuration,
}

impl Default for BestTermApp {
    fn default() -> Self {
        Self::new()
    }
}

/// A session named on the command line.
///
/// MobaXterm can be told to open a session when it starts, and a terminal that cannot is a terminal
/// people have to click through every morning. It is also the only way to exercise the connection path
/// without driving synthetic input, which is what makes the screenshot tests in `docs/ui-parity.md`
/// possible at all.
#[derive(Clone, Debug, Default)]
pub struct Startup {
    /// `user@host:port` to open once the window exists.
    pub connect: Option<String>,
    /// A `.mxtsessions` file to import into the session tree.
    pub import: Option<std::path::PathBuf>,
}

impl BestTermApp {
    /// Build the application.
    ///
    /// No tab is opened here. Opening one needs something to wake when its output arrives, and that
    /// only exists once the interface does — so the first shell opens on the first frame instead.
    pub fn new() -> Self {
        Self::with_startup(Startup::default())
    }

    /// Build the application, opening whatever the command line asked for.
    pub fn with_startup(startup: Startup) -> Self {
        let shells = discover();
        tracing::info!(count = shells.len(), "discovered local shells");

        // `BESTTERM_CONFIG_DIR` puts everything under one directory. The docstring on
        // `Paths::rooted_at` already anticipated it for portable installations; it is used here to
        // investigate interface behaviour without a real inventory of sessions in the way.
        let paths = match std::env::var_os("BESTTERM_CONFIG_DIR") {
            Some(dir) => Some(bestterm_config::Paths::rooted_at(dir)),
            None => bestterm_config::Paths::discover(),
        };
        let store = paths.map(ConfigStore::new);
        let tree = match &store {
            Some(store) => match store.load_tree() {
                Ok(tree) => tree,
                Err(error) => {
                    // A tree that cannot be read is reported and replaced with an empty one rather
                    // than being fatal: somebody with a corrupt file still needs a terminal, and
                    // nothing here overwrites the file until they change something.
                    tracing::warn!(%error, "could not read the saved sessions");
                    SessionTree::new()
                }
            },
            None => SessionTree::new(),
        };
        tracing::info!(sessions = tree.walk().len(), "loaded the session tree");

        Self {
            theme: ChromeTheme::light(),
            term_style: TerminalStyle::default(),
            // Replaced with a real measurement on the first frame, once fonts exist.
            metrics: TerminalMetrics {
                cell_width: 8.0,
                cell_height: 16.0,
            },
            chrome: ChromeState::default(),
            tabs: Vec::new(),
            shells,
            palette: Palette::xterm(),
            theme_installed: false,
            opened_first_shell: false,
            startup,
            store,
            tree,
            dialog: SessionDialog::default(),
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("bestterm-net")
                .build()
                .expect("a tokio runtime"),
            sessions: crossbeam_channel::unbounded(),
            pending_host_key: None,
            notices: Vec::new(),
            vault: VaultState::default(),
            pending_session: None,
            connections: Vec::new(),
            next_connection: 1,
            tunnels: tunnels::TunnelState::default(),
            configuration: bestterm_ui_chrome::configuration::Configuration::default(),
        }
    }

    /// Open a tab running `shells[index]`, or the first shell if the index is out of range.
    ///
    /// `ctx` is what the tab's relay wakes when output arrives.
    fn open_shell(&mut self, index: usize, ctx: &egui::Context) {
        let Some(profile) = self.shells.get(index).or_else(|| self.shells.first()) else {
            tracing::error!("no shells available; cannot open a tab");
            return;
        };

        // A conventional size for the moment between opening and the first frame, which measures
        // the window and resizes the tab to fit it.
        let (cols, rows) = (80, 24);
        let waker = {
            let ctx = ctx.clone();
            std::sync::Arc::new(move || ctx.request_repaint()) as crate::tab::Waker
        };
        match TerminalTab::spawn(profile, cols, rows, SCROLLBACK, self.palette.clone(), waker) {
            Ok(tab) => {
                self.tabs.push(pane::Pane::Terminal(Box::new(tab)));
                self.chrome.active_tab = self.tabs.len() - 1;
            }
            Err(err) => {
                // Phase 1 surfaces this in the UI. Logging it is the honest minimum for now.
                tracing::error!(shell = %profile.id, %err, "failed to open shell");
            }
        }
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let mut tab = self.tabs.remove(index);
        let connection = tab.connection();
        tab.shutdown();

        // A connection ends when its last window does. Anything still running over it -- today the
        // tunnels, tomorrow an SFTP panel -- goes with it, because a forward that outlived every
        // window that could show it is a listening socket nobody knows about, still carrying traffic
        // into a network somebody believes they have left.
        if let Some(id) = connection
            && !self.tabs.iter().any(|other| other.connection() == Some(id))
        {
            self.tunnels.stop_all_over(&self.runtime, id);
            self.connections.retain(|live| live.id != id);
        }

        if self.chrome.active_tab >= self.tabs.len() {
            self.chrome.active_tab = self.tabs.len().saturating_sub(1);
        }
    }

    /// Move output from every transport into its emulator, and answers back the other way.
    ///
    /// Returns true if anything changed and the UI should repaint.
    fn pump(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        for tab in &mut self.tabs {
            changed |= tab.pump(ctx);
        }
        changed
    }

    fn apply_actions(&mut self, actions: Vec<ChromeAction>, ctx: &egui::Context) {
        for action in actions {
            match action {
                ChromeAction::NewLocalShell => self.open_shell(0, ctx),
                ChromeAction::SelectTab(index) if index < self.tabs.len() => {
                    self.chrome.active_tab = index;
                }
                ChromeAction::SelectTab(_) => {}
                ChromeAction::CloseTab(index) => self.close_tab(index),
                ChromeAction::OpenConfiguration => self.configuration.open = true,
                ChromeAction::ReconnectTab(index) => self.reconnect_tab(index, ctx),
                ChromeAction::OpenTunnels => {
                    self.tunnels.open = true;
                    // Pre-selected when there is only one candidate, because choosing between one
                    // thing is not a choice.
                    if self.tunnels.form.over.is_none()
                        && let [only] = self.connections.as_slice()
                    {
                        self.tunnels.form.over = Some(only.id);
                    }
                }
                ChromeAction::ToggleSidebar => {
                    self.chrome.sidebar_open = !self.chrome.sidebar_open;
                }
                ChromeAction::SelectSidebarPanel(panel) => self.chrome.sidebar_panel = panel,
                ChromeAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                ChromeAction::QuickConnect(target) => {
                    match parse_quick_connect(&target) {
                        Some(config) => self.connect_ssh(config, ctx),
                        None => self
                            .notices
                            .push(format!("could not read '{target}' as user@host:port")),
                    }
                    self.chrome.quick_connect.clear();
                }
                ChromeAction::OpenSessionDialog => self.dialog.open_fresh(),
                ChromeAction::Unimplemented(what) => {
                    tracing::info!(control = what, "not implemented yet");
                }
            }
        }
    }

    /// Start an SSH session in the background.
    ///
    /// Returns immediately: a connection takes as long as a network does, and the frame loop cannot
    /// wait for one. The outcome arrives on [`Self::sessions`] and is picked up by a later frame.
    fn connect_ssh(&mut self, config: bestterm_core_model::SshConfig, ctx: &egui::Context) {
        use bestterm_core_model::SshAuth;
        use bestterm_proto_ssh::Auth;

        // Resolved before anything is spawned, because a locked vault means asking a question rather
        // than starting a connection that would fail on the far side of a network round trip.
        let auth = match &config.auth {
            SshAuth::Agent => Auth::Agent,
            SshAuth::Password { credential: None } => {
                // The session says "password" and names no entry, which is what an imported session
                // looks like when its password was never brought across.
                self.notices.push(format!(
                    "{} has no stored password; add one or use an agent key",
                    config.host
                ));
                return;
            }
            SshAuth::Password {
                credential: Some(credential),
            } => {
                let name = credential.key().to_owned();
                match self.vault.get(&name) {
                    Some(secret) => Auth::Password(secret),
                    None if !self.vault.is_open() => {
                        self.vault
                            .ask(self.store.as_ref(), Some(PendingUnlock::Session));
                        self.pending_session = Some(config);
                        return;
                    }
                    None => {
                        self.notices
                            .push(format!("the vault holds no entry called '{name}'"));
                        return;
                    }
                }
            }
            SshAuth::PublicKey { path, .. } => Auth::PrivateKeyFile {
                path: std::path::PathBuf::from(path),
                // A passphrase from the vault comes with key authentication proper; a key without one
                // works today and is the common case for a key an agent is not holding.
                passphrase: None,
            },
            other => {
                self.notices.push(format!(
                    "{} authentication is not wired up yet",
                    match other {
                        SshAuth::KeyboardInteractive => "keyboard-interactive",
                        _ => "this",
                    }
                ));
                return;
            }
        };

        let waker = {
            let ctx = ctx.clone();
            std::sync::Arc::new(move || ctx.request_repaint())
                as std::sync::Arc<dyn Fn() + Send + Sync>
        };
        let size = self
            .tabs
            .get(self.chrome.active_tab)
            .map(|tab| {
                let (cols, rows) = tab.grid();
                GridSize::new(cols as u16, rows as u16)
            })
            .unwrap_or(GridSize::new(80, 24));

        tracing::info!(host = %config.host, port = config.port, "connecting");
        ssh::connect(
            self.runtime.handle(),
            config,
            auth,
            read_known_hosts(),
            size,
            self.sessions.0.clone(),
            waker,
        );
    }

    /// Open a serial port.
    ///
    /// Synchronous, and not on the runtime: opening a port is a system call rather than a network
    /// round trip, and the thread that reads it is the port's own. Spawning a task to do a `open()`
    /// would add a hop and hide where the blocking is.
    fn open_serial(&mut self, config: &bestterm_core_model::SerialConfig, ctx: &egui::Context) {
        let waker = {
            let ctx = ctx.clone();
            std::sync::Arc::new(move || ctx.request_repaint()) as crate::tab::Waker
        };

        match bestterm_proto_serial::SerialTransport::open(config) {
            Ok(open) => {
                let title = open.transport.label();
                let tab = TerminalTab::adopt(crate::tab::NewTab {
                    open,
                    title,
                    cols: 80,
                    rows: 24,
                    scrollback: SCROLLBACK,
                    palette: self.palette.clone(),
                    waker,
                    // A port owns itself; there is nothing underneath it to keep alive.
                    owner: None,
                });
                self.tabs.push(pane::Pane::Terminal(Box::new(tab)));
                self.chrome.active_tab = self.tabs.len() - 1;
            }
            Err(error) => self.notices.push(error.to_string()),
        }
    }

    /// Open a telnet session.
    ///
    /// No credential and no vault: telnet has no authentication of its own, and the login prompt is
    /// just more of the same byte stream. The warning about that is raised by `proto-telnet` at the
    /// moment it becomes true and repeated here, because somebody about to type a password into a
    /// switch should be told before they do rather than after.
    fn connect_telnet(&mut self, config: bestterm_core_model::TelnetConfig, ctx: &egui::Context) {
        let label = format!("{}:{}", config.host, config.port);
        self.notices.push(format!(
            "{label}: telnet is not encrypted — anything typed into it travels in clear text"
        ));

        let events = self.sessions.0.clone();
        let waker = {
            let ctx = ctx.clone();
            std::sync::Arc::new(move || ctx.request_repaint())
                as std::sync::Arc<dyn Fn() + Send + Sync>
        };
        let size = bestterm_transport::GridSize::new(80, 24);

        self.runtime.spawn(async move {
            let event = match bestterm_proto_telnet::TelnetTransport::open(
                &config.host,
                config.port,
                "xterm-256color",
                size,
            )
            .await
            {
                Ok(open) => ssh::SessionEvent::Opened {
                    title: label,
                    open: Box::new(open),
                    // Telnet has no session object under the channel: the transport is the whole of
                    // it, so there is nothing to keep alive beside it and nothing to reconnect with.
                    session: None,
                    record: None,
                    reconnect: Err(bestterm_proto_ssh::NotReconnectable::Interactive),
                    target: None,
                },
                Err(error) => ssh::SessionEvent::Failed {
                    title: label,
                    reason: error.to_string(),
                },
            };
            let _ = events.send(event);
            waker();
        });
    }

    /// Open a remote desktop by launching the helper process.
    ///
    /// Unlike SSH, none of this happens on the runtime: the helper is a process, and everything slow
    /// about the connection happens inside it. What is done here is a spawn and a write.
    fn connect_rdp(&mut self, config: bestterm_core_model::RdpConfig, ctx: &egui::Context) {
        let helper = match bestterm_helper_surface::helper_path(RDP_HELPER) {
            Ok(path) if path.is_file() => path,
            Ok(path) => {
                // Said with the path in it. "The helper is missing" sends somebody looking in the
                // wrong place; the path says which directory is short of a file.
                self.notices.push(format!(
                    "the RDP helper is not installed beside this program (looked for {})",
                    path.display()
                ));
                return;
            }
            Err(error) => {
                self.notices.push(format!(
                    "could not work out where the RDP helper is: {error}"
                ));
                return;
            }
        };

        let user = config.user.clone().unwrap_or_default();
        let label = if user.is_empty() {
            config.host.clone()
        } else {
            format!("{user}@{}", config.host)
        };

        // The password is not read from the vault here. An RDP session with no stored credential is
        // one where Windows asks at its own login screen, which works; reaching into the vault
        // without being asked would unlock it for a session that may not need it.
        let request = bestterm_ipc_frame::ConnectRequest {
            host: config.host.clone(),
            port: config.port,
            username: user,
            domain: config.domain.clone().filter(|d| !d.is_empty()),
            password: bestterm_core_vault::Secret::new(String::new()),
            desktop_size: bestterm_surface::FrameSize::new(1280, 800),
            enable_credssp: true,
            keyboard_layout: 0,
            client_name: "BestTerm".to_string(),
            known_server_key: self.known_server_key(&config.host, config.port),
        };

        let waker = {
            let ctx = ctx.clone();
            std::sync::Arc::new(move || ctx.request_repaint()) as bestterm_helper_surface::Waker
        };

        match bestterm_helper_surface::connect(
            &helper,
            bestterm_surface::SurfaceKind::Rdp,
            label.clone(),
            request,
            waker,
        ) {
            Ok((surface, events)) => {
                let tab = crate::surface_tab::SurfaceTab::adopt(Box::new(surface), events, label);
                self.tabs.push(pane::Pane::Surface(Box::new(tab)));
                self.chrome.active_tab = self.tabs.len() - 1;
            }
            Err(error) => self.notices.push(format!("{label}: {error}")),
        }
    }

    /// Open a VNC session by launching the helper process.
    ///
    /// Almost the same as [`BestTermApp::connect_rdp`], and the differences are the protocol's: VNC
    /// has no server key to confirm, and nothing about it is encrypted — which is said here rather
    /// than left in a log, because a password typed into a VNC session is a password on the wire.
    fn connect_vnc(&mut self, config: bestterm_core_model::VncConfig, ctx: &egui::Context) {
        let label = format!("{}:{}", config.host, config.port);
        self.notices.push(format!(
            "{label}: VNC is not encrypted — the desktop and everything typed into it travel in              clear text"
        ));

        let helper = match bestterm_helper_surface::helper_path(VNC_HELPER) {
            Ok(path) if path.is_file() => path,
            Ok(path) => {
                self.notices.push(format!(
                    "the VNC helper is not installed beside this program (looked for {})",
                    path.display()
                ));
                return;
            }
            Err(error) => {
                self.notices.push(format!(
                    "could not work out where the VNC helper is: {error}"
                ));
                return;
            }
        };

        // The password is not read from the vault yet: the session dialog does not collect a VNC
        // credential, and reaching into the vault for a session that may not need one would unlock it
        // for nothing. A server that wants a password refuses, and says so.
        let request = bestterm_ipc_frame::ConnectRequest {
            host: config.host.clone(),
            port: config.port,
            username: String::new(),
            domain: None,
            password: bestterm_core_vault::Secret::new(String::new()),
            desktop_size: bestterm_surface::FrameSize::new(1280, 800),
            enable_credssp: false,
            keyboard_layout: 0,
            client_name: "BestTerm".to_string(),
            known_server_key: None,
        };

        let waker = {
            let ctx = ctx.clone();
            std::sync::Arc::new(move || ctx.request_repaint()) as bestterm_helper_surface::Waker
        };

        match bestterm_helper_surface::connect(
            &helper,
            bestterm_surface::SurfaceKind::Vnc,
            label.clone(),
            request,
            waker,
        ) {
            Ok((surface, events)) => {
                let tab = crate::surface_tab::SurfaceTab::adopt(Box::new(surface), events, label);
                self.tabs.push(pane::Pane::Surface(Box::new(tab)));
                self.chrome.active_tab = self.tabs.len() - 1;
            }
            Err(error) => self.notices.push(format!("{label}: {error}")),
        }
    }

    /// The key recorded for an RDP server, if one is.
    ///
    /// Read on every connection rather than cached: the file is small, it is the person's to edit,
    /// and a cache would mean a key removed by hand went on being trusted.
    fn known_server_key(&self, host: &str, port: u16) -> Option<String> {
        let store = self.store.as_ref()?;
        let text =
            std::fs::read_to_string(store.paths().config_dir().join(RDP_KNOWN_SERVERS)).ok()?;
        let wanted = format!("{}:{port}", host.to_ascii_lowercase());
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            let mut fields = line.split_whitespace();
            match (fields.next(), fields.next(), fields.next()) {
                (Some(address), Some(algorithm), Some(digest))
                    if address == wanted && algorithm.eq_ignore_ascii_case("sha256") =>
                {
                    return Some(digest.to_string());
                }
                _ => {}
            }
        }
        None
    }

    /// Write down a key an RDP session settled on.
    fn record_server_key(&mut self, host: &str, port: u16, digest: &str) {
        let Some(store) = self.store.as_ref() else {
            self.notices.push(
                "the server's key was accepted but there is nowhere to record it".to_string(),
            );
            return;
        };
        let path = store.paths().config_dir().join(RDP_KNOWN_SERVERS);
        let line = format!("{}:{port} sha256 {digest}\n", host.to_ascii_lowercase());
        // Appended, like `known_hosts`: the file is a log of decisions, and rewriting it would mean
        // this program deciding which of somebody's earlier decisions still count.
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
        if let Err(error) = written {
            self.notices
                .push(format!("could not record the server's key: {error}"));
        }
    }

    /// Ask about a server key a desktop is waiting on, and record what a desktop settled.
    ///
    /// Drained here rather than inside the tab because both ends of it belong to the application: the
    /// window is the application's, and so is the file the answer is written to.
    fn server_key_prompt(&mut self, ctx: &egui::Context) {
        let mut settled = Vec::new();
        let mut question = None;

        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let Some(surface) = tab.surface_mut() else {
                continue;
            };
            if let Some((digest, store)) = surface.settled_key.take()
                && store
            {
                settled.push((index, digest));
            }
            // One at a time, and the active tab's first. Two questions about two different servers,
            // stacked, is how somebody accepts the wrong one.
            if question.is_none()
                && let Some(asked) = surface.question.clone()
            {
                question = Some((index, asked));
            }
        }

        for (index, digest) in settled {
            if let Some(asked) = self.tabs.get(index).map(pane::Pane::title) {
                let (host, port) = split_host_port(&asked);
                self.record_server_key(&host, port, &digest);
            }
        }

        let Some((index, asked)) = question else {
            return;
        };

        let mut answer = None;
        egui::Modal::new(egui::Id::new("rdp-server-key")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(match asked.expected {
                Some(_) => "This server's key has changed",
                None => "This server has not been seen before",
            });
            ui.add_space(8.0);
            ui.label(format!("{}:{}", asked.host, asked.port));
            ui.add_space(6.0);
            ui.label(format!("Presented: {}", asked.fingerprint));
            if let Some(expected) = &asked.expected {
                ui.label(format!("Expected:  {expected}"));
                ui.add_space(6.0);
                // Stated rather than implied. A changed key is either a rebuild somebody did or a
                // machine answering that should not be, and only the person knows which.
                ui.colored_label(
                    egui::Color32::from_rgb(0xB0, 0x20, 0x20),
                    "Something is answering for this address that was not answering for it before. \
                     If nobody rebuilt this server, do not continue.",
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Connect").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });

        if let Some(accept) = answer
            && let Some(surface) = self.tabs.get_mut(index).and_then(pane::Pane::surface_mut)
        {
            surface.answer_server_key(accept);
        }
    }

    /// Take whatever the runtime has reported since the last frame.
    fn drain_sessions(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.sessions.1.try_recv() {
            match event {
                SessionEvent::Opened {
                    title,
                    open,
                    session,
                    record,
                    reconnect,
                    target,
                } => {
                    if let Some(record) = record {
                        append_known_host(&record);
                    }
                    let waker = {
                        let ctx = ctx.clone();
                        std::sync::Arc::new(move || ctx.request_repaint()) as crate::tab::Waker
                    };
                    let (cols, rows) = (80, 24);

                    // Recorded before the tab takes it, so the tunnel window can offer this session
                    // without reaching into a tab for it. Only for the protocols that have a session
                    // to offer: a telnet connection carries no channels to forward over.
                    let id = session.as_ref().map(|connection| {
                        let id = tunnels::ConnectionId(self.next_connection);
                        self.next_connection += 1;
                        self.connections.push(tunnels::LiveConnection {
                            id,
                            label: title.clone(),
                            connection: std::sync::Arc::clone(connection),
                        });
                        id
                    });

                    let mut tab = TerminalTab::adopt(crate::tab::NewTab {
                        open: *open,
                        title,
                        cols,
                        rows,
                        scrollback: SCROLLBACK,
                        palette: self.palette.clone(),
                        waker,
                        // Without this the connection would be dropped here and the session would die
                        // the moment it started working. A protocol with no session object under its
                        // channel -- telnet -- has nothing to hold.
                        owner: session.map(|connection| {
                            Box::new(connection) as Box<dyn std::any::Any + Send + Sync>
                        }),
                    });
                    tab.connection = id;
                    tab.reopen = match (reconnect, target) {
                        (Ok(ready), Some(target)) => Ok(crate::tab::Reopen { ready, target }),
                        // A credential that could be replayed with nowhere to replay it to is not a
                        // reconnectable session; the two travel together or not at all.
                        (Ok(_), None) => Err(bestterm_proto_ssh::NotReconnectable::Interactive),
                        (Err(why), _) => Err(why),
                    };
                    self.tabs.push(pane::Pane::Terminal(Box::new(tab)));
                    self.chrome.active_tab = self.tabs.len() - 1;
                }
                SessionEvent::Failed { title, reason } => {
                    tracing::warn!(%title, %reason, "connection failed");
                    self.notices.push(format!("{title}: {reason}"));
                }
                SessionEvent::AskAboutHostKey(question) => {
                    // One at a time. Two stacked prompts about two servers is how somebody accepts
                    // the wrong one; the rest wait their turn on the channel.
                    if self.pending_host_key.is_none() {
                        self.pending_host_key = Some(question);
                    } else {
                        question.answer(bestterm_proto_ssh::host_key::HostKeyDecision::Reject);
                    }
                }
            }
        }
    }

    /// Ask about a server's host key, if one is waiting.
    ///
    /// The three answers are the three the protocol layer defines, and each says what it does rather
    /// than yes or no: somebody who has been shown a fingerprint deserves to know whether they are
    /// about to write it down.
    fn host_key_prompt(&mut self, ctx: &egui::Context) {
        use bestterm_proto_ssh::host_key::HostKeyDecision;

        let Some(question) = self.pending_host_key.clone() else {
            return;
        };
        let mut answered = None;

        egui::Modal::new(egui::Id::new("bestterm_host_key")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            match &question.verdict {
                HostKeyVerdict::Unknown => {
                    ui.heading("A server you have not connected to before");
                    ui.label(format!(
                        "{}:{} presented this key:",
                        question.host, question.port
                    ));
                    ui.add_space(4.0);
                    ui.monospace(&question.presented);
                    ui.add_space(4.0);
                    ui.label("Accept it only if it matches what the server's administrator published.");
                }
                HostKeyVerdict::Changed { expected } => {
                    // Deliberately not phrased as a question about a "new" key. A changed key is what
                    // a machine-in-the-middle looks like, and it is also what a rebuilt server looks
                    // like, and only the person can tell those apart.
                    ui.heading("This server's key has CHANGED");
                    ui.label(format!("{}:{} now presents:", question.host, question.port));
                    ui.add_space(4.0);
                    ui.monospace(&question.presented);
                    ui.add_space(4.0);
                    ui.label(if expected.len() == 1 {
                        "but the key recorded for it is:"
                    } else {
                        "but the keys recorded for it are:"
                    });
                    for fingerprint in expected {
                        ui.monospace(fingerprint);
                    }
                    ui.add_space(4.0);
                    ui.label(
                        "Either the server was rebuilt, or something is impersonating it. Do not accept this unless you know which.",
                    );
                }
                HostKeyVerdict::Revoked => {
                    ui.heading("This key was revoked");
                    ui.label(format!(
                        "{}:{} presented a key recorded as revoked. It will not be accepted.",
                        question.host, question.port
                    ));
                }
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let revoked = question.verdict == HostKeyVerdict::Revoked;
                ui.add_enabled_ui(!revoked, |ui| {
                    if ui.button("Accept and remember").clicked() {
                        answered = Some(HostKeyDecision::AcceptAndStore);
                    }
                    if ui.button("Accept just this once").clicked() {
                        answered = Some(HostKeyDecision::Accept);
                    }
                });
                if ui.button("Do not connect").clicked() {
                    answered = Some(HostKeyDecision::Reject);
                }
            });
        });

        if let Some(decision) = answered {
            question.answer(decision);
            self.pending_host_key = None;
        }
    }

    /// Ask for the master password, if something is waiting on it.
    fn vault_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.vault.prompt else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;

        egui::Modal::new(egui::Id::new("bestterm_vault")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            match prompt {
                Prompt::Unlock => {
                    ui.heading("Unlock the credential vault");
                    ui.label("Your stored passwords are encrypted with this.");
                }
                Prompt::Create => {
                    ui.heading("Choose a master password");
                    ui.label(
                        "It encrypts every password this application stores. There is no way to \
                         recover it, and nothing here will remember it for you.",
                    );
                }
            }
            ui.add_space(8.0);

            let field = ui.add(
                egui::TextEdit::singleline(&mut self.vault.typed)
                    .password(true)
                    .hint_text("Master password")
                    .desired_width(f32::INFINITY),
            );
            field.request_focus();

            if prompt == Prompt::Create {
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.vault.repeated)
                        .password(true)
                        .hint_text("Repeat it")
                        .desired_width(f32::INFINITY),
                );
            }

            if let Some(error) = &self.vault.error {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(0xB0, 0x20, 0x20), error);
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let label = if prompt == Prompt::Create {
                    "Create"
                } else {
                    "Unlock"
                };
                if ui.button(label).clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });

            // Enter submits, because a password field that needs a mouse is a password field people
            // grow to resent.
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
        });

        if cancel {
            self.vault.cancel();
            self.pending_session = None;
            return;
        }
        if submit {
            let resumed = self.vault.submit(self.store.as_ref());
            if let (Some(PendingUnlock::Session), Some(config)) =
                (resumed, self.pending_session.take())
            {
                self.connect_ssh(config, ctx);
            }
        }
    }

    /// Show whatever went wrong, until it is dismissed.
    fn notice_window(&mut self, ctx: &egui::Context) {
        if self.notices.is_empty() {
            return;
        }
        let mut dismiss = false;
        egui::Window::new("Messages")
            .collapsible(false)
            .resizable(false)
            // Anchored low and centred. egui's default placement put it over the ribbon, covering the
            // controls somebody reads an error and then reaches for.
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -32.0))
            .show(ctx, |ui| {
                for notice in &self.notices {
                    ui.label(notice);
                }
                ui.add_space(6.0);
                if ui.button("Dismiss").clicked() {
                    dismiss = true;
                }
            });
        if dismiss {
            self.notices.clear();
        }
    }

    /// The port forwarding window.
    ///
    /// One window with the running tunnels above and a form below, rather than a wizard: a tunnel is
    /// four fields, and the thing people actually come here to do is check whether the one they
    /// opened this morning is still up.
    fn tunnel_window(&mut self, ctx: &egui::Context) {
        if !self.tunnels.open {
            return;
        }

        let mut open = true;
        let mut start: Option<tunnels::TunnelRequest> = None;
        let mut stop: Option<usize> = None;

        egui::Window::new("Port forwarding")
            .open(&mut open)
            .resizable(true)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Running").strong());
                if self.tunnels.running.is_empty() {
                    ui.weak("Nothing is forwarded.");
                } else {
                    for (index, tunnel) in self.tunnels.running.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.button("Stop").clicked() {
                                stop = Some(index);
                            }
                            ui.label(tunnel.describe());
                            ui.weak(format!("over {}", tunnel.over_label));
                        });
                    }
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                if self.connections.is_empty() {
                    // Said plainly rather than shown as an empty list with a dead button: there is a
                    // real prerequisite here, and "open an SSH session first" is the whole answer.
                    ui.label("Open an SSH session first — a tunnel runs over one.");
                    return;
                }

                ui.label(egui::RichText::new("New tunnel").strong());
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    for kind in tunnels::TunnelKind::ALL {
                        if ui
                            .selectable_label(self.tunnels.form.kind == kind, kind.label())
                            .clicked()
                        {
                            self.tunnels.form.kind = kind;
                            self.tunnels.error = None;
                        }
                    }
                });

                ui.add_space(4.0);
                ui.weak(self.tunnels.form.kind.summary());
                ui.add_space(8.0);

                let selected = self
                    .connections
                    .iter()
                    .find(|live| Some(live.id) == self.tunnels.form.over);
                egui::ComboBox::from_label("Over")
                    .selected_text(match selected {
                        Some(live) => live.label.clone(),
                        None => "Choose a session".to_string(),
                    })
                    .show_ui(ui, |ui| {
                        for live in &self.connections {
                            ui.selectable_value(
                                &mut self.tunnels.form.over,
                                Some(live.id),
                                &live.label,
                            );
                        }
                    });

                ui.add_space(6.0);
                egui::Grid::new("tunnel-form")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Listen on");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.tunnels.form.listen_host)
                                    .hint_text(match self.tunnels.form.kind {
                                        tunnels::TunnelKind::Remote => "server decides",
                                        _ => "127.0.0.1",
                                    })
                                    .desired_width(160.0),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut self.tunnels.form.listen_port)
                                    .hint_text("port")
                                    .desired_width(70.0),
                            );
                        });
                        ui.end_row();

                        if self.tunnels.form.kind.has_target() {
                            ui.label("Connect to");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.tunnels.form.target_host)
                                        .hint_text("host")
                                        .desired_width(160.0),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.tunnels.form.target_port)
                                        .hint_text("port")
                                        .desired_width(70.0),
                                );
                            });
                            ui.end_row();
                        }
                    });

                if let Some(error) = self.tunnels.error {
                    ui.add_space(4.0);
                    ui.colored_label(egui::Color32::from_rgb(0xB0, 0x20, 0x20), error.message());
                }
                if let Some(notice) = &self.tunnels.notice {
                    ui.add_space(4.0);
                    ui.colored_label(egui::Color32::from_rgb(0xB0, 0x20, 0x20), notice);
                }

                ui.add_space(8.0);
                if ui.button("Open").clicked() {
                    match self.tunnels.form.check() {
                        Ok(request) => {
                            self.tunnels.error = None;
                            start = Some(request);
                        }
                        Err(error) => self.tunnels.error = Some(error),
                    }
                }
            });

        // Acted on after the window closes, because both borrow the state the window is drawing.
        if let Some(index) = stop {
            self.tunnels.stop(&self.runtime, index);
        }
        if let Some(request) = start {
            // Looked up again rather than captured: the window ran a frame ago in wall-clock terms,
            // and a session can close between a click and its handling.
            match self
                .connections
                .iter()
                .find(|live| live.id == request.over)
                .map(|live| tunnels::LiveConnection {
                    id: live.id,
                    label: live.label.clone(),
                    connection: std::sync::Arc::clone(&live.connection),
                }) {
                Some(over) => self.tunnels.start(&self.runtime, request, &over),
                None => self.tunnels.error = Some(tunnels::FormError::NoConnection),
            }
        }
        if !open {
            self.tunnels.open = false;
        }
    }

    /// Open a fresh session in place of one that died.
    ///
    /// The replacement is a whole new connection: `russh` has no resumption, so the working
    /// directory, the shell's history, whatever was running and the scrollback are all gone. The tab
    /// is left in place and a new one opens beside it rather than the old one being quietly
    /// refilled, because a terminal that comes back empty in the same tab looks like it lost its
    /// contents to a bug.
    ///
    /// The host key is pinned to the one the dead connection saw. Not re-checked against
    /// `known_hosts`: that asks whether the *address* is trusted, and the address is exactly what can
    /// have moved while the connection was down. See `bestterm_proto_ssh::reconnect`.
    fn reconnect_tab(&mut self, index: usize, ctx: &egui::Context) {
        let Some(pane::Pane::Terminal(tab)) = self.tabs.get(index) else {
            return;
        };
        let reopen = match &tab.reopen {
            Ok(reopen) => reopen,
            Err(why) => {
                self.notices.push(why.to_string());
                return;
            }
        };

        let config = (*reopen.target).clone();
        let auth = reopen.ready.auth.clone();
        let verifier = reopen.ready.verifier();
        let title = tab.title();

        let waker = {
            let ctx = ctx.clone();
            std::sync::Arc::new(move || ctx.request_repaint())
                as std::sync::Arc<dyn Fn() + Send + Sync>
        };

        tracing::info!(%title, "reconnecting");
        ssh::reconnect(
            self.runtime.handle(),
            config,
            auth,
            verifier,
            bestterm_transport::GridSize::new(80, 24),
            self.sessions.0.clone(),
            waker,
        );
    }

    /// The Configuration dialog, and what its rows open.
    fn configuration_dialog(&mut self, ctx: &egui::Context) {
        use bestterm_ui_chrome::configuration::{ConfigAction, ConfigField, ConfigLink};

        for action in self.configuration.show(ctx, &self.theme) {
            match action {
                // The one row that already has somewhere to go. The vault is the passwords, so this
                // is not a placeholder standing in for one.
                ConfigAction::Open(ConfigLink::Passwords) => {
                    self.vault.ask(self.store.as_ref(), None);
                }
                ConfigAction::Open(link) => {
                    self.notices
                        .push(format!("\"{}\" has nothing behind it yet", link.label()));
                }
                // Picking a directory needs a file dialog, which is a dependency this build does not
                // have. Said plainly rather than doing nothing: a button that silently does nothing
                // reads as a bug in the button.
                ConfigAction::Browse(field) => {
                    self.notices.push(format!(
                        "there is no folder picker yet — type the path for {}",
                        field.label().trim_end_matches(':')
                    ));
                }
                ConfigAction::Reset(field) => match field {
                    ConfigField::Home => self.configuration.home.clear(),
                    ConfigField::Root => self.configuration.root.clear(),
                    ConfigField::Editor => self.configuration.editor.clear(),
                },
                // Nothing in it is persisted yet, which is phase 1's configuration work rather than
                // this dialog's. Accepting closes it and keeps what was typed for this run.
                ConfigAction::Accepted => tracing::debug!("configuration accepted"),
                ConfigAction::Cancelled => tracing::debug!("configuration dismissed"),
            }
        }
    }

    /// Read a `.mxtsessions` file into the tree and save it.
    ///
    /// Every count is reported, including the ones that are zero. An import that silently dropped a
    /// third of somebody's inventory would be discovered weeks later, by which time they would have
    /// stopped believing the tool.
    fn import_mxtsessions(&mut self, path: &std::path::Path) {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.notices
                    .push(format!("could not read {}: {error}", path.display()));
                return;
            }
        };

        let import = bestterm_importers::mxtsessions::parse(&bytes);
        let ids = import.tree.walk();
        let sessions = ids
            .iter()
            .filter(|id| {
                import
                    .tree
                    .get(**id)
                    .is_some_and(|node| !node.kind.is_folder())
            })
            .count();

        // Secrets arrive in clear text, which is how the file stores them, and go straight into the
        // vault. They are never written anywhere else and never held past this function: an import
        // that left a copy in a log or a temporary file would undo the vault entirely.
        if !import.secrets.is_empty() {
            if self.vault.is_open() {
                let mut stored = 0;
                for secret in &import.secrets {
                    if self
                        .vault
                        .set(self.store.as_ref(), secret.reference.key(), &secret.secret)
                    {
                        stored += 1;
                    }
                }
                self.notices
                    .push(format!("{stored} password(s) moved into the vault"));
            } else {
                // Refused rather than kept in memory until an unlock: the person can unlock and import
                // again, and nothing is lost because the file is still theirs.
                self.notices.push(format!(
                    "{} password(s) were not imported: unlock the vault first, then import again",
                    import.secrets.len()
                ));
            }
        }
        if !import.skipped.is_empty() {
            self.notices
                .push(format!("{} entries were skipped", import.skipped.len()));
        }

        self.tree = import.tree;
        self.notices.push(format!(
            "imported {sessions} session(s) from {}",
            path.display()
        ));
        self.save_tree();
    }

    /// Write the session tree, reporting a failure rather than losing it quietly.
    fn save_tree(&mut self) {
        let Some(store) = &self.store else {
            self.notices
                .push("no configuration directory: sessions will not be remembered".to_string());
            return;
        };
        if let Err(error) = store.save_tree(&self.tree) {
            self.notices
                .push(format!("could not save the sessions: {error}"));
        }
    }

    /// Draw the saved session tree, returning a session somebody asked to open.
    fn session_tree(&mut self, ui: &mut egui::Ui) -> Option<NodeId> {
        let roots: Vec<NodeId> = self.tree.roots().to_vec();
        if roots.is_empty() {
            ui.label(
                egui::RichText::new(
                    "No saved sessions. Import a .mxtsessions file to bring some in.",
                )
                .small()
                .color(self.theme.text_dim),
            );
            return None;
        }
        let mut requested = None;
        for id in roots {
            self.session_node(ui, id, &mut requested);
        }
        requested
    }

    /// Draw one node and, for a folder, its children.
    fn session_node(&mut self, ui: &mut egui::Ui, id: NodeId, requested: &mut Option<NodeId>) {
        let Some(node) = self.tree.get(id) else {
            return;
        };
        let name = node.name.clone();

        if node.kind.is_folder() {
            let children: Vec<NodeId> = node.children().to_vec();
            // egui's own collapsing header, because it paints its triangle with the painter rather
            // than with a glyph. The first version used `▸` and `▾`, which the bundled font does not
            // have, so every folder in the tree was marked with an empty box.
            egui::CollapsingHeader::new(name)
                .id_salt(id)
                .default_open(false)
                .show(ui, |ui| {
                    for child in children {
                        self.session_node(ui, child, requested);
                    }
                });
        } else if ui.selectable_label(false, name).double_clicked() {
            *requested = Some(id);
        }
    }

    /// Open the session a tree node describes.
    fn open_saved_session(&mut self, id: NodeId, ctx: &egui::Context) {
        let Some(node) = self.tree.get(id) else {
            return;
        };
        match &node.kind {
            NodeKind::Session { config } => match config.as_ref() {
                ProtocolConfig::Ssh(ssh) => {
                    let ssh = ssh.clone();
                    self.connect_ssh(ssh, ctx);
                }
                other => self.notices.push(format!(
                    "{} sessions cannot be opened yet",
                    other.protocol().id()
                )),
            },
            NodeKind::Folder { .. } => {}
        }
    }

    /// Put the interface into the state [`UI_STATE_VARIABLE`] asked for, if it asked for one.
    ///
    /// Runs once, on the first frame, after the initial shell has opened so that a capture shows the
    /// requested state over a real session rather than an empty window.
    fn apply_startup(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.startup.import.take() {
            self.import_mxtsessions(&path);
        }
        let Some(target) = self.startup.connect.take() else {
            return;
        };
        match parse_quick_connect(&target) {
            Some(config) => self.connect_ssh(config, ctx),
            None => self
                .notices
                .push(format!("could not read '{target}' as user@host:port")),
        }
    }

    fn apply_requested_state(&mut self) {
        let Ok(state) = std::env::var(UI_STATE_VARIABLE) else {
            return;
        };
        match state.as_str() {
            "session-dialog" => self.dialog.open_fresh(),
            "tools" => self.chrome.sidebar_panel = SidebarPanel::Tools,
            "macros" => self.chrome.sidebar_panel = SidebarPanel::Macros,
            other => tracing::warn!(state = other, "unknown {UI_STATE_VARIABLE} value; ignored"),
        }
    }

    /// Act on what the Session settings dialog produced.
    ///
    /// Nothing connects yet: the session model reaches the application here for the first time, and
    /// turning a `ProtocolConfig` into a live connection is the next piece of work. Each outcome is
    /// reported rather than dropped, because a dialog that closes and does nothing is
    /// indistinguishable from one that is broken.
    fn apply_dialog_outcome(&mut self, outcome: DialogOutcome, ctx: &egui::Context) {
        match outcome {
            DialogOutcome::Accepted(config) => match *config {
                bestterm_core_model::ProtocolConfig::Ssh(ssh) => self.connect_ssh(ssh, ctx),
                bestterm_core_model::ProtocolConfig::Rdp(rdp) => self.connect_rdp(rdp, ctx),
                bestterm_core_model::ProtocolConfig::Vnc(vnc) => self.connect_vnc(vnc, ctx),
                bestterm_core_model::ProtocolConfig::Telnet(telnet) => {
                    self.connect_telnet(telnet, ctx)
                }
                bestterm_core_model::ProtocolConfig::Serial(serial) => {
                    self.open_serial(&serial, ctx)
                }
                other => {
                    self.notices.push(format!(
                        "{} sessions cannot be opened yet",
                        other.protocol().id()
                    ));
                }
            },
            DialogOutcome::Cancelled => tracing::debug!("session dialog cancelled"),
            DialogOutcome::Unsupported(name) => {
                tracing::warn!(protocol = name, "no session model for this protocol yet");
            }
            DialogOutcome::Incomplete { field } => {
                tracing::warn!(field, "a required field was empty");
            }
        }
    }

    /// Refresh the view model the chrome draws from.
    fn sync_chrome(&mut self) {
        self.chrome.tabs = self
            .tabs
            .iter()
            .map(|tab| TabInfo {
                title: tab.title(),
                program_title: tab.program_title(),
                protocol: tab.protocol(),
                tint: None,
            })
            .collect();

        let grid = self
            .tabs
            .get(self.chrome.active_tab)
            .map(|tab| tab.grid())
            .unwrap_or((0, 0));

        let can_reconnect = matches!(
            self.tabs.get(self.chrome.active_tab),
            Some(pane::Pane::Terminal(tab)) if tab.can_reconnect()
        );

        self.chrome.status = StatusInfo {
            // No X server until phase 6; reporting "stopped" is accurate, not a placeholder.
            x_display: None,
            grid,
            session: self
                .tabs
                .get(self.chrome.active_tab)
                .map(|tab| tab.status_line())
                .unwrap_or_default(),
            can_reconnect,
        };
    }

    /// The terminal area: sizing, input and painting.
    fn terminal_ui(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let (cols, rows) = self.metrics.grid_for(available);
        let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());

        let id = response.id;
        if response.clicked() {
            ui.memory_mut(|memory| memory.request_focus(id));
        }
        let focused = ui.memory(|memory| memory.has_focus(id));

        if focused {
            // Without this, egui steals Tab for widget navigation, Escape for closing things, and
            // the arrow keys for moving between widgets — all of which the terminal needs.
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
        }

        let cell = (
            self.metrics.cell_width.round() as u16,
            self.metrics.cell_height.round() as u16,
        );

        let Some(tab) = self.tabs.get_mut(self.chrome.active_tab) else {
            return;
        };

        // The one place that legitimately branches on what a pane holds: a grid of glyphs and a
        // texture are different work, and generalising over them would mean an abstraction whose only
        // two implementations share nothing.
        let tab = match tab {
            pane::Pane::Surface(surface) => {
                surface.show(ui);
                return;
            }
            pane::Pane::Terminal(terminal) => terminal,
        };

        tab.resize(cols, rows, cell);

        if focused {
            let events = ui.input(|input| input.events.clone());
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            handle_input(tab, &events, scroll, self.metrics.cell_height);
        }

        let snapshot = tab.emulator().snapshot();
        bestterm_term_render::paint(
            ui.painter(),
            rect,
            &snapshot,
            &self.metrics,
            &self.term_style,
            focused,
        );
    }
}

/// Translate a frame's worth of `egui` input into bytes for the pty.
fn handle_input(tab: &mut TerminalTab, events: &[egui::Event], scroll_y: f32, cell_height: f32) {
    let mut out: Vec<u8> = Vec::new();

    for event in events {
        match event {
            // Printable input arrives as text. Using it rather than reconstructing characters from
            // key codes is what makes non-Latin layouts and dead keys work.
            egui::Event::Text(text) => out.extend_from_slice(text.as_bytes()),
            egui::Event::Paste(text) => out.extend_from_slice(text.as_bytes()),

            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let Some(term_key) = keys::from_egui(*key) else {
                    continue;
                };
                let mods = keys::mods_from_egui(modifiers);

                // A plain printable key already came through as `Text`; encoding it here too would
                // double every keystroke. Ctrl and Alt combinations produce no `Text`, so those are
                // ours to encode.
                if matches!(term_key, TermKey::Char(_)) && !mods.ctrl && !mods.alt {
                    continue;
                }

                if let Some(bytes) = keys::encode(term_key, mods) {
                    out.extend_from_slice(&bytes);
                }
            }

            _ => {}
        }
    }

    // Any keystroke returns the view to the live output, which is what every terminal does.
    if !out.is_empty() {
        tab.emulator_mut().scroll_to_bottom();
        tab.write(&out);
    }

    if scroll_y.abs() >= 1.0 && cell_height > 0.0 {
        let lines = (scroll_y / cell_height).round() as i32;
        if lines != 0 {
            tab.emulator_mut().scroll(lines);
        }
    }
}

impl eframe::App for BestTermApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if !self.theme_installed {
            apply_theme(&ctx, &self.theme);
            self.theme_installed = true;
        }
        self.metrics = TerminalMetrics::measure(&ctx, &self.term_style);

        if !self.opened_first_shell {
            self.opened_first_shell = true;
            self.open_shell(0, &ctx);
            self.apply_requested_state();
            self.apply_startup(&ctx);
        }

        self.drain_sessions(&ctx);
        let output_arrived = self.pump(&ctx);
        self.sync_chrome();

        let mut actions: Vec<ChromeAction> = Vec::new();
        // Filled by the left panel, acted on after it has finished drawing.
        let mut requested_shell: Option<usize> = None;
        let mut requested_session: Option<NodeId> = None;

        // Cloned once per frame so the panel closures below borrow a local rather than `self`,
        // which would otherwise conflict with the two closures that need `&mut self`. The theme is
        // a handful of colours and floats; the clarity is worth more than the copy.
        let theme = self.theme.clone();
        // Cloned for the same reason: the row below hands `&mut self.chrome` to the quick-connect
        // field and a read-only view to the tab bar, in one closure.
        let chrome = self.chrome.clone();

        // Panel order is layout order: first added is outermost. The central panel must be last.
        Panel::top("bestterm_menu_bar")
            .frame(chrome_frame(theme.menu_bg))
            .show(ui, |ui| menu_bar(ui, &mut actions));

        Panel::top("bestterm_ribbon")
            .exact_size(theme.ribbon_height)
            .frame(chrome_frame(theme.chrome_bg))
            .show(ui, |ui| ribbon(ui, &theme, &mut actions));

        // One row, full width, above the sidebar: the quick-connect field on the left and the tab
        // bar immediately to its right. Measured from the reference, which does not give either of
        // them a row of its own — see `docs/ui-parity.md`.
        Panel::top("bestterm_connect_and_tabs")
            .exact_size(theme.quick_connect_height + 6.0)
            .frame(chrome_frame(theme.chrome_bg))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    quick_connect_field(ui, &mut self.chrome, &mut actions);
                    tab_bar(ui, &theme, &chrome, &mut actions);
                });
            });

        Panel::bottom("bestterm_status_bar")
            .exact_size(theme.status_bar_height)
            .frame(chrome_frame(theme.chrome_bg))
            .show(ui, |ui| {
                status_bar(ui, &theme, &chrome.status, &chrome, &mut actions)
            });

        // The dialog covers everything below the quick-connect row -- the sidebar included, which is
        // where the reference puts it. Drawn here rather than inside the central panel because at this
        // point the remaining rectangle is still the full width of the window; adding the left panels
        // first would confine it to the session area and leave its fifteen tabs wrapping onto two rows.
        if self.dialog.open {
            Frame::NONE
                .fill(theme.chrome_bg)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| session_dialog(ui, &theme, &mut self.dialog));

            if let Some(outcome) = self.dialog.take_outcome() {
                self.apply_dialog_outcome(outcome, &ctx);
            }
            self.apply_actions(actions, &ctx);
            return;
        }

        // The edge strip is always visible, even when the panel beside it is collapsed.
        Panel::left("bestterm_sidebar_strip")
            .exact_size(theme.sidebar_strip_width)
            .resizable(false)
            .frame(Frame::NONE.fill(theme.chrome_bg))
            .show(ui, |ui| {
                sidebar_strip(ui, &theme, &self.chrome, &mut actions)
            });

        if self.chrome.sidebar_open {
            Panel::left("bestterm_sidebar")
                .default_size(theme.sidebar_width)
                .min_size(theme.sidebar_min_width)
                .frame(chrome_frame(theme.chrome_bg))
                .show(ui, |ui| {
                    requested_shell =
                        self.sidebar_contents(ui, &mut actions, &mut requested_session)
                });
        }

        CentralPanel::no_frame().show(ui, |ui| self.terminal_ui(ui));

        // Modal, and over everything: a question about a server's identity is not something to
        // answer by accident while reaching for a tab.
        self.host_key_prompt(&ctx);
        self.vault_prompt(&ctx);
        self.notice_window(&ctx);
        self.server_key_prompt(&ctx);
        self.configuration_dialog(&ctx);
        self.tunnel_window(&ctx);

        if let Some(index) = requested_shell {
            self.open_shell(index, &ctx);
        }
        if let Some(id) = requested_session {
            self.open_saved_session(id, &ctx);
        }

        self.apply_actions(actions, &ctx);

        // A repaint here covers the case where the output budget was reached and bytes are still
        // queued. Waking on *arrival* is the relay's job — see `tab.rs` — because a frame is the one
        // thing that cannot schedule itself.
        if output_arrived {
            ctx.request_repaint();
        }
    }
}

impl BestTermApp {
    /// Placeholder contents for the left panel.
    ///
    /// The session tree lands in phase 2 and the SFTP browser in phase 4; the panel exists now so the
    /// layout it participates in is correct from the start.
    /// Draw the left panel, returning a shell the person asked to open.
    ///
    /// Returned rather than opened here: this runs inside a closure that already holds `&mut self`,
    /// and opening a tab needs the interface context, which the caller has.
    fn sidebar_contents(
        &mut self,
        ui: &mut egui::Ui,
        actions: &mut Vec<ChromeAction>,
        requested_session: &mut Option<NodeId>,
    ) -> Option<usize> {
        match self.chrome.sidebar_panel {
            SidebarPanel::Sessions => {
                ui.label(egui::RichText::new("User sessions").strong());
                ui.separator();
                if let Some(id) = self.session_tree(ui) {
                    *requested_session = Some(id);
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Local shells").strong());
                ui.separator();
                let shells: Vec<(usize, String)> = self
                    .shells
                    .iter()
                    .enumerate()
                    .map(|(index, shell)| (index, shell.label.clone()))
                    .collect();
                let mut requested = None;
                for (index, label) in shells {
                    if ui.selectable_label(false, label).double_clicked() {
                        requested = Some(index);
                    }
                }
                requested
            }
            SidebarPanel::Tools => {
                tools_panel(ui, &self.theme, actions);
                None
            }
            SidebarPanel::Macros => {
                macros_panel(ui, &self.theme, actions);
                None
            }
        }
    }
}

/// Read the user's `known_hosts`, or an empty store if there is none.
///
/// OpenSSH's file, deliberately: somebody who already trusts a host from the command line should not be
/// asked about it again here, and a second store would mean two answers to the same question.
fn read_known_hosts() -> String {
    let Some(home) = home_directory() else {
        return String::new();
    };
    std::fs::read_to_string(home.join(".ssh").join("known_hosts")).unwrap_or_default()
}

/// Append an accepted key to `known_hosts`.
///
/// Rendering the line is `proto-ssh`'s business; this only decides where it goes. Failures are logged
/// and not fatal: a session that works but could not be written down is better than one refused because
/// a file was read-only.
fn append_known_host(record: &HostKeyRecord) {
    use bestterm_proto_ssh::known_hosts::KnownHosts;
    use std::io::Write as _;

    let mut store = KnownHosts::new();
    let line = match store.add(&record.host, record.port, &record.key, false) {
        Ok(line) => line,
        Err(error) => {
            tracing::warn!(%error, "could not render a known_hosts entry");
            return;
        }
    };

    let Some(home) = home_directory() else {
        return;
    };
    let directory = home.join(".ssh");
    if let Err(error) = std::fs::create_dir_all(&directory) {
        tracing::warn!(%error, "could not create the .ssh directory");
        return;
    }
    let path = directory.join("known_hosts");
    let opened = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path);
    match opened {
        Ok(mut file) => {
            if let Err(error) = writeln!(file, "{line}") {
                tracing::warn!(%error, "could not append to known_hosts");
            }
        }
        Err(error) => tracing::warn!(%error, "could not open known_hosts for appending"),
    }
}

/// The user's home directory, from the environment.
fn home_directory() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

/// Read `user@host:port` as an SSH session.
///
/// The user and the port are both optional, which is what makes this worth a function: `srv.int`,
/// `admin@srv.int` and `admin@srv.int:2222` all have to work, and a bracketed IPv6 address must not have
/// its colons mistaken for a port separator.
///
/// # Why it is strict about what a host may contain
///
/// This is reached from the command line as well as from the quick-connect field, and a stray word in
/// an argument list must not become a network connection. That is not hypothetical: a path with a space
/// in it, passed unquoted, split into two arguments, and the leftover `Sessions.mxtsessions` was
/// promptly looked up as a host name. A host is letters, digits, dots, hyphens and — for IPv6 — colons,
/// so anything else is refused rather than resolved.
fn parse_quick_connect(text: &str) -> Option<bestterm_core_model::SshConfig> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    // Split at the last `@`: a password is never in this field, but a user name legitimately contains
    // one when it is an email-shaped login.
    let (user, rest) = match text.rsplit_once('@') {
        Some((user, rest)) if !user.is_empty() && !rest.is_empty() => (Some(user.to_owned()), rest),
        Some(_) => return None,
        None => (None, text),
    };

    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        let (address, tail) = inner.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(port) => Some(port.parse().ok()?),
            None if tail.is_empty() => None,
            None => return None,
        };
        (address.to_owned(), port)
    } else {
        match rest.rsplit_once(':') {
            // More than one colon and no brackets is a bare IPv6 address, which has no port.
            Some((head, _)) if head.contains(':') => (rest.to_owned(), None),
            Some((host, port)) => (host.to_owned(), Some(port.parse().ok()?)),
            None => (rest.to_owned(), None),
        }
    };

    if host.is_empty() || !is_plausible_host(&host) {
        return None;
    }
    Some(bestterm_core_model::SshConfig {
        host,
        port: port.unwrap_or(22),
        user,
        ..bestterm_core_model::SshConfig::default()
    })
}

/// Split `user@host` or `host` back into a host and a port.
///
/// The label is what the tab is called, which is what the session was named, so this recovers the
/// address to record a key against. A port is only ever in it when the session carried one.
fn split_host_port(label: &str) -> (String, u16) {
    let host = label.rsplit('@').next().unwrap_or(label);
    match host.rsplit_once(':') {
        Some((name, port)) => match port.parse() {
            Ok(port) => (name.to_string(), port),
            // A colon that is not a port is part of an IPv6 address.
            Err(_) => (host.to_string(), 3389),
        },
        None => (host.to_string(), 3389),
    }
}

/// Whether a string could be a host name or an address.
///
/// Deliberately about *shape* and not about existence: resolving it to find out would mean a DNS
/// lookup for every typo, and on many networks a lookup is itself a disclosure. What this rules out is
/// the shape of a file name or a fragment of a sentence — which is what turns up when an argument list
/// has been split somewhere it should not have been.
fn is_plausible_host(host: &str) -> bool {
    // A trailing dot is legal in a fully-qualified name and an embedded colon is legal in IPv6.
    !host.starts_with('.')
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'))
}

/// A square, hairline-bordered chrome panel.
fn chrome_frame(fill: egui::Color32) -> Frame {
    Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(4, 2))
        .corner_radius(CornerRadius::ZERO)
        .stroke(Stroke::NONE)
}

/// The window's initial inner size, in logical pixels.
pub const DEFAULT_WINDOW_SIZE: [f32; 2] = [1180.0, 760.0];

/// A [`GridSize`] for `cols` × `rows` with the given cell pixel size.
pub(crate) fn grid_size(cols: usize, rows: usize, cell: (u16, u16)) -> GridSize {
    GridSize::with_pixels(
        cols.min(u16::MAX as usize) as u16,
        rows.min(u16::MAX as usize) as u16,
        cell.0.saturating_mul(cols.min(u16::MAX as usize) as u16),
        cell.1.saturating_mul(rows.min(u16::MAX as usize) as u16),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_size_clamps_and_multiplies() {
        let g = grid_size(80, 24, (8, 16));
        assert_eq!((g.cols, g.rows), (80, 24));
        assert_eq!((g.pixel_width, g.pixel_height), (640, 384));
    }

    #[test]
    fn grid_size_saturates_instead_of_overflowing() {
        // A very large grid must not panic in release or wrap in debug.
        let g = grid_size(60_000, 60_000, (8, 16));
        assert_eq!(g.pixel_width, u16::MAX);
        assert_eq!(g.pixel_height, u16::MAX);
    }

    #[test]
    fn grid_size_never_reports_zero_dimensions() {
        let g = grid_size(0, 0, (0, 0));
        assert_eq!((g.cols, g.rows), (1, 1));
    }

    #[test]
    fn quick_connect_reads_a_bare_host() {
        let config = parse_quick_connect("srv.int").expect("a host alone is enough");
        assert_eq!(config.host, "srv.int");
        assert_eq!(config.port, 22);
        assert_eq!(config.user, None);
    }

    #[test]
    fn quick_connect_reads_a_user_and_a_port() {
        let config = parse_quick_connect(" admin@srv.int:2222 ").expect("parses");
        assert_eq!(config.user.as_deref(), Some("admin"));
        assert_eq!(config.host, "srv.int");
        assert_eq!(config.port, 2222);
    }

    #[test]
    fn quick_connect_splits_at_the_last_at_sign() {
        // A login shaped like an email address is a real thing, and it contains an `@`.
        let config = parse_quick_connect("first.last@corp.example@bastion.int").expect("parses");
        assert_eq!(config.user.as_deref(), Some("first.last@corp.example"));
        assert_eq!(config.host, "bastion.int");
    }

    #[test]
    fn quick_connect_does_not_mistake_an_ipv6_address_for_a_port() {
        // The colons in `2001:db8::1` are part of the address. Reading the last one as a port
        // separator would send somebody to a host that does not exist.
        let bare = parse_quick_connect("2001:db8::1").expect("a bare ipv6 address");
        assert_eq!(bare.host, "2001:db8::1");
        assert_eq!(bare.port, 22);

        let bracketed = parse_quick_connect("[2001:db8::1]:2222").expect("bracketed with a port");
        assert_eq!(bracketed.host, "2001:db8::1");
        assert_eq!(bracketed.port, 2222);

        let no_port = parse_quick_connect("[2001:db8::1]").expect("bracketed without a port");
        assert_eq!(no_port.host, "2001:db8::1");
        assert_eq!(no_port.port, 22);
    }

    #[test]
    fn a_stray_word_never_becomes_a_connection() {
        // The case that actually happened: an unquoted path with a space split into two arguments, and
        // the leftover was looked up as a host. A host does not contain a backslash or a space.
        assert!(
            parse_quick_connect("Sessions.mxtsessions").is_some(),
            "a bare name is a plausible host"
        );
        assert!(
            parse_quick_connect("D:\\DBA\\MobaXterm").is_none(),
            "a path is not a host"
        );
        assert!(parse_quick_connect("some words here").is_none());
        assert!(parse_quick_connect("--import").is_none());
        assert!(parse_quick_connect("/etc/hosts").is_none());
        assert!(
            parse_quick_connect(".hidden").is_none(),
            "a leading dot is not a host"
        );
        assert!(
            parse_quick_connect("-flag").is_none(),
            "a leading hyphen is not a host"
        );
    }

    #[test]
    fn quick_connect_refuses_what_it_cannot_read() {
        // Each of these would otherwise become a connection to somewhere unintended.
        assert!(parse_quick_connect("").is_none());
        assert!(parse_quick_connect("   ").is_none());
        assert!(parse_quick_connect("@srv.int").is_none(), "no user");
        assert!(parse_quick_connect("admin@").is_none(), "no host");
        assert!(parse_quick_connect("srv.int:").is_none(), "empty port");
        assert!(
            parse_quick_connect("srv.int:no").is_none(),
            "port is not a number"
        );
        assert!(
            parse_quick_connect("srv.int:99999").is_none(),
            "port does not fit"
        );
    }
}
