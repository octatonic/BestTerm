//! The application shell: window layout, tabs, and the wiring between input, transport and emulator.
//!
//! This is the only crate that knows about all the others. Everything below it is independently
//! testable, which is the point — see `docs/ARCHITECTURE.md`.

mod ssh;
mod tab;

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

/// The application.
pub struct BestTermApp {
    theme: ChromeTheme,
    term_style: TerminalStyle,
    metrics: TerminalMetrics,
    chrome: ChromeState,
    tabs: Vec<TerminalTab>,
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
                self.tabs.push(tab);
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
        tab.shutdown();
        if self.chrome.active_tab >= self.tabs.len() {
            self.chrome.active_tab = self.tabs.len().saturating_sub(1);
        }
    }

    /// Move output from every transport into its emulator, and answers back the other way.
    ///
    /// Returns true if anything changed and the UI should repaint.
    fn pump(&mut self) -> bool {
        let mut changed = false;
        for tab in &mut self.tabs {
            changed |= tab.pump();
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
            read_known_hosts(),
            size,
            self.sessions.0.clone(),
            waker,
        );
    }

    /// Take whatever the runtime has reported since the last frame.
    fn drain_sessions(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.sessions.1.try_recv() {
            match event {
                SessionEvent::Opened {
                    title,
                    open,
                    record,
                } => {
                    if let Some(record) = record {
                        append_known_host(&record);
                    }
                    let waker = {
                        let ctx = ctx.clone();
                        std::sync::Arc::new(move || ctx.request_repaint()) as crate::tab::Waker
                    };
                    let (cols, rows) = (80, 24);
                    let tab = TerminalTab::adopt(
                        *open,
                        title,
                        cols,
                        rows,
                        SCROLLBACK,
                        self.palette.clone(),
                        waker,
                    );
                    self.tabs.push(tab);
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

    /// Put the interface into the state [`UI_STATE_VARIABLE`] asked for, if it asked for one.
    ///
    /// Runs once, on the first frame, after the initial shell has opened so that a capture shows the
    /// requested state over a real session rather than an empty window.
    fn apply_startup(&mut self, ctx: &egui::Context) {
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
                protocol: tab.protocol().to_string(),
                tint: None,
            })
            .collect();

        let grid = self
            .tabs
            .get(self.chrome.active_tab)
            .map(|tab| tab.grid())
            .unwrap_or((0, 0));

        self.chrome.status = StatusInfo {
            // No X server until phase 6; reporting "stopped" is accurate, not a placeholder.
            x_display: None,
            grid,
            session: self
                .tabs
                .get(self.chrome.active_tab)
                .map(|tab| tab.status_line())
                .unwrap_or_default(),
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
        let output_arrived = self.pump();
        self.sync_chrome();

        let mut actions: Vec<ChromeAction> = Vec::new();
        // Filled by the left panel, acted on after it has finished drawing.
        let mut requested_shell: Option<usize> = None;

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
            .show(ui, |ui| status_bar(ui, &theme, &self.chrome.status));

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
                    requested_shell = self.sidebar_contents(ui, &mut actions)
                });
        }

        CentralPanel::no_frame().show(ui, |ui| self.terminal_ui(ui));

        // Modal, and over everything: a question about a server's identity is not something to
        // answer by accident while reaching for a tab.
        self.host_key_prompt(&ctx);
        self.notice_window(&ctx);

        if let Some(index) = requested_shell {
            self.open_shell(index, &ctx);
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
    ) -> Option<usize> {
        match self.chrome.sidebar_panel {
            SidebarPanel::Sessions => {
                ui.label(egui::RichText::new("User sessions").strong());
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
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Saved sessions arrive in phase 2.")
                        .small()
                        .color(self.theme.text_dim),
                );
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

    if host.is_empty() {
        return None;
    }
    Some(bestterm_core_model::SshConfig {
        host,
        port: port.unwrap_or(22),
        user,
        ..bestterm_core_model::SshConfig::default()
    })
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
