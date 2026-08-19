//! The application theme.
//!
//! The target is the reference application's look: a dense, light, square-cornered, thin-bordered
//! grey-blue chrome. `egui`'s defaults are the opposite of that — rounded, airy, dark — so this
//! replaces [`egui::Style`] wholesale rather than nudging it. Nudging never converges.
//!
//! # Provisional values
//!
//! Every constant marked `PROVISIONAL` is a placeholder awaiting a real measurement from the capture
//! procedure in `docs/ui-parity.md`. They are named and grouped here, rather than scattered through
//! the widget code, precisely so the gap is visible: filling them in is a change to this file.

use egui::{Color32, CornerRadius, Margin, Stroke, Style, Vec2, vec2};

/// Colours and metrics for the chrome.
#[derive(Clone, Debug, PartialEq)]
pub struct ChromeTheme {
    /// Background of panels: ribbon, sidebar, status bar.
    pub chrome_bg: Color32,
    /// Background of the menu bar.
    pub menu_bg: Color32,
    /// Hairline separator colour.
    pub separator: Color32,
    /// Border around sunken controls.
    pub border: Color32,
    /// Background of text entry and other sunken areas.
    pub sunken_bg: Color32,
    /// Primary text.
    pub text: Color32,
    /// De-emphasised text: status bar detail, disabled labels.
    pub text_dim: Color32,
    /// Fill of a hovered control.
    pub hover_bg: Color32,
    /// Fill of a pressed or selected control.
    pub selected_bg: Color32,
    /// Text on a selected control.
    pub selected_text: Color32,
    /// Fill of the active tab.
    pub tab_active_bg: Color32,
    /// Fill of an inactive tab.
    pub tab_inactive_bg: Color32,
    /// Text for something that went wrong.
    ///
    /// Dark enough to read on the light chrome rather than the pure red every framework reaches
    /// for, which on a grey panel is bright and hard to read at this text size.
    pub warning: Color32,

    /// Height of the ribbon toolbar. `PROVISIONAL`
    pub ribbon_height: f32,
    /// Width of a ribbon button. `PROVISIONAL`
    pub ribbon_button_width: f32,
    /// Height of the quick-connect bar. `PROVISIONAL`
    pub quick_connect_height: f32,
    /// Width of the always-visible vertical tab strip on the left edge. `PROVISIONAL`
    pub sidebar_strip_width: f32,
    /// Default width of the expanded left panel. `PROVISIONAL`
    pub sidebar_width: f32,
    /// Minimum width the left panel can be dragged to. `PROVISIONAL`
    pub sidebar_min_width: f32,
    /// Height of the tab bar. `PROVISIONAL`
    pub tab_bar_height: f32,
    /// Height of the status bar. `PROVISIONAL`
    pub status_bar_height: f32,
    /// UI font size. `PROVISIONAL`
    pub font_size: f32,
}

impl Default for ChromeTheme {
    fn default() -> Self {
        Self::light()
    }
}

impl ChromeTheme {
    /// The light theme, approximating the reference application.
    pub fn light() -> Self {
        Self {
            chrome_bg: Color32::from_rgb(0xF0, 0xF0, 0xF0),
            menu_bg: Color32::from_rgb(0xF6, 0xF6, 0xF6),
            separator: Color32::from_rgb(0xC8, 0xC8, 0xC8),
            border: Color32::from_rgb(0xA0, 0xA6, 0xAE),
            sunken_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            text: Color32::from_rgb(0x1A, 0x1A, 0x1A),
            text_dim: Color32::from_rgb(0x5A, 0x5F, 0x66),
            hover_bg: Color32::from_rgb(0xDE, 0xE8, 0xF5),
            selected_bg: Color32::from_rgb(0xB6, 0xCF, 0xEB),
            selected_text: Color32::from_rgb(0x0A, 0x0A, 0x0A),
            tab_active_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            tab_inactive_bg: Color32::from_rgb(0xE2, 0xE2, 0xE2),
            warning: Color32::from_rgb(0xA6, 0x1B, 0x1B),

            // Measured from MobaXterm Professional 26.4.0.5512; see docs/ui-parity.md. The three
            // top bands together occupy 99 px in the reference, and the numbers below add up to
            // that: an 18 px menu bar, a 57 px ribbon and a 24 px row for quick connect and tabs.
            ribbon_height: 57.0,
            ribbon_button_width: 57.0,
            quick_connect_height: 24.0,
            // 35 px of strip, then a hairline, then 299 px of panel -- which is 336 in total, and the
            // reference's own settings file says SidebarWidth=336. The only figure here with
            // independent confirmation.
            sidebar_strip_width: 35.0,
            sidebar_width: 299.0,
            sidebar_min_width: 120.0,
            tab_bar_height: 24.0,
            status_bar_height: 20.0,
            font_size: 12.0,
        }
    }

    /// Padding used inside chrome panels.
    pub fn panel_margin(&self) -> Margin {
        Margin::symmetric(4, 2)
    }

    /// Spacing between adjacent chrome controls.
    pub fn item_spacing(&self) -> Vec2 {
        vec2(3.0, 2.0)
    }
}

/// Install `theme` into `ctx`.
///
/// Call once at startup and again whenever the theme changes; `egui` styles are cheap to replace.
pub fn apply(ctx: &egui::Context, theme: &ChromeTheme) {
    let mut style = Style::default();

    style.visuals.dark_mode = false;
    style.visuals.override_text_color = Some(theme.text);
    style.visuals.panel_fill = theme.chrome_bg;
    style.visuals.window_fill = theme.chrome_bg;
    style.visuals.extreme_bg_color = theme.sunken_bg;
    style.visuals.faint_bg_color = theme.menu_bg;
    style.visuals.window_stroke = Stroke::new(1.0, theme.border);

    // Square everything. This single change does more for the resemblance than any colour.
    style.visuals.window_corner_radius = CornerRadius::ZERO;
    style.visuals.menu_corner_radius = CornerRadius::ZERO;

    style.visuals.selection.bg_fill = theme.selected_bg;
    style.visuals.selection.stroke = Stroke::new(1.0, theme.selected_text);

    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::ZERO;
        widget.bg_stroke = Stroke::new(1.0, theme.separator);
        widget.fg_stroke = Stroke::new(1.0, theme.text);
        // No grow-on-hover: chrome controls in the reference do not move.
        widget.expansion = 0.0;
    }

    style.visuals.widgets.noninteractive.bg_fill = theme.chrome_bg;
    style.visuals.widgets.noninteractive.weak_bg_fill = theme.chrome_bg;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, theme.separator);

    style.visuals.widgets.inactive.bg_fill = theme.chrome_bg;
    style.visuals.widgets.inactive.weak_bg_fill = theme.chrome_bg;
    // An unhovered toolbar button shows no border in the reference; it appears on hover.
    style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;

    style.visuals.widgets.hovered.bg_fill = theme.hover_bg;
    style.visuals.widgets.hovered.weak_bg_fill = theme.hover_bg;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, theme.border);

    style.visuals.widgets.active.bg_fill = theme.selected_bg;
    style.visuals.widgets.active.weak_bg_fill = theme.selected_bg;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, theme.border);

    style.visuals.widgets.open.bg_fill = theme.selected_bg;
    style.visuals.widgets.open.weak_bg_fill = theme.selected_bg;

    // Dense spacing. This is the other half of the resemblance.
    style.spacing.item_spacing = theme.item_spacing();
    style.spacing.button_padding = vec2(4.0, 2.0);
    style.spacing.menu_margin = Margin::symmetric(2, 2);
    style.spacing.window_margin = theme.panel_margin();
    style.spacing.interact_size = vec2(20.0, 18.0);
    style.spacing.indent = 14.0;

    for font in style.text_styles.values_mut() {
        font.size = theme.font_size;
    }

    // Pin the look. `egui` keeps a separate style per theme and switches between them with the OS
    // preference; BestTerm has one chrome to reproduce, so both slots get the same style and the
    // preference is fixed. Otherwise a user with a dark desktop would get a half-converted theme.
    ctx.set_theme(egui::ThemePreference::Light);
    ctx.all_styles_mut(|slot| *slot = style.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_theme_metrics_are_positive() {
        let t = ChromeTheme::light();
        for (name, value) in [
            ("ribbon_height", t.ribbon_height),
            ("ribbon_button_width", t.ribbon_button_width),
            ("quick_connect_height", t.quick_connect_height),
            ("sidebar_strip_width", t.sidebar_strip_width),
            ("sidebar_width", t.sidebar_width),
            ("sidebar_min_width", t.sidebar_min_width),
            ("tab_bar_height", t.tab_bar_height),
            ("status_bar_height", t.status_bar_height),
            ("font_size", t.font_size),
        ] {
            assert!(value > 0.0, "{name} must be positive, got {value}");
        }
    }

    #[test]
    fn sidebar_minimum_is_below_its_default() {
        let t = ChromeTheme::light();
        assert!(t.sidebar_min_width < t.sidebar_width);
        assert!(t.sidebar_strip_width < t.sidebar_min_width);
    }

    #[test]
    fn applying_the_theme_squares_the_corners() {
        let ctx = egui::Context::default();
        let theme = ChromeTheme::light();
        apply(&ctx, &theme);

        let style = ctx.style_of(egui::Theme::Light);
        assert!(!style.visuals.dark_mode);
        assert_eq!(style.visuals.window_corner_radius, CornerRadius::ZERO);
        assert_eq!(
            style.visuals.widgets.inactive.corner_radius,
            CornerRadius::ZERO
        );
        assert_eq!(style.visuals.panel_fill, theme.chrome_bg);
        assert_eq!(style.spacing.item_spacing, theme.item_spacing());
    }

    #[test]
    fn both_theme_slots_get_the_same_style() {
        // A user with a dark desktop must still get BestTerm's chrome, not half of it.
        let ctx = egui::Context::default();
        apply(&ctx, &ChromeTheme::light());
        let light = ctx.style_of(egui::Theme::Light);
        let dark = ctx.style_of(egui::Theme::Dark);
        assert_eq!(light.visuals.panel_fill, dark.visuals.panel_fill);
        assert!(!dark.visuals.dark_mode);
    }

    #[test]
    fn applying_the_theme_sets_every_text_style_to_one_size() {
        let ctx = egui::Context::default();
        let theme = ChromeTheme::light();
        apply(&ctx, &theme);
        for font in ctx.style_of(egui::Theme::Light).text_styles.values() {
            assert_eq!(font.size, theme.font_size);
        }
    }
}
