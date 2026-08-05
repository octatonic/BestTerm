//! The application chrome.
//!
//! Structure comes from [`docs/ui-parity.md`](../../../docs/ui-parity.md), which is the source of
//! truth for this crate. Menu titles, their order, the ribbon button sequence and the sidebar tabs
//! are reproduced from it deliberately and must not be "improved" — recognisability is the feature.
//!
//! Every widget here reports what the user did as a [`ChromeAction`] rather than acting itself. The
//! chrome therefore holds no application state and needs no access to sessions, which is what keeps
//! it a pure view layer.
//!
//! # Phase 0 scope
//!
//! The frame is real: panels, strip, tabs and status bar lay out and respond. Ribbon buttons show
//! placeholder glyph boxes because the icon set is not wired up until phase 1, and menu items that
//! have no implementation yet report [`ChromeAction::Unimplemented`] instead of silently doing
//! nothing.

pub mod theme;

pub use theme::{ChromeTheme, apply as apply_theme};

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Response, Sense, Stroke, TextStyle,
    Ui, epaint::TextShape, pos2, vec2,
};

/// Quarter turn anticlockwise, so sidebar labels read bottom-to-top.
const ROTATE_CCW: f32 = -std::f32::consts::FRAC_PI_2;

/// The panels reachable from the left edge strip, in the reference's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarPanel {
    /// The session tree.
    Sessions,
    /// Local and remote tools.
    Tools,
    /// Recorded macros.
    Macros,
    /// The file browser for the active session.
    Sftp,
}

impl SidebarPanel {
    /// Every panel, in display order.
    pub const ALL: [Self; 4] = [Self::Sessions, Self::Tools, Self::Macros, Self::Sftp];

    /// The label shown on the edge strip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Tools => "Tools",
            Self::Macros => "Macros",
            Self::Sftp => "Sftp",
        }
    }
}

/// A tab as the chrome needs to draw it.
#[derive(Clone, Debug, PartialEq)]
pub struct TabInfo {
    /// Text shown on the tab.
    pub title: String,
    /// Protocol identifier, used to pick the icon.
    pub protocol: String,
    /// Per-session tab colour, imported from `.mxtsessions` where present.
    pub tint: Option<Color32>,
}

/// What the status bar shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusInfo {
    /// The `DISPLAY` value when an X server is running, `None` when it is not.
    pub x_display: Option<String>,
    /// Grid size of the focused pane, as `(cols, rows)`.
    pub grid: (usize, usize),
    /// Description of the focused session.
    pub session: String,
}

/// Chrome state owned by the application and lent to the widgets.
#[derive(Clone, Debug)]
pub struct ChromeState {
    /// Whether the left panel is expanded.
    pub sidebar_open: bool,
    /// Which panel the left strip has selected.
    pub sidebar_panel: SidebarPanel,
    /// Contents of the quick-connect field.
    pub quick_connect: String,
    /// Open tabs, in order.
    pub tabs: Vec<TabInfo>,
    /// Index into [`Self::tabs`].
    pub active_tab: usize,
    /// Status bar contents.
    pub status: StatusInfo,
}

impl Default for ChromeState {
    fn default() -> Self {
        Self {
            sidebar_open: true,
            sidebar_panel: SidebarPanel::Sessions,
            quick_connect: String::new(),
            tabs: Vec::new(),
            active_tab: 0,
            status: StatusInfo::default(),
        }
    }
}

/// Something the user asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum ChromeAction {
    /// Open a new local shell tab.
    NewLocalShell,
    /// Focus a tab.
    SelectTab(usize),
    /// Close a tab.
    CloseTab(usize),
    /// Connect to the target typed in the quick-connect field.
    QuickConnect(String),
    /// Expand or collapse the left panel.
    ToggleSidebar,
    /// Switch the left panel.
    SelectSidebarPanel(SidebarPanel),
    /// Open the session dialog.
    OpenSessionDialog,
    /// Quit the application.
    Quit,
    /// A control that exists in the layout but has no behaviour yet.
    ///
    /// Reported rather than ignored so that the gap is visible in the UI and in logs, instead of
    /// looking like a bug.
    Unimplemented(&'static str),
}

/// Menu titles and their items, in the reference's order.
///
/// Items are placeholders pending the enumeration described in `docs/ui-parity.md`; the *titles* and
/// their order are already correct and are what the layout is judged on.
const MENUS: &[(&str, &[&str])] = &[
    ("Terminal", &["New local shell", "Clear buffer", "Log session"]),
    ("Sessions", &["New session…", "Save session", "Import…"]),
    ("View", &["Toggle sidebar", "Toggle status bar", "Full screen"]),
    ("Split", &["Two panes", "Three panes", "Four panes"]),
    ("MultiExec", &["Start MultiExec", "Stop MultiExec"]),
    ("Tunneling", &["Tunnel manager…", "New tunnel…"]),
    ("Packages", &["Package manager…"]),
    ("Settings", &["Preferences…", "Keyboard shortcuts…"]),
    ("Macros", &["Record macro", "Stop recording", "Manage macros…"]),
    ("Help", &["Documentation", "About BestTerm"]),
];

/// Ribbon buttons, in the reference's order.
///
/// `Games` is absent by decision, and `Packages` is retained as a reserved slot; both are recorded
/// in `docs/ui-parity.md` with the reasoning.
const RIBBON: &[&str] = &[
    "Session",
    "Servers",
    "Tools",
    "Sessions",
    "View",
    "Split",
    "MultiExec",
    "Tunneling",
    "Packages",
    "Settings",
    "Help",
    "X server",
    "Exit",
];

/// The menu bar.
pub fn menu_bar(ui: &mut Ui, actions: &mut Vec<ChromeAction>) {
    ui.horizontal(|ui| {
        for (title, items) in MENUS {
            ui.menu_button(*title, |ui| {
                for item in *items {
                    if ui.button(*item).clicked() {
                        actions.push(menu_action(title, item));
                        ui.close();
                    }
                }
            });
        }
    });
}

/// Map a menu item to its action.
///
/// Split out so the mapping is unit-testable without a UI: this table grows to several hundred
/// entries as the phases land, and a table that large needs tests.
fn menu_action(menu: &'static str, item: &'static str) -> ChromeAction {
    match (menu, item) {
        ("Terminal", "New local shell") => ChromeAction::NewLocalShell,
        ("Sessions", "New session…") => ChromeAction::OpenSessionDialog,
        ("View", "Toggle sidebar") => ChromeAction::ToggleSidebar,
        _ => ChromeAction::Unimplemented(item),
    }
}

/// The ribbon toolbar: one row of icon-over-label buttons.
pub fn ribbon(ui: &mut Ui, theme: &ChromeTheme, actions: &mut Vec<ChromeAction>) {
    ui.horizontal(|ui| {
        for label in RIBBON {
            if ribbon_button(ui, theme, label).clicked() {
                actions.push(ribbon_action(label));
            }
        }
    });
}

fn ribbon_action(label: &'static str) -> ChromeAction {
    match label {
        "Session" => ChromeAction::OpenSessionDialog,
        "Exit" => ChromeAction::Quit,
        other => ChromeAction::Unimplemented(other),
    }
}

/// One ribbon button: a placeholder glyph box above a label.
///
/// The box becomes the real icon in phase 1. Drawing a visible placeholder rather than nothing keeps
/// the ribbon's true height and spacing under test from the start.
fn ribbon_button(ui: &mut Ui, theme: &ChromeTheme, label: &str) -> Response {
    let size = vec2(theme.ribbon_button_width, theme.ribbon_height - 4.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.is_pointer_button_down_on() {
            painter.rect_filled(rect, CornerRadius::ZERO, theme.selected_bg);
        } else if response.hovered() {
            painter.rect_filled(rect, CornerRadius::ZERO, theme.hover_bg);
            painter.rect_stroke(
                rect,
                CornerRadius::ZERO,
                Stroke::new(1.0, theme.border),
                egui::StrokeKind::Inside,
            );
        }

        let icon_side = 24.0;
        let icon = Rect::from_center_size(
            pos2(rect.center().x, rect.top() + 4.0 + icon_side / 2.0),
            vec2(icon_side, icon_side),
        );
        painter.rect_stroke(
            icon,
            CornerRadius::ZERO,
            Stroke::new(1.0, theme.text_dim),
            egui::StrokeKind::Inside,
        );

        painter.text(
            pos2(rect.center().x, icon.bottom() + 2.0),
            Align2::CENTER_TOP,
            label,
            TextStyle::Small.resolve(ui.style()),
            theme.text,
        );
    }

    response
}

/// The quick-connect bar.
pub fn quick_connect_bar(ui: &mut Ui, state: &mut ChromeState, actions: &mut Vec<ChromeAction>) {
    ui.horizontal(|ui| {
        ui.label("Quick connect:");
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.quick_connect)
                .hint_text("user@host:port")
                .desired_width(240.0),
        );
        let submitted =
            field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (submitted || ui.button("Go").clicked()) && !state.quick_connect.trim().is_empty() {
            actions.push(ChromeAction::QuickConnect(
                state.quick_connect.trim().to_string(),
            ));
        }
    });
}

/// The always-visible vertical tab strip on the left edge.
///
/// The rotated labels are the layout's signature element, and the strip stays visible when the panel
/// is collapsed — clicking the selected tab collapses, clicking another switches and expands.
pub fn sidebar_strip(ui: &mut Ui, theme: &ChromeTheme, state: &ChromeState, actions: &mut Vec<ChromeAction>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        for panel in SidebarPanel::ALL {
            let selected = state.sidebar_panel == panel && state.sidebar_open;
            if vertical_tab(ui, theme, panel.label(), selected).clicked() {
                if state.sidebar_panel == panel {
                    actions.push(ChromeAction::ToggleSidebar);
                } else {
                    actions.push(ChromeAction::SelectSidebarPanel(panel));
                    if !state.sidebar_open {
                        actions.push(ChromeAction::ToggleSidebar);
                    }
                }
            }
        }
    });
}

/// A tab with its label rotated a quarter turn anticlockwise.
fn vertical_tab(ui: &mut Ui, theme: &ChromeTheme, label: &str, selected: bool) -> Response {
    let font: FontId = TextStyle::Body.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, theme.text);

    // Rotated, the galley's width becomes the tab's height.
    let padding = 10.0;
    let size = vec2(theme.sidebar_strip_width, galley.rect.width() + padding);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let fill = if selected {
            theme.selected_bg
        } else if response.hovered() {
            theme.hover_bg
        } else {
            theme.tab_inactive_bg
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::ZERO, fill);
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(1.0, theme.separator),
            egui::StrokeKind::Inside,
        );

        // Anchoring on the galley's centre keeps the text centred in the tab after rotation; the
        // pre-shift by `-centre` is what makes `with_angle_and_anchor` land it on `rect.center()`.
        let centre = galley.rect.center().to_vec2();
        painter.add(
            TextShape::new(rect.center() - centre, galley, theme.text)
                .with_angle_and_anchor(ROTATE_CCW, Align2::CENTER_CENTER),
        );
    }

    response
}

/// The tab bar, plus the `+` button that opens a new local shell.
pub fn tab_bar(ui: &mut Ui, theme: &ChromeTheme, state: &ChromeState, actions: &mut Vec<ChromeAction>) {
    egui::ScrollArea::horizontal()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 1.0;
                for (index, tab) in state.tabs.iter().enumerate() {
                    let active = index == state.active_tab;
                    let (clicked, closed) = tab_widget(ui, theme, tab, active);
                    if closed {
                        actions.push(ChromeAction::CloseTab(index));
                    } else if clicked {
                        actions.push(ChromeAction::SelectTab(index));
                    }
                }
                if ui
                    .button("+")
                    .on_hover_text("New local shell")
                    .clicked()
                {
                    actions.push(ChromeAction::NewLocalShell);
                }
            });
        });
}

/// Returns `(clicked, close_requested)`.
fn tab_widget(ui: &mut Ui, theme: &ChromeTheme, tab: &TabInfo, active: bool) -> (bool, bool) {
    let mut close_requested = false;

    let response = ui
        .scope(|ui| {
            let fill = tab.tint.unwrap_or(if active {
                theme.tab_active_bg
            } else {
                theme.tab_inactive_bg
            });
            egui::Frame::NONE
                .fill(fill)
                .stroke(Stroke::new(1.0, theme.separator))
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Placeholder for the protocol icon; real icons land in phase 1.
                        // Taken by character, not by byte: slicing `&s[..1]` panics on a
                        // multi-byte first character.
                        let initial: String =
                            tab.protocol.chars().next().into_iter().collect();
                        ui.label(egui::RichText::new(initial).small());
                        ui.label(&tab.title);
                        if ui.small_button("x").clicked() {
                            close_requested = true;
                        }
                    });
                });
        })
        .response;

    let clicked = response.interact(Sense::click()).clicked();
    (clicked && !close_requested, close_requested)
}

/// The status bar.
pub fn status_bar(ui: &mut Ui, theme: &ChromeTheme, status: &StatusInfo) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            let x_label = match &status.x_display {
                Some(display) => format!("X server: running  ·  DISPLAY={display}"),
                None => "X server: stopped".to_string(),
            };
            ui.label(egui::RichText::new(x_label).color(theme.text_dim));
            ui.separator();
            ui.label(
                egui::RichText::new(format!("{}x{}", status.grid.0, status.grid.1))
                    .color(theme.text_dim),
            );
            if !status.session.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new(&status.session).color(theme.text_dim));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_titles_and_order_match_the_parity_spec() {
        let titles: Vec<&str> = MENUS.iter().map(|(title, _)| *title).collect();
        assert_eq!(
            titles,
            vec![
                "Terminal",
                "Sessions",
                "View",
                "Split",
                "MultiExec",
                "Tunneling",
                "Packages",
                "Settings",
                "Macros",
                "Help",
            ]
        );
    }

    #[test]
    fn every_menu_has_items() {
        for (title, items) in MENUS {
            assert!(!items.is_empty(), "menu {title} has no items");
        }
    }

    #[test]
    fn ribbon_starts_with_session_and_ends_with_exit() {
        assert_eq!(RIBBON.first(), Some(&"Session"));
        assert_eq!(RIBBON.last(), Some(&"Exit"));
    }

    #[test]
    fn ribbon_omits_games_and_keeps_packages() {
        // Both are deliberate decisions recorded in docs/ui-parity.md.
        assert!(!RIBBON.contains(&"Games"));
        assert!(RIBBON.contains(&"Packages"));
    }

    #[test]
    fn wired_menu_items_map_to_real_actions() {
        assert_eq!(
            menu_action("Terminal", "New local shell"),
            ChromeAction::NewLocalShell
        );
        assert_eq!(
            menu_action("Sessions", "New session…"),
            ChromeAction::OpenSessionDialog
        );
        assert_eq!(
            menu_action("View", "Toggle sidebar"),
            ChromeAction::ToggleSidebar
        );
    }

    #[test]
    fn unwired_menu_items_are_reported_rather_than_ignored() {
        assert_eq!(
            menu_action("Help", "About BestTerm"),
            ChromeAction::Unimplemented("About BestTerm")
        );
    }

    #[test]
    fn every_menu_item_maps_to_something() {
        for (menu, items) in MENUS {
            for item in *items {
                // Must not panic, and must produce an action.
                let _ = menu_action(menu, item);
            }
        }
    }

    #[test]
    fn ribbon_actions_cover_every_button() {
        for label in RIBBON {
            let _ = ribbon_action(label);
        }
        assert_eq!(ribbon_action("Exit"), ChromeAction::Quit);
        assert_eq!(ribbon_action("Session"), ChromeAction::OpenSessionDialog);
    }

    #[test]
    fn sidebar_panels_are_in_the_reference_order() {
        let labels: Vec<&str> = SidebarPanel::ALL.iter().map(|p| p.label()).collect();
        assert_eq!(labels, vec!["Sessions", "Tools", "Macros", "Sftp"]);
    }

    #[test]
    fn default_state_opens_on_the_session_tree() {
        let state = ChromeState::default();
        assert!(state.sidebar_open);
        assert_eq!(state.sidebar_panel, SidebarPanel::Sessions);
        assert!(state.tabs.is_empty());
    }

    #[test]
    fn rotation_is_a_quarter_turn_anticlockwise() {
        assert!((ROTATE_CCW + std::f32::consts::FRAC_PI_2).abs() < f32::EPSILON);
    }
}
