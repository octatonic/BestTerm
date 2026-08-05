//! Where BestTerm keeps its files.
//!
//! Two directories, deliberately separate:
//!
//! * **Config** holds the session tree and the preferences — the things a user would want in version
//!   control and synchronised between machines.
//! * **State** holds the saved window layout and other per-machine residue, which should *not*
//!   follow them to another machine. A layout restored from a different monitor arrangement is worse
//!   than no layout at all.
//!
//! On Linux these land in `~/.config/bestterm` and `~/.local/state/bestterm`; on Windows both fall
//! under `%APPDATA%`, since Windows draws no equivalent distinction.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

/// Filename of the session tree.
pub const SESSIONS_FILE: &str = "sessions.toml";

/// Filename of the preferences.
pub const SETTINGS_FILE: &str = "settings.toml";

/// Filename of the saved window layout.
pub const LAYOUT_FILE: &str = "layout.toml";

/// Filename of the credential vault.
///
/// Sits in the config directory with the session tree, not in state: the vault is encrypted, so it is
/// meant to travel with the sessions it belongs to. Everything in it is useless without the master
/// password.
pub const VAULT_FILE: &str = "vault.toml";

/// Resolved locations for this installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    config_dir: PathBuf,
    state_dir: PathBuf,
}

impl Paths {
    /// The platform's locations for BestTerm.
    ///
    /// Returns `None` when the platform will not say where the home directory is, which in practice
    /// means a service account with no profile. Callers should fall back to
    /// [`Paths::rooted_at`] with an explicit directory rather than guessing.
    pub fn discover() -> Option<Self> {
        let dirs = ProjectDirs::from("", "", "bestterm")?;
        Some(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            // `data_local_dir` is the closest thing to XDG_STATE_HOME that `directories` exposes on
            // every platform, and it has the property that matters: not synchronised.
            state_dir: dirs.data_local_dir().to_path_buf(),
        })
    }

    /// Put everything under one directory.
    ///
    /// Used by `--config-dir`, by portable installations, and by tests, which must never touch the
    /// developer's real configuration.
    pub fn rooted_at(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            config_dir: root.to_path_buf(),
            state_dir: root.join("state"),
        }
    }

    /// Directory holding the session tree and preferences.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Directory holding per-machine state.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Path of the session tree.
    pub fn sessions(&self) -> PathBuf {
        self.config_dir.join(SESSIONS_FILE)
    }

    /// Path of the preferences.
    pub fn settings(&self) -> PathBuf {
        self.config_dir.join(SETTINGS_FILE)
    }

    /// Path of the saved layout.
    pub fn layout(&self) -> PathBuf {
        self.state_dir.join(LAYOUT_FILE)
    }

    /// Path of the credential vault.
    pub fn vault(&self) -> PathBuf {
        self.config_dir.join(VAULT_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_paths_sit_under_the_given_directory() {
        let paths = Paths::rooted_at("/tmp/bt");
        assert_eq!(paths.sessions(), Path::new("/tmp/bt").join(SESSIONS_FILE));
        assert_eq!(paths.settings(), Path::new("/tmp/bt").join(SETTINGS_FILE));
        assert_eq!(
            paths.layout(),
            Path::new("/tmp/bt").join("state").join(LAYOUT_FILE)
        );
    }

    #[test]
    fn config_and_state_are_not_the_same_directory() {
        // Layout must not follow a user to a machine with a different monitor arrangement.
        let paths = Paths::rooted_at("/tmp/bt");
        assert_ne!(paths.config_dir(), paths.state_dir());
    }

    #[test]
    fn discovery_produces_distinct_directories_when_it_works() {
        // On a CI runner with a home directory this succeeds; where it does not, there is nothing to
        // assert and the caller is expected to supply a root explicitly.
        if let Some(paths) = Paths::discover() {
            assert_ne!(paths.config_dir(), paths.state_dir());
            assert!(paths.sessions().ends_with(SESSIONS_FILE));
            assert!(paths.layout().ends_with(LAYOUT_FILE));
        }
    }

    #[test]
    fn filenames_are_distinct() {
        let names = [SESSIONS_FILE, SETTINGS_FILE, LAYOUT_FILE];
        let mut sorted = names.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }
}
