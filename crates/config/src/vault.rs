//! The credential vault as a persisted document.
//!
//! Same arrangement as the session tree: `bestterm-core-vault` owns the cryptography and knows
//! nothing about files, this crate owns the bytes. It gets the same versioning, migration and
//! atomic-write guarantees as every other document — which matters more here than anywhere else,
//! because a half-written vault is a lost vault.

use bestterm_core_vault::VaultFile;

use crate::store::Document;

impl Document for VaultFile {
    const VERSION: u32 = 1;
    const NAME: &'static str = "vault";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;
    use bestterm_core_vault::{Secret, Vault};

    fn master() -> Secret {
        Secret::new("correct horse battery staple")
    }

    #[test]
    fn a_vault_survives_the_round_trip_through_a_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("vault.toml");

        let mut vault = Vault::create(&master()).expect("creates");
        vault
            .set("prod/db/password", &Secret::new("hunter2"))
            .expect("sets");
        store::save(&path, &vault.to_file()).expect("saves");

        let file: VaultFile = store::load(&path).expect("loads");
        let reopened = Vault::unlock(file, &master()).expect("unlocks");
        let secret = reopened
            .get("prod/db/password")
            .expect("gets")
            .expect("exists");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn the_file_on_disk_holds_no_plaintext() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("vault.toml");

        let mut vault = Vault::create(&master()).expect("creates");
        vault
            .set("prod/db/password", &Secret::new("hunter2"))
            .expect("sets");
        store::save(&path, &vault.to_file()).expect("saves");

        let text = std::fs::read_to_string(&path).expect("reads");
        assert!(!text.contains("hunter2"), "got:\n{text}");
        assert!(!text.contains("correct horse"), "got:\n{text}");
        assert!(text.contains("version = 1"), "got:\n{text}");
    }

    #[test]
    fn a_vault_written_by_a_newer_build_is_left_alone() {
        // Worth pinning for the vault specifically: overwriting it would destroy every credential.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("vault.toml");

        let vault = Vault::create(&master()).expect("creates");
        store::save(&path, &vault.to_file()).expect("saves");

        let text = std::fs::read_to_string(&path).expect("reads");
        let bumped = text.replace("version = 1", "version = 99");
        std::fs::write(&path, &bumped).expect("writes");

        let error = store::load::<VaultFile>(&path).expect_err("must fail");
        assert!(
            matches!(error, crate::ConfigError::FromTheFuture { .. }),
            "got {error:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("reads"), bumped);
    }
}
