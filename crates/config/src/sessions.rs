//! The session tree as a persisted document.
//!
//! `TreeDoc` comes from `bestterm-core-model`, which knows nothing about files. Making it a
//! [`Document`] here is what keeps the model free of I/O while still letting it be the thing that is
//! stored — the model owns the shape and its validation, this crate owns the bytes.

use bestterm_core_model::TreeDoc;

use crate::store::Document;

impl Document for TreeDoc {
    const VERSION: u32 = 1;
    const NAME: &'static str = "sessions";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;
    use bestterm_core_model::{ProtocolConfig, SessionTree, SettingsOverride, SshConfig};

    fn sample() -> SessionTree {
        let mut tree = SessionTree::new();
        let prod = tree.add_folder(None, "Production").expect("folder");
        tree.get_mut(prod).expect("node").settings = SettingsOverride {
            keepalive_secs: Some(30),
            ..Default::default()
        };
        tree.add_session(
            Some(prod),
            "mongo-1",
            ProtocolConfig::Ssh(SshConfig {
                host: "mongo-1.int".to_string(),
                user: Some("ops".to_string()),
                ..Default::default()
            }),
        )
        .expect("session");
        tree
    }

    #[test]
    fn a_tree_survives_the_whole_round_trip_through_a_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sessions.toml");

        let original = sample();
        store::save(&path, &TreeDoc::from_tree(&original)).expect("saves");

        let doc: TreeDoc = store::load(&path).expect("loads");
        let rebuilt = doc.into_tree().expect("valid");

        assert_eq!(rebuilt.walk(), original.walk());
        for id in original.walk() {
            assert_eq!(rebuilt.get(id), original.get(id));
            assert_eq!(
                rebuilt.resolve_settings(id),
                original.resolve_settings(id),
                "inherited settings must survive the file"
            );
        }
    }

    #[test]
    fn the_stored_file_is_readable_and_carries_no_secrets() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sessions.toml");
        store::save(&path, &TreeDoc::from_tree(&sample())).expect("saves");

        let text = std::fs::read_to_string(&path).expect("reads");
        // The point of keeping credentials in the vault: the tree is safe to read, diff and sync.
        assert!(text.contains("mongo-1.int"), "got:\n{text}");
        assert!(text.contains("version = 1"), "got:\n{text}");
        assert!(!text.to_lowercase().contains("password"), "got:\n{text}");
    }

    #[test]
    fn an_absent_file_yields_an_empty_tree() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nothing-here.toml");
        let doc: TreeDoc = store::load_or_default(&path).expect("defaults");
        assert!(doc.into_tree().expect("valid").is_empty());
    }
}
