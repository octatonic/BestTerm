//! Application preferences.
//!
//! These are the settings that are not per-session: how the terminal looks, how the application
//! behaves. The per-session ones live in the session tree, where they inherit.
//!
//! The two meet at [`AppSettings::defaults`], which is the root of the inheritance chain: a value set
//! there applies to every session that does not override it, including sessions in no folder at all.

use bestterm_core_model::{ResolvedSettings, SettingsOverride};
use serde::{Deserialize, Serialize};

use crate::store::Document;

/// What the bell does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BellStyle {
    /// Nothing.
    None,
    /// Flash the pane.
    #[default]
    Visual,
    /// Ask the desktop for attention on the taskbar.
    Urgent,
}

/// Shape of the text cursor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorStyle {
    /// Filled block.
    #[default]
    Block,
    /// Underscore.
    Underline,
    /// Vertical bar.
    Beam,
}

/// How the terminal looks and reacts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalSettings {
    /// Font family.
    pub font_family: String,
    /// Font size in points.
    pub font_size: f32,
    /// Multiplier on the font's natural row height.
    pub line_height: f32,
    /// Named colour palette.
    pub palette: String,
    /// Cursor shape.
    pub cursor_style: CursorStyle,
    /// Whether the cursor blinks.
    pub cursor_blink: bool,
    /// What the bell does.
    pub bell: BellStyle,
    /// Put a selection on the clipboard as soon as it is made.
    pub copy_on_select: bool,
    /// Paste on middle click, as X11 users expect.
    pub paste_on_middle_click: bool,
    /// Characters that end a word for double-click selection.
    pub word_separators: String,
    /// Warn before pasting text containing a newline.
    ///
    /// A newline in a paste executes whatever preceded it. This is the standard defence against a
    /// copied command running before it has been read, and it is on by default for that reason.
    pub warn_on_multiline_paste: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: "monospace".to_string(),
            font_size: 14.0,
            line_height: 1.0,
            palette: "xterm".to_string(),
            cursor_style: CursorStyle::default(),
            cursor_blink: true,
            bell: BellStyle::default(),
            // Off: silently replacing the clipboard whenever the mouse crosses the terminal loses
            // whatever the user had copied from elsewhere.
            copy_on_select: false,
            paste_on_middle_click: cfg!(unix),
            word_separators: " \t\n\"'`()[]{}<>|;:,".to_string(),
            warn_on_multiline_paste: true,
        }
    }
}

/// How the application behaves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BehaviourSettings {
    /// Ask before closing a window with live sessions.
    pub confirm_close_live_sessions: bool,
    /// Reopen the tabs and splits from last time.
    pub restore_layout_on_start: bool,
    /// Shell id from discovery to use for new local tabs; `None` means the discovered default.
    pub default_shell: Option<String>,
    /// Where session transcripts go; `None` means the state directory.
    pub log_directory: Option<String>,
    /// Lines to keep when a pane is scrolled back and new output arrives.
    pub scroll_on_output: bool,
}

impl Default for BehaviourSettings {
    fn default() -> Self {
        Self {
            confirm_close_live_sessions: true,
            restore_layout_on_start: true,
            default_shell: None,
            log_directory: None,
            // Off: jumping to the bottom while the user is reading scrollback is the single most
            // irritating behaviour a terminal can have.
            scroll_on_output: false,
        }
    }
}

/// The preferences file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppSettings {
    /// Terminal appearance and interaction.
    pub terminal: TerminalSettings,
    /// Application behaviour.
    pub behaviour: BehaviourSettings,
    /// Root of the session-settings inheritance chain.
    ///
    /// Anything set here applies to every session that does not override it, and to sessions that sit
    /// in no folder. It is what makes "set the keepalive once, everywhere" possible even for a flat
    /// tree.
    pub defaults: SettingsOverride,
}

impl AppSettings {
    /// The base a session tree resolves against.
    pub fn session_defaults(&self) -> ResolvedSettings {
        let mut resolved = ResolvedSettings::default();
        self.defaults.apply_to(&mut resolved);
        resolved
    }
}

impl Document for AppSettings {
    const VERSION: u32 = 1;
    const NAME: &'static str = "settings";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    #[test]
    fn defaults_round_trip_through_the_store() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");

        let original = AppSettings::default();
        store::save(&path, &original).expect("saves");
        let loaded: AppSettings = store::load(&path).expect("loads");
        assert_eq!(loaded, original);
    }

    #[test]
    fn a_partial_file_fills_the_rest_from_defaults() {
        // Hand-written config files are always partial; every missing key must have an answer.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "version = 1\n[terminal]\nfont_size = 18.0\n").expect("writes");

        let loaded: AppSettings = store::load(&path).expect("loads");
        assert_eq!(loaded.terminal.font_size, 18.0);
        assert_eq!(
            loaded.terminal.font_family,
            TerminalSettings::default().font_family
        );
        assert_eq!(loaded.behaviour, BehaviourSettings::default());
    }

    #[test]
    fn a_typo_is_rejected_rather_than_ignored() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "version = 1\n[terminal]\nfont_sze = 18.0\n").expect("writes");
        assert!(store::load::<AppSettings>(&path).is_err());
    }

    #[test]
    fn app_defaults_become_the_base_of_the_inheritance_chain() {
        let settings = AppSettings {
            defaults: SettingsOverride {
                keepalive_secs: Some(5),
                ..Default::default()
            },
            ..Default::default()
        };
        let base = settings.session_defaults();
        assert_eq!(base.keepalive_secs, 5);
        // Everything else is still the model's default.
        assert_eq!(base.scrollback, ResolvedSettings::default().scrollback);
    }

    #[test]
    fn no_app_defaults_means_the_models_defaults() {
        assert_eq!(
            AppSettings::default().session_defaults(),
            ResolvedSettings::default()
        );
    }

    #[test]
    fn the_dangerous_conveniences_are_off_by_default() {
        let terminal = TerminalSettings::default();
        // Each of these silently loses or executes something.
        assert!(!terminal.copy_on_select);
        assert!(terminal.warn_on_multiline_paste);
        assert!(!BehaviourSettings::default().scroll_on_output);
    }

    #[test]
    fn middle_click_paste_follows_the_platform_convention() {
        assert_eq!(
            TerminalSettings::default().paste_on_middle_click,
            cfg!(unix)
        );
    }
}
