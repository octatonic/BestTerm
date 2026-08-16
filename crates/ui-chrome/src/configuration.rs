//! The Configuration dialog.
//!
//! Measured from MobaXterm Professional 26.4.0.5512, from a screenshot of the dialog rather than from
//! memory — which is the rule the rest of `docs/ui-parity.md` is written under, and the reason the
//! tab list here is seven and not the eight or nine that guessing would produce.
//!
//! # What was measured
//!
//! Seven tabs, in this order: General, Terminal, X11, SSH, Display, Toolbar, Misc. Each carries an
//! icon to the left of its label. The General tab holds three path rows, a bordered group of five
//! links, and one checkbox; a single OK button sits centred at the foot of the dialog, outside the
//! tab area.
//!
//! The other six tabs are named and empty. That is deliberate and it is visible: an empty tab says
//! its contents have not been measured yet, where inventing plausible settings would produce a dialog
//! that looks finished and disagrees with the reference in ways nobody would think to check.
//!
//! # The links are links
//!
//! The five entries in the middle group open other things — right-click menu entries, keyboard
//! shortcuts, stored passwords, shared sessions, session presets — rather than setting anything.
//! They are reported as [`ConfigAction`] rather than toggled, because a row that looks like a setting
//! and behaves like a button is the sort of thing that gets clicked once and never again.

use egui::{Response, Ui};

use crate::icons::{self, Icon};
use crate::theme::ChromeTheme;

/// Side of an icon on a dialog tab.
///
/// Smaller than the ribbon's 24: measured against the label's cap height in the reference, where the
/// icon and the text are about the same size.
const TAB_ICON: f32 = 16.0;

/// Side of the little icon in front of a link row.
const ROW_ICON: f32 = 16.0;

/// The dialog's tabs, in the order the reference shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigTab {
    /// Paths, and the links to the other editors.
    General,
    /// Terminal behaviour and appearance.
    Terminal,
    /// The X server.
    X11,
    /// SSH defaults.
    Ssh,
    /// Fonts, colours, themes.
    Display,
    /// What the toolbar itself shows.
    Toolbar,
    /// Everything with nowhere else to go.
    Misc,
}

impl ConfigTab {
    /// Every tab, in the measured order.
    pub const ALL: [Self; 7] = [
        Self::General,
        Self::Terminal,
        Self::X11,
        Self::Ssh,
        Self::Display,
        Self::Toolbar,
        Self::Misc,
    ];

    /// The tab's label, spelled as the reference spells it.
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Terminal => "Terminal",
            Self::X11 => "X11",
            Self::Ssh => "SSH",
            Self::Display => "Display",
            Self::Toolbar => "Toolbar",
            Self::Misc => "Misc",
        }
    }

    /// The picture beside the label.
    pub fn icon(self) -> Icon {
        match self {
            Self::General => Icon::General,
            Self::Terminal => Icon::Session,
            Self::X11 => Icon::X11,
            Self::Ssh => Icon::Ssh,
            Self::Display => Icon::Display,
            Self::Toolbar => Icon::Toolbar,
            Self::Misc => Icon::Misc,
        }
    }
}

/// The five rows in the General tab's middle group.
///
/// Each opens something else. See the module documentation for why they are actions rather than
/// settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigLink {
    /// Entries added to the shell's context menu.
    ShellIntegration,
    /// The keyboard shortcut editor.
    Shortcuts,
    /// The vault.
    Passwords,
    /// Sessions shared with other people.
    SharedSessions,
    /// Defaults new sessions start from.
    SessionPresets,
}

impl ConfigLink {
    /// Every row, in the measured order.
    pub const ALL: [Self; 5] = [
        Self::ShellIntegration,
        Self::Shortcuts,
        Self::Passwords,
        Self::SharedSessions,
        Self::SessionPresets,
    ];

    /// What the row says.
    ///
    /// Reworded from the reference where the reference names itself: "MobaXterm keyboard shortcuts"
    /// becomes "BestTerm keyboard shortcuts", because the product's name is a trademark and this is
    /// not that product. The rows that name no product are left alone.
    pub fn label(self) -> &'static str {
        match self {
            Self::ShellIntegration => "Windows right-click menu entries",
            Self::Shortcuts => "BestTerm keyboard shortcuts",
            Self::Passwords => "BestTerm passwords management",
            Self::SharedSessions => "Manage my shared sessions",
            Self::SessionPresets => "Edit my sessions presets",
        }
    }

    /// The picture in front of it.
    pub fn icon(self) -> Icon {
        match self {
            Self::ShellIntegration => Icon::Packages,
            Self::Shortcuts => Icon::Toolbar,
            Self::Passwords => Icon::Key,
            Self::SharedSessions => Icon::People,
            Self::SessionPresets => Icon::Sessions,
        }
    }
}

/// What the dialog asks the application to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigAction {
    /// One of the middle rows was clicked.
    Open(ConfigLink),
    /// Somebody wants to pick a directory for a path field.
    Browse(ConfigField),
    /// A field was emptied back to its default.
    Reset(ConfigField),
    /// The dialog was accepted.
    Accepted,
    /// The dialog was dismissed.
    Cancelled,
}

/// The editable paths on the General tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigField {
    /// Where a local shell starts.
    Home,
    /// What the shell sees as the root of the tree.
    Root,
    /// The program that opens a remote file for editing.
    Editor,
}

impl ConfigField {
    /// The label to the left of the field.
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Terminal home directory:",
            Self::Root => "Terminal root (/) directory:",
            Self::Editor => "Default text editor program:",
        }
    }
}

/// What the dialog is editing, and whether it is on screen.
pub struct Configuration {
    /// Whether the dialog is shown.
    pub open: bool,
    /// Which tab is selected.
    pub tab: ConfigTab,
    /// Where a local shell starts.
    pub home: String,
    /// What the shell sees as `/`.
    pub root: String,
    /// The editor for remote files.
    pub editor: String,
    /// Whether the configuration file is copied before it is rewritten.
    pub backup: bool,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            open: false,
            tab: ConfigTab::General,
            home: String::new(),
            root: String::new(),
            editor: String::new(),
            // On, as in the reference. A configuration file this program rewrites is one it can
            // corrupt, and the copy costs a few kilobytes.
            backup: true,
        }
    }
}

impl Configuration {
    /// Draw the dialog, if it is open, and report what happened.
    ///
    /// Returns every action the frame produced rather than the first: a click on a row and a click on
    /// OK can land in the same frame, and dropping either would lose work somebody did.
    pub fn show(&mut self, ctx: &egui::Context, theme: &ChromeTheme) -> Vec<ConfigAction> {
        let mut actions = Vec::new();
        if !self.open {
            return actions;
        }

        let mut open = true;
        egui::Window::new("BestTerm Configuration")
            .open(&mut open)
            .resizable(true)
            .default_width(660.0)
            .default_height(560.0)
            .show(ctx, |ui| {
                self.tab_strip(ui, theme);
                ui.add_space(8.0);

                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, theme.border))
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_min_height(360.0);
                        match self.tab {
                            ConfigTab::General => self.general(ui, theme, &mut actions),
                            // Named and empty, on purpose. See the module documentation.
                            other => {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(140.0);
                                    ui.weak(format!(
                                        "The {} settings have not been measured from the reference \
                                         yet.",
                                        other.label()
                                    ));
                                });
                            }
                        }
                    });

                // Outside the tab area and centred, as measured.
                ui.add_space(10.0);
                ui.vertical_centered(|ui| {
                    if icon_button(ui, Icon::Ok, "OK").clicked() {
                        actions.push(ConfigAction::Accepted);
                    }
                });
            });

        if !open {
            self.open = false;
            actions.push(ConfigAction::Cancelled);
        }
        if actions.contains(&ConfigAction::Accepted) {
            self.open = false;
        }
        actions
    }

    /// The row of tabs across the top.
    fn tab_strip(&mut self, ui: &mut Ui, theme: &ChromeTheme) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for tab in ConfigTab::ALL {
                let selected = self.tab == tab;
                let response = ui
                    .scope(|ui| {
                        if selected {
                            ui.style_mut().visuals.widgets.inactive.weak_bg_fill =
                                theme.selected_bg;
                        }
                        icon_button(ui, tab.icon(), tab.label())
                    })
                    .inner;
                if response.clicked() {
                    self.tab = tab;
                }
            }
        });
    }

    /// The General tab.
    fn general(&mut self, ui: &mut Ui, theme: &ChromeTheme, actions: &mut Vec<ConfigAction>) {
        for field in [ConfigField::Home, ConfigField::Root, ConfigField::Editor] {
            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, theme.border))
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        icon(ui, field_icon(field), ROW_ICON);
                        ui.add_space(6.0);
                        ui.label(field.label());
                        let value = match field {
                            ConfigField::Home => &mut self.home,
                            ConfigField::Root => &mut self.root,
                            ConfigField::Editor => &mut self.editor,
                        };
                        ui.add(
                            egui::TextEdit::singleline(value)
                                .desired_width(ui.available_width() - 64.0),
                        );
                        if small_icon_button(ui, Icon::Folder).clicked() {
                            actions.push(ConfigAction::Browse(field));
                        }
                        if small_icon_button(ui, Icon::Remove).clicked() {
                            actions.push(ConfigAction::Reset(field));
                        }
                    });
                });
            ui.add_space(6.0);
        }

        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, theme.border))
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        for link in ConfigLink::ALL {
                            ui.horizontal(|ui| {
                                icon(ui, link.icon(), ROW_ICON);
                                ui.add_space(6.0);
                                if ui.link(link.label()).clicked() {
                                    actions.push(ConfigAction::Open(link));
                                }
                            });
                            ui.add_space(4.0);
                        }
                    });
                });
        });

        ui.add_space(10.0);
        ui.checkbox(
            &mut self.backup,
            "Automatically backup BestTerm configuration file",
        );
    }
}

/// The picture beside a path field.
fn field_icon(field: ConfigField) -> Icon {
    match field {
        ConfigField::Home | ConfigField::Root => Icon::Folder,
        ConfigField::Editor => Icon::File,
    }
}

/// Draw an icon inline, taking the space it needs.
fn icon(ui: &mut Ui, which: Icon, side: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        icons::draw(ui.painter(), rect, which);
    }
}

/// A button carrying an icon and a label.
fn icon_button(ui: &mut Ui, which: Icon, label: &str) -> Response {
    ui.scope(|ui| {
        ui.horizontal(|ui| {
            let response = ui.button(format!("      {label}"));
            // Painted over the button's own left padding, so the label's baseline is the button's
            // and the icon does not push the text out of the reference's spacing.
            let side = TAB_ICON.min(response.rect.height() - 4.0);
            let centre = egui::pos2(
                response.rect.left() + 6.0 + side / 2.0,
                response.rect.center().y,
            );
            icons::draw(
                ui.painter(),
                egui::Rect::from_center_size(centre, egui::vec2(side, side)),
                which,
            );
            response
        })
        .inner
    })
    .inner
}

/// A square button with only a picture on it.
fn small_icon_button(ui: &mut Ui, which: Icon) -> Response {
    let side = 20.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius::ZERO,
                ui.visuals().widgets.hovered.bg_fill,
            );
        }
        icons::draw(ui.painter(), rect.shrink(2.0), which);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tabs_are_the_seven_that_were_measured_in_the_order_they_appear() {
        // From a screenshot of MobaXterm Professional 26.4.0.5512, not from memory. Guessing this
        // list produces eight or nine plausible tabs, none of which is what the dialog has.
        let labels: Vec<&str> = ConfigTab::ALL.iter().map(|tab| tab.label()).collect();
        assert_eq!(
            labels,
            vec![
                "General", "Terminal", "X11", "SSH", "Display", "Toolbar", "Misc"
            ]
        );
    }

    #[test]
    fn the_general_tab_has_the_five_rows_that_were_measured() {
        assert_eq!(ConfigLink::ALL.len(), 5);
        let labels: Vec<&str> = ConfigLink::ALL.iter().map(|row| row.label()).collect();
        assert_eq!(labels[0], "Windows right-click menu entries");
        assert_eq!(labels[3], "Manage my shared sessions");
        assert_eq!(labels[4], "Edit my sessions presets");
    }

    #[test]
    fn nothing_visible_carries_the_other_product_s_name() {
        // The layout is reproduced; the name is a trademark and is not ours to use. The rows that
        // name a product in the reference name this one instead, and the rows that name none are
        // left exactly as measured.
        let mut visible: Vec<String> = ConfigLink::ALL
            .iter()
            .map(|row| row.label().to_string())
            .collect();
        visible.extend(ConfigTab::ALL.iter().map(|tab| tab.label().to_string()));
        visible.extend(
            [ConfigField::Home, ConfigField::Root, ConfigField::Editor]
                .iter()
                .map(|field| field.label().to_string()),
        );

        for text in &visible {
            assert!(
                !text.to_lowercase().contains("moba"),
                "the reference's name must not appear in our interface: {text}"
            );
        }
    }

    #[test]
    fn every_tab_and_row_has_a_picture_of_its_own() {
        // Two tabs sharing an icon is how a strip of seven becomes unreadable at a glance, which is
        // the only thing the icons are for.
        let mut seen = std::collections::HashSet::new();
        for tab in ConfigTab::ALL {
            assert!(seen.insert(tab.icon()), "{:?} repeats an icon", tab);
        }
        let mut seen = std::collections::HashSet::new();
        for link in ConfigLink::ALL {
            assert!(seen.insert(link.icon()), "{:?} repeats an icon", link);
        }
    }

    #[test]
    fn a_closed_dialog_reports_nothing() {
        let ctx = egui::Context::default();
        let theme = ChromeTheme::light();
        let mut config = Configuration::default();
        assert!(!config.open);

        let mut actions = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            actions = config.show(ui.ctx(), &theme);
        });
        output.textures_delta.clear();
        assert!(actions.is_empty());
    }

    #[test]
    fn an_open_dialog_draws_and_stays_open_until_something_closes_it() {
        let ctx = egui::Context::default();
        let theme = ChromeTheme::light();
        let mut config = Configuration {
            open: true,
            ..Configuration::default()
        };

        let mut actions = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            actions = config.show(ui.ctx(), &theme);
        });
        output.textures_delta.clear();

        assert!(actions.is_empty(), "nothing was clicked: {actions:?}");
        assert!(config.open, "drawing a dialog must not dismiss it");
        assert_eq!(config.tab, ConfigTab::General);
    }

    #[test]
    fn the_backup_checkbox_starts_on() {
        // As in the reference. A configuration file this program rewrites is one it can corrupt.
        assert!(Configuration::default().backup);
    }
}
