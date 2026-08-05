//! The application shell: window layout, tabs, and the wiring between input, transport and emulator.
//!
//! This is the only crate that knows about all the others. Everything below it is independently
//! testable, which is the point — see `docs/ARCHITECTURE.md`.

mod tab;

use bestterm_core_pty::{ShellProfile, discover};
use bestterm_core_terminal::{Palette, TerminalEmulator};
use bestterm_term_render::keys::{self, TermKey};
use bestterm_term_render::{TerminalMetrics, TerminalStyle};
use bestterm_transport::GridSize;
use bestterm_ui_chrome::{
    ChromeAction, ChromeState, ChromeTheme, SidebarPanel, StatusInfo, TabInfo, apply_theme,
    menu_bar, quick_connect_bar, ribbon, sidebar_strip, status_bar, tab_bar,
};
use egui::{CentralPanel, CornerRadius, EventFilter, Frame, Panel, Sense, Stroke};

use crate::tab::TerminalTab;

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
}

impl Default for BestTermApp {
    fn default() -> Self {
        Self::new()
    }
}

impl BestTermApp {
    /// Build the application and open one local shell.
    pub fn new() -> Self {
        let shells = discover();
        tracing::info!(count = shells.len(), "discovered local shells");

        let mut app = Self {
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
        };
        app.open_shell(0);
        app
    }

    /// Open a tab running `shells[index]`, or the first shell if the index is out of range.
    fn open_shell(&mut self, index: usize) {
        let Some(profile) = self.shells.get(index).or_else(|| self.shells.first()) else {
            tracing::error!("no shells available; cannot open a tab");
            return;
        };

        let (cols, rows) = (80, 24);
        match TerminalTab::spawn(profile, cols, rows, SCROLLBACK, self.palette.clone()) {
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
                ChromeAction::NewLocalShell => self.open_shell(0),
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
                    // SSH arrives in phase 2. Reported rather than silently dropped.
                    tracing::info!(%target, "quick connect requested; SSH lands in phase 2");
                    self.chrome.quick_connect.clear();
                }
                ChromeAction::OpenSessionDialog => {
                    tracing::info!("session dialog requested; it lands in phase 2");
                }
                ChromeAction::Unimplemented(what) => {
                    tracing::info!(control = what, "not implemented yet");
                }
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

        let output_arrived = self.pump();
        self.sync_chrome();

        let mut actions: Vec<ChromeAction> = Vec::new();

        // Cloned once per frame so the panel closures below borrow a local rather than `self`,
        // which would otherwise conflict with the two closures that need `&mut self`. The theme is
        // a handful of colours and floats; the clarity is worth more than the copy.
        let theme = self.theme.clone();

        // Panel order is layout order: first added is outermost. The central panel must be last.
        Panel::top("bestterm_menu_bar")
            .frame(chrome_frame(theme.menu_bg))
            .show(ui, |ui| menu_bar(ui, &mut actions));

        Panel::top("bestterm_ribbon")
            .exact_size(theme.ribbon_height)
            .frame(chrome_frame(theme.chrome_bg))
            .show(ui, |ui| ribbon(ui, &theme, &mut actions));

        Panel::top("bestterm_quick_connect")
            .exact_size(theme.quick_connect_height + 6.0)
            .frame(chrome_frame(theme.chrome_bg))
            .show(ui, |ui| {
                quick_connect_bar(ui, &mut self.chrome, &mut actions)
            });

        Panel::bottom("bestterm_status_bar")
            .exact_size(theme.status_bar_height)
            .frame(chrome_frame(theme.chrome_bg))
            .show(ui, |ui| status_bar(ui, &theme, &self.chrome.status));

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
                .show(ui, |ui| self.sidebar_contents(ui));
        }

        CentralPanel::no_frame().show(ui, |ui| {
            let chrome = self.chrome.clone();
            Panel::top("bestterm_tab_bar")
                .exact_size(theme.tab_bar_height)
                .frame(chrome_frame(theme.chrome_bg))
                .show(ui, |ui| tab_bar(ui, &theme, &chrome, &mut actions));

            self.terminal_ui(ui);
        });

        self.apply_actions(actions, &ctx);

        // Repaint on new output. Otherwise egui idles, which is exactly what we want: an idle
        // terminal must not burn a core redrawing an unchanged screen.
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
    fn sidebar_contents(&mut self, ui: &mut egui::Ui) {
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
                for (index, label) in shells {
                    if ui.selectable_label(false, label).double_clicked() {
                        self.open_shell(index);
                    }
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Saved sessions arrive in phase 2.")
                        .small()
                        .color(self.theme.text_dim),
                );
            }
            SidebarPanel::Tools => {
                ui.label("Tools");
            }
            SidebarPanel::Macros => {
                ui.label("Macros");
            }
            SidebarPanel::Sftp => {
                ui.label("Sftp");
                ui.label(
                    egui::RichText::new("The file browser arrives in phase 4.")
                        .small()
                        .color(self.theme.text_dim),
                );
            }
        }
    }
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
}
