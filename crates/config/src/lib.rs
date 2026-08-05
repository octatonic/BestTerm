//! Configuration: where BestTerm's files live, how they are versioned, and how they are written.
//!
//! Three files, split by what should follow a user between machines:
//!
//! | File | Directory | Synchronised? |
//! |---|---|---|
//! | `sessions.toml` | config | yes — this is the tree people want in git |
//! | `settings.toml` | config | yes |
//! | `layout.toml` | state | no — a layout from another monitor setup is worse than none |
//!
//! Every file carries a schema `version`, is migrated forward one step at a time with the original
//! kept aside, and is written atomically. The reasoning for each is in [`store`].
//!
//! # Example
//!
//! ```
//! use bestterm_config::{ConfigStore, Paths};
//! use bestterm_core_model::{ProtocolConfig, SessionTree, SshConfig};
//!
//! let dir = tempfile::tempdir()?;
//! let store = ConfigStore::new(Paths::rooted_at(dir.path()));
//!
//! // First run: nothing on disk, so everything comes back as defaults.
//! let mut tree = store.load_tree()?;
//! assert!(tree.is_empty());
//!
//! tree.add_session(
//!     None,
//!     "db",
//!     ProtocolConfig::Ssh(SshConfig {
//!         host: "db.internal".to_string(),
//!         ..Default::default()
//!     }),
//! )?;
//! store.save_tree(&tree)?;
//!
//! let reloaded: SessionTree = store.load_tree()?;
//! assert_eq!(reloaded.len(), 1);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod layout;
pub mod paths;
mod sessions;
pub mod settings;
pub mod store;

pub use layout::{
    LayoutDoc, PaneContent, PaneNode, SidebarLayout, SplitAxis, TabLayout, WindowLayout,
};
pub use paths::Paths;
pub use settings::{AppSettings, BehaviourSettings, BellStyle, CursorStyle, TerminalSettings};
pub use store::{ConfigError, ConfigResult, Document, Migration};

use bestterm_core_model::{NodeId, ResolvedSettings, SessionTree, TreeDoc};

/// Reads and writes BestTerm's configuration.
///
/// A thin facade over [`store`]: it knows which document lives at which path, and nothing else. Held
/// by the application so the rest of the code never handles a path.
#[derive(Clone, Debug)]
pub struct ConfigStore {
    paths: Paths,
}

impl ConfigStore {
    /// Read and write under `paths`.
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    /// The locations in use.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Load the preferences, or their defaults on first run.
    pub fn load_settings(&self) -> ConfigResult<AppSettings> {
        store::load_or_default(&self.paths.settings())
    }

    /// Write the preferences.
    pub fn save_settings(&self, settings: &AppSettings) -> ConfigResult<()> {
        store::save(&self.paths.settings(), settings)
    }

    /// Load the session tree, or an empty one on first run.
    ///
    /// A file that exists but does not describe a tree is reported rather than replaced — see
    /// [`ConfigError::InvalidTree`]. Quietly starting empty would mean the next save destroys it.
    pub fn load_tree(&self) -> ConfigResult<SessionTree> {
        let path = self.paths.sessions();
        let doc: TreeDoc = store::load_or_default(&path)?;
        doc.into_tree()
            .map_err(|source| ConfigError::InvalidTree { path, source })
    }

    /// Write the session tree.
    pub fn save_tree(&self, tree: &SessionTree) -> ConfigResult<()> {
        store::save(&self.paths.sessions(), &TreeDoc::from_tree(tree))
    }

    /// Load the saved layout, already made safe to use.
    ///
    /// `session_exists` is consulted to drop panes pointing at sessions that have since been
    /// deleted; see [`LayoutDoc::sanitise`].
    pub fn load_layout(&self, session_exists: &dyn Fn(NodeId) -> bool) -> ConfigResult<LayoutDoc> {
        let mut doc: LayoutDoc = store::load_or_default(&self.paths.layout())?;
        doc.sanitise(session_exists);
        Ok(doc)
    }

    /// Write the layout.
    pub fn save_layout(&self, layout: &LayoutDoc) -> ConfigResult<()> {
        store::save(&self.paths.layout(), layout)
    }

    /// Settings for a session, resolved through its folders over the application defaults.
    ///
    /// The one place the two halves of the settings system meet: application preferences are the
    /// outermost link in the inheritance chain, folders come next, the session itself last.
    pub fn resolve_session_settings(
        &self,
        settings: &AppSettings,
        tree: &SessionTree,
        id: NodeId,
    ) -> ResolvedSettings {
        tree.resolve_settings_from(settings.session_defaults(), id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bestterm_core_model::{NodeId, ProtocolConfig, SettingsOverride, SshConfig};

    fn store_in(dir: &tempfile::TempDir) -> ConfigStore {
        ConfigStore::new(Paths::rooted_at(dir.path()))
    }

    #[test]
    fn first_run_produces_defaults_without_writing_anything() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = store_in(&dir);

        assert_eq!(store.load_settings().expect("settings"), AppSettings::default());
        assert!(store.load_tree().expect("tree").is_empty());
        assert!(
            store
                .load_layout(&|_| true)
                .expect("layout")
                .tabs
                .is_empty()
        );

        assert!(!store.paths().settings().exists());
        assert!(!store.paths().sessions().exists());
        assert!(!store.paths().layout().exists());
    }

    #[test]
    fn everything_round_trips_through_its_own_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = store_in(&dir);

        let mut settings = AppSettings::default();
        settings.terminal.font_size = 16.0;
        store.save_settings(&settings).expect("saves settings");

        let mut tree = SessionTree::new();
        let session = tree
            .add_session(
                None,
                "db",
                ProtocolConfig::Ssh(SshConfig {
                    host: "db.internal".to_string(),
                    ..Default::default()
                }),
            )
            .expect("session");
        store.save_tree(&tree).expect("saves tree");

        let layout = LayoutDoc {
            tabs: vec![TabLayout {
                title: None,
                root: PaneNode::session(session),
            }],
            ..Default::default()
        };
        store.save_layout(&layout).expect("saves layout");

        assert_eq!(store.load_settings().expect("settings"), settings);
        assert_eq!(store.load_tree().expect("tree").len(), 1);
        assert_eq!(
            store.load_layout(&|id| id == session).expect("layout").tabs,
            layout.tabs
        );
    }

    #[test]
    fn the_layout_lives_outside_the_config_directory() {
        // So that syncing the config directory does not carry a monitor arrangement with it.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = store_in(&dir);
        store
            .save_layout(&LayoutDoc::default())
            .expect("saves layout");

        assert!(store.paths().layout().exists());
        assert_ne!(store.paths().config_dir(), store.paths().state_dir());
        assert!(
            !store.paths().config_dir().join(paths::LAYOUT_FILE).exists(),
            "the layout must not be written into the synchronised directory"
        );
    }

    #[test]
    fn a_layout_pointing_at_a_deleted_session_loads_without_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = store_in(&dir);

        let gone = NodeId::new();
        store
            .save_layout(&LayoutDoc {
                tabs: vec![TabLayout {
                    title: None,
                    root: PaneNode::session(gone),
                }],
                ..Default::default()
            })
            .expect("saves layout");

        let loaded = store.load_layout(&|_| false).expect("loads");
        assert!(loaded.tabs.is_empty());
    }

    #[test]
    fn a_broken_session_file_is_reported_not_silently_emptied() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = store_in(&dir);
        let path = store.paths().sessions();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

        // A node claiming a parent that is not in the file.
        std::fs::write(
            &path,
            format!(
                "version = 1\n\n[[nodes]]\nid = \"{}\"\nparent = \"{}\"\nname = \"orphan\"\n\
                 expanded = true\n",
                NodeId::new(),
                NodeId::new()
            ),
        )
        .expect("writes");

        let error = store.load_tree().expect_err("must fail");
        assert!(
            matches!(error, ConfigError::InvalidTree { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn application_defaults_sit_outermost_in_the_inheritance_chain() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = store_in(&dir);

        let settings = AppSettings {
            defaults: SettingsOverride {
                keepalive_secs: Some(5),
                scrollback: Some(1_000),
                ..Default::default()
            },
            ..Default::default()
        };

        let mut tree = SessionTree::new();
        let folder = tree.add_folder(None, "Production").expect("folder");
        tree.get_mut(folder).expect("node").settings = SettingsOverride {
            keepalive_secs: Some(30),
            ..Default::default()
        };
        let session = tree
            .add_session(
                Some(folder),
                "db",
                ProtocolConfig::Ssh(SshConfig::default()),
            )
            .expect("session");

        let resolved = store.resolve_session_settings(&settings, &tree, session);
        // The folder beats the application default...
        assert_eq!(resolved.keepalive_secs, 30);
        // ...but the application default still supplies what no folder mentions.
        assert_eq!(resolved.scrollback, 1_000);
    }
}
