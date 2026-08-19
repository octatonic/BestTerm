//! Settings that a folder can set and a session can override.
//!
//! Inheritance down the folder tree is the feature that makes a tree of five hundred hosts
//! maintainable: set the keepalive once on `Production`, not five hundred times. The model is the one
//! mRemoteNG uses — a folder supplies defaults, a descendant overrides them, nearest wins.
//!
//! The mechanism is deliberately dull: every field of [`SettingsOverride`] is an `Option`, where
//! `None` means "inherit" and `Some` means "stop here". Resolution walks from the root down, so the
//! last writer is the closest ancestor.

use serde::{Deserialize, Serialize};

/// Settings left unset inherit from the nearest ancestor that sets them.
///
/// `Option` is load-bearing here and must not be replaced with sentinel values: there has to be a
/// difference between "this folder says scrollback is 10 000" and "this folder has an opinion about
/// nothing", or a folder in the middle of the tree would silently reset everything below it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SettingsOverride {
    /// Terminal font family.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    /// Terminal font size in points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    /// Lines of scrollback to keep.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollback: Option<usize>,
    /// Named colour palette.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palette: Option<String>,
    /// Tab tint, as RGB. Imported from `.mxtsessions` where present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_color: Option<[u8; 3]>,
    /// Seconds between keepalives; zero disables them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keepalive_secs: Option<u32>,
    /// Request X11 forwarding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x11_forwarding: Option<bool>,
    /// Forward the ssh-agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_forwarding: Option<bool>,
    /// Write a transcript of the session to disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_session: Option<bool>,
    /// Reconnect automatically after an unexpected disconnect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_reconnect: Option<bool>,
    /// What to tell the remote end this terminal is, as `TERM`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_type: Option<String>,
    /// Where a transcript goes, when `log_session` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    /// Whether Backspace sends `^H` rather than delete.
    ///
    /// Which one is right depends entirely on the far end, which is why it is a setting and not a
    /// decision: the same key has to do different things on different hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backspace_sends_ctrl_h: Option<bool>,
    /// Keep the tab's title as the session's name rather than letting the program change it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_terminal_title: Option<bool>,
    /// Say so in the terminal when a session ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnection_message: Option<bool>,
}

impl SettingsOverride {
    /// Whether this override says nothing at all.
    ///
    /// Used to keep the persisted file clean: a node with no opinions writes no settings table.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Write whatever this override specifies onto `target`.
    ///
    /// Applied root-first during resolution, so the closest ancestor is the last to write and
    /// therefore wins.
    pub fn apply_to(&self, target: &mut ResolvedSettings) {
        if let Some(v) = &self.font_family {
            target.font_family = v.clone();
        }
        if let Some(v) = self.font_size {
            target.font_size = v;
        }
        if let Some(v) = self.scrollback {
            target.scrollback = v;
        }
        if let Some(v) = &self.palette {
            target.palette = v.clone();
        }
        // Tab colour stays optional after resolution: "no tint" is a real, common answer, unlike
        // "no font size".
        if let Some(v) = self.tab_color {
            target.tab_color = Some(v);
        }
        if let Some(v) = self.keepalive_secs {
            target.keepalive_secs = v;
        }
        if let Some(v) = self.x11_forwarding {
            target.x11_forwarding = v;
        }
        if let Some(v) = self.agent_forwarding {
            target.agent_forwarding = v;
        }
        if let Some(v) = self.log_session {
            target.log_session = v;
        }
        if let Some(v) = self.auto_reconnect {
            target.auto_reconnect = v;
        }
    }
}

/// Settings with every question answered.
///
/// What the connection layer is handed. There are no `Option`s left except where absence is itself
/// meaningful.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResolvedSettings {
    /// Terminal font family.
    pub font_family: String,
    /// Terminal font size in points.
    pub font_size: f32,
    /// Lines of scrollback to keep.
    pub scrollback: usize,
    /// Named colour palette.
    pub palette: String,
    /// Tab tint, or `None` for the default colour.
    pub tab_color: Option<[u8; 3]>,
    /// Seconds between keepalives; zero disables them.
    pub keepalive_secs: u32,
    /// Request X11 forwarding.
    pub x11_forwarding: bool,
    /// Forward the ssh-agent.
    pub agent_forwarding: bool,
    /// Write a transcript of the session to disk.
    pub log_session: bool,
    /// Reconnect automatically after an unexpected disconnect.
    pub auto_reconnect: bool,
}

impl Default for ResolvedSettings {
    fn default() -> Self {
        Self {
            font_family: "monospace".to_string(),
            font_size: 14.0,
            // alacritty_terminal's own default, and a sane compromise.
            scrollback: 10_000,
            palette: "xterm".to_string(),
            tab_color: None,
            // 60s keeps NAT and idle-timeout middleboxes from dropping a quiet session.
            keepalive_secs: 60,
            // Both forwardings are off by default: each hands the remote host a capability on the
            // local machine, and that should be a decision, not a default.
            x11_forwarding: false,
            agent_forwarding: false,
            log_session: false,
            auto_reconnect: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_override_changes_nothing() {
        let mut resolved = ResolvedSettings::default();
        let before = resolved.clone();
        SettingsOverride::default().apply_to(&mut resolved);
        assert_eq!(resolved, before);
    }

    #[test]
    fn is_empty_detects_the_do_nothing_override() {
        assert!(SettingsOverride::default().is_empty());
        assert!(
            !SettingsOverride {
                font_size: Some(12.0),
                ..Default::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn a_set_field_wins_over_the_default() {
        let mut resolved = ResolvedSettings::default();
        SettingsOverride {
            scrollback: Some(50_000),
            x11_forwarding: Some(true),
            ..Default::default()
        }
        .apply_to(&mut resolved);
        assert_eq!(resolved.scrollback, 50_000);
        assert!(resolved.x11_forwarding);
        // Untouched fields keep their defaults.
        assert_eq!(resolved.font_size, ResolvedSettings::default().font_size);
    }

    #[test]
    fn the_last_override_applied_wins() {
        // Resolution applies root-first, so "last" means "closest ancestor".
        let mut resolved = ResolvedSettings::default();
        SettingsOverride {
            scrollback: Some(1_000),
            ..Default::default()
        }
        .apply_to(&mut resolved);
        SettingsOverride {
            scrollback: Some(2_000),
            ..Default::default()
        }
        .apply_to(&mut resolved);
        assert_eq!(resolved.scrollback, 2_000);
    }

    #[test]
    fn a_later_none_does_not_erase_an_earlier_some() {
        // The whole point of Option-as-inherit: a folder with no opinion must not reset its parent's.
        let mut resolved = ResolvedSettings::default();
        SettingsOverride {
            keepalive_secs: Some(15),
            ..Default::default()
        }
        .apply_to(&mut resolved);
        SettingsOverride::default().apply_to(&mut resolved);
        assert_eq!(resolved.keepalive_secs, 15);
    }

    #[test]
    fn forwardings_are_off_by_default() {
        // Each hands the remote host a capability on this machine; that must be opt-in.
        let d = ResolvedSettings::default();
        assert!(!d.x11_forwarding);
        assert!(!d.agent_forwarding);
    }

    #[test]
    fn an_empty_override_serialises_to_nothing() {
        let text = toml::to_string(&SettingsOverride::default()).expect("serialises");
        assert_eq!(
            text.trim(),
            "",
            "a node with no opinions must not clutter the file, got:\n{text}"
        );
    }

    #[test]
    fn override_round_trips_and_keeps_unset_fields_unset() {
        let original = SettingsOverride {
            font_size: Some(13.5),
            tab_color: Some([255, 0, 0]),
            ..Default::default()
        };
        let text = toml::to_string(&original).expect("serialises");
        let back: SettingsOverride = toml::from_str(&text).expect("deserialises");
        assert_eq!(back, original);
        assert_eq!(back.scrollback, None);
    }

    #[test]
    fn an_unknown_settings_key_is_rejected() {
        assert!(toml::from_str::<SettingsOverride>("scrolback = 100").is_err());
    }
}
