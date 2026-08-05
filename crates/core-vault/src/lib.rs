//! The credential vault.
//!
//! # Design
//!
//! Envelope encryption, with two keys rather than one:
//!
//! ```text
//! master password ──Argon2id(salt)──► KEK ──seals──► DEK ──seals──► every entry
//!                                      │              │
//!                          stored: salt + costs   stored: wrapped blob
//! ```
//!
//! The data-encryption key is random and never leaves the vault; the key-encryption key is derived
//! from the master password and only ever unwraps the DEK. Three things fall out of that, and none of
//! them work with a single derived key:
//!
//! * **Changing the master password is instant.** A new salt, a new KEK, the same DEK rewrapped.
//!   Entries are untouched, so the file's diff is two lines rather than every secret it holds.
//! * **The operating system's keystore can hold the DEK** for an unlock that does not ask for a
//!   password, without the keystore ever holding the password itself.
//! * **Raising the Argon2 cost later is safe.** Parameters live in the file, so an old vault keeps
//!   opening with its own until it is rewritten.
//!
//! Every entry is sealed with its own random nonce and authenticated against **its own name**. That
//! last part matters: without it, someone with write access to the file could move the ciphertext of
//! `staging/password` onto `production/password` and every check would still pass.
//!
//! # What this crate does not do
//!
//! No file I/O — like `bestterm-core-model`, it defines the shape and the rules, and
//! `bestterm-config` owns the bytes. And no operating-system keystore implementation yet: the
//! [`KeyStore`] seam is here and tested against an in-memory store, but the platform backend is
//! deliberately still absent. See the note on [`KeyStore`].
//!
//! # Example
//!
//! ```
//! use bestterm_core_vault::{Secret, Vault};
//!
//! let master = Secret::new("correct horse battery staple");
//! let mut vault = Vault::create(&master)?;
//! vault.set("prod/db/password", &Secret::new("hunter2"))?;
//!
//! // Persisting and reopening.
//! let file = vault.to_file();
//! let reopened = Vault::unlock(file, &master)?;
//! assert_eq!(
//!     reopened.get("prod/db/password")?.map(|s| s.expose().to_string()),
//!     Some("hunter2".to_string())
//! );
//! # Ok::<(), bestterm_core_vault::VaultError>(())
//! ```

mod base64_bytes;
mod crypto;
mod error;
mod secret;

pub use base64_bytes::Base64Bytes;
pub use crypto::{KdfAlgorithm, KdfParams, SealedBlob};
pub use error::{VaultError, VaultResult};
pub use secret::{DataKey, Secret};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crypto::{derive_kek, open, seal};

/// Associated data binding the wrapped key to its purpose.
const KEY_AAD: &[u8] = b"bestterm:vault:data-key";

/// Associated data for the verifier.
const VERIFIER_AAD: &[u8] = b"bestterm:vault:verifier";

/// Constant sealed under the data key so a key from elsewhere can be checked before use.
const VERIFIER_PLAINTEXT: &[u8] = b"bestterm-vault-v1";

/// Prefix reserved for the vault's own bookkeeping.
const RESERVED_PREFIX: &str = "__bestterm";

/// The stored form of a vault.
///
/// Contains nothing usable without the master password or the data key. Safe to keep in version
/// control, which is the point.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultFile {
    /// How the key-encryption key is derived.
    pub kdf: KdfParams,
    /// The data key, sealed under the key-encryption key.
    pub wrapped_key: SealedBlob,
    /// A known constant sealed under the data key, so a key can be verified before use.
    pub verifier: SealedBlob,
    /// Sealed entries, by name.
    ///
    /// A `BTreeMap` so the file is written in a stable order and a diff shows only what changed.
    #[serde(default)]
    pub entries: BTreeMap<String, SealedBlob>,
}

/// Somewhere the operating system can hold the data key.
///
/// # Status
///
/// The seam exists and is exercised by [`MemoryKeyStore`]; **no platform backend is implemented
/// yet**, so unlocking always asks for the master password today.
///
/// It is a trait rather than a direct dependency on the `keyring` crate deliberately. That crate's
/// own documentation says applications wanting control over which credential stores they use should
/// link to `keyring-core` and specific store crates instead of to the facade — and its 4.x line has
/// just been restructured around exactly that. Putting the boundary here means the backend can be
/// chosen, and replaced, without the vault noticing.
pub trait KeyStore {
    /// Remember the data key for `vault_id`.
    fn store(&self, vault_id: &str, key: &DataKey) -> VaultResult<()>;

    /// Recall the data key for `vault_id`, if one is held.
    fn load(&self, vault_id: &str) -> VaultResult<Option<DataKey>>;

    /// Forget the data key for `vault_id`. Succeeds when there was nothing to forget.
    fn clear(&self, vault_id: &str) -> VaultResult<()>;
}

/// A keystore that holds nothing.
///
/// The default, and what a user who wants to type their password every time gets.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoKeyStore;

impl KeyStore for NoKeyStore {
    fn store(&self, _vault_id: &str, _key: &DataKey) -> VaultResult<()> {
        Ok(())
    }

    fn load(&self, _vault_id: &str) -> VaultResult<Option<DataKey>> {
        Ok(None)
    }

    fn clear(&self, _vault_id: &str) -> VaultResult<()> {
        Ok(())
    }
}

/// A keystore held in memory, for tests.
///
/// Explicitly not a security feature: the key lives in the process and is gone when it exits.
#[derive(Debug, Default)]
pub struct MemoryKeyStore {
    keys: std::sync::Mutex<BTreeMap<String, DataKey>>,
}

impl KeyStore for MemoryKeyStore {
    fn store(&self, vault_id: &str, key: &DataKey) -> VaultResult<()> {
        let mut keys = self.keys.lock().map_err(|_| VaultError::Encryption)?;
        keys.insert(vault_id.to_string(), key.clone());
        Ok(())
    }

    fn load(&self, vault_id: &str) -> VaultResult<Option<DataKey>> {
        let keys = self.keys.lock().map_err(|_| VaultError::Encryption)?;
        Ok(keys.get(vault_id).cloned())
    }

    fn clear(&self, vault_id: &str) -> VaultResult<()> {
        let mut keys = self.keys.lock().map_err(|_| VaultError::Encryption)?;
        keys.remove(vault_id);
        Ok(())
    }
}

/// An unlocked vault.
///
/// Holding one means holding the data key, so it is only ever produced by [`Vault::create`],
/// [`Vault::unlock`] or [`Vault::unlock_with_key`].
#[derive(Debug)]
pub struct Vault {
    kdf: KdfParams,
    wrapped_key: SealedBlob,
    verifier: SealedBlob,
    entries: BTreeMap<String, SealedBlob>,
    data_key: DataKey,
}

impl Vault {
    /// Create a new, empty vault protected by `master`.
    pub fn create(master: &Secret) -> VaultResult<Self> {
        let kdf = KdfParams::generate()?;
        let kek = derive_kek(master.expose(), &kdf)?;
        let data_key = DataKey::generate().map_err(|_| VaultError::Random)?;

        let wrapped_key = seal(&kek, KEY_AAD, data_key.expose())?;
        let verifier = seal(&data_key, VERIFIER_AAD, VERIFIER_PLAINTEXT)?;

        Ok(Self {
            kdf,
            wrapped_key,
            verifier,
            entries: BTreeMap::new(),
            data_key,
        })
    }

    /// Open a stored vault with the master password.
    pub fn unlock(file: VaultFile, master: &Secret) -> VaultResult<Self> {
        let kek = derive_kek(master.expose(), &file.kdf)?;

        // A failure here is overwhelmingly a mistyped password, so say so rather than reporting a
        // generic authentication failure the user cannot act on.
        let key_bytes = open(&kek, KEY_AAD, &file.wrapped_key).map_err(|error| match error {
            VaultError::Authentication => VaultError::WrongPassword,
            other => other,
        })?;

        let data_key = data_key_from(&key_bytes)?;
        Self::assemble(file, data_key)
    }

    /// Open a stored vault with a data key from a [`KeyStore`].
    ///
    /// The key is verified before use, so a key left behind after the master password was changed
    /// elsewhere is reported as [`VaultError::StaleStoredKey`] rather than producing a vault whose
    /// every read fails.
    pub fn unlock_with_key(file: VaultFile, data_key: DataKey) -> VaultResult<Self> {
        match open(&data_key, VERIFIER_AAD, &file.verifier) {
            Ok(plaintext) if plaintext == VERIFIER_PLAINTEXT => {}
            Ok(_) => return Err(VaultError::MalformedVault("verifier does not match")),
            Err(VaultError::Authentication) => return Err(VaultError::StaleStoredKey),
            Err(other) => return Err(other),
        }
        Self::assemble(file, data_key)
    }

    fn assemble(file: VaultFile, data_key: DataKey) -> VaultResult<Self> {
        Ok(Self {
            kdf: file.kdf,
            wrapped_key: file.wrapped_key,
            verifier: file.verifier,
            entries: file.entries,
            data_key,
        })
    }

    /// The stored form, ready to be written out.
    pub fn to_file(&self) -> VaultFile {
        VaultFile {
            kdf: self.kdf.clone(),
            wrapped_key: self.wrapped_key.clone(),
            verifier: self.verifier.clone(),
            entries: self.entries.clone(),
        }
    }

    /// The data key, for handing to a [`KeyStore`].
    pub fn data_key(&self) -> &DataKey {
        &self.data_key
    }

    /// Store or replace a secret.
    pub fn set(&mut self, name: &str, secret: &Secret) -> VaultResult<()> {
        check_name(name)?;
        let blob = seal(&self.data_key, &entry_aad(name), secret.expose_bytes())?;
        self.entries.insert(name.to_string(), blob);
        Ok(())
    }

    /// Read a secret, or `None` if there is no such entry.
    pub fn get(&self, name: &str) -> VaultResult<Option<Secret>> {
        let Some(blob) = self.entries.get(name) else {
            return Ok(None);
        };
        let plaintext = open(&self.data_key, &entry_aad(name), blob)?;
        let text = String::from_utf8(plaintext)
            .map_err(|_| VaultError::MalformedVault("entry is not valid UTF-8"))?;
        Ok(Some(Secret::new(text)))
    }

    /// Remove an entry. Returns whether there was one.
    pub fn remove(&mut self, name: &str) -> bool {
        self.entries.remove(name).is_some()
    }

    /// Whether an entry exists, without decrypting it.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Entry names, in order.
    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// How many entries the vault holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the vault holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Change the master password.
    ///
    /// Rewraps the same data key under a key derived from the new password and a fresh salt. Entries
    /// are not re-encrypted, so this costs one Argon2 derivation regardless of how many secrets the
    /// vault holds.
    pub fn change_master_password(&mut self, new_master: &Secret) -> VaultResult<()> {
        let kdf = KdfParams::generate()?;
        let kek = derive_kek(new_master.expose(), &kdf)?;
        let wrapped_key = seal(&kek, KEY_AAD, self.data_key.expose())?;

        // Assigned only once both succeeded, so a failure leaves the vault openable with the old
        // password rather than with neither.
        self.kdf = kdf;
        self.wrapped_key = wrapped_key;
        Ok(())
    }
}

fn data_key_from(bytes: &[u8]) -> VaultResult<DataKey> {
    let array: [u8; DataKey::LEN] = bytes
        .try_into()
        .map_err(|_| VaultError::MalformedVault("data key is the wrong length"))?;
    Ok(DataKey::from_bytes(array))
}

fn entry_aad(name: &str) -> Vec<u8> {
    let mut aad = b"bestterm:vault:entry:".to_vec();
    aad.extend_from_slice(name.as_bytes());
    aad
}

fn check_name(name: &str) -> VaultResult<()> {
    if name.is_empty() || name.starts_with(RESERVED_PREFIX) {
        return Err(VaultError::ReservedName(name.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master() -> Secret {
        Secret::new("correct horse battery staple")
    }

    fn vault_with_entries() -> Vault {
        let mut vault = Vault::create(&master()).expect("creates");
        vault
            .set("prod/db/password", &Secret::new("hunter2"))
            .expect("sets");
        vault
            .set("staging/db/password", &Secret::new("letmein"))
            .expect("sets");
        vault
    }

    #[test]
    fn a_new_vault_is_empty_but_usable() {
        let vault = Vault::create(&master()).expect("creates");
        assert!(vault.is_empty());
        assert_eq!(vault.len(), 0);
        assert!(vault.names().is_empty());
    }

    #[test]
    fn a_secret_comes_back_out() {
        let vault = vault_with_entries();
        let secret = vault.get("prod/db/password").expect("gets").expect("exists");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn a_missing_entry_is_none_not_an_error() {
        let vault = vault_with_entries();
        assert!(vault.get("nothing/here").expect("gets").is_none());
        assert!(!vault.contains("nothing/here"));
    }

    #[test]
    fn setting_twice_replaces() {
        let mut vault = vault_with_entries();
        vault
            .set("prod/db/password", &Secret::new("rotated"))
            .expect("sets");
        assert_eq!(vault.len(), 2);
        let secret = vault.get("prod/db/password").expect("gets").expect("exists");
        assert_eq!(secret.expose(), "rotated");
    }

    #[test]
    fn removing_reports_whether_there_was_anything() {
        let mut vault = vault_with_entries();
        assert!(vault.remove("prod/db/password"));
        assert!(!vault.remove("prod/db/password"));
        assert_eq!(vault.len(), 1);
    }

    #[test]
    fn names_are_sorted_so_the_file_diffs_cleanly() {
        let vault = vault_with_entries();
        assert_eq!(vault.names(), vec!["prod/db/password", "staging/db/password"]);
    }

    #[test]
    fn reserved_and_empty_names_are_refused() {
        let mut vault = Vault::create(&master()).expect("creates");
        assert!(matches!(
            vault.set("", &Secret::new("x")),
            Err(VaultError::ReservedName(_))
        ));
        assert!(matches!(
            vault.set("__bestterm/sneaky", &Secret::new("x")),
            Err(VaultError::ReservedName(_))
        ));
    }

    #[test]
    fn a_vault_survives_a_round_trip_through_the_file_and_the_password() {
        let original = vault_with_entries();
        let file = original.to_file();

        let reopened = Vault::unlock(file, &master()).expect("unlocks");
        assert_eq!(reopened.names(), original.names());
        for name in reopened.names() {
            assert_eq!(
                reopened.get(name).expect("gets").map(|s| s.expose().to_string()),
                original.get(name).expect("gets").map(|s| s.expose().to_string()),
            );
        }
    }

    #[test]
    fn a_wrong_password_says_so() {
        let file = vault_with_entries().to_file();
        let error = Vault::unlock(file, &Secret::new("wrong")).expect_err("must fail");
        assert_eq!(error, VaultError::WrongPassword);
    }

    #[test]
    fn the_stored_file_reveals_neither_secrets_nor_password() {
        let vault = vault_with_entries();
        let text = toml::to_string(&vault.to_file()).expect("serialises");

        assert!(!text.contains("hunter2"), "got:\n{text}");
        assert!(!text.contains("letmein"), "got:\n{text}");
        assert!(!text.contains("correct horse"), "got:\n{text}");
        // Names are visible on purpose: the file has to be reviewable, and a name is not a secret.
        assert!(text.contains("prod/db/password"), "got:\n{text}");
    }

    #[test]
    fn the_file_round_trips_through_toml() {
        let file = vault_with_entries().to_file();
        let text = toml::to_string(&file).expect("serialises");
        let parsed: VaultFile = toml::from_str(&text).expect("parses");
        assert_eq!(parsed, file);
        // And still opens.
        assert_eq!(Vault::unlock(parsed, &master()).expect("unlocks").len(), 2);
    }

    #[test]
    fn changing_the_password_leaves_the_entries_alone() {
        // The point of the envelope: entries are not re-encrypted, so their ciphertext is untouched.
        let mut vault = vault_with_entries();
        let before = vault.to_file();

        let new_master = Secret::new("a whole new passphrase");
        vault.change_master_password(&new_master).expect("changes");
        let after = vault.to_file();

        assert_eq!(after.entries, before.entries, "entries must not be rewritten");
        assert_ne!(after.wrapped_key, before.wrapped_key);
        assert_ne!(after.kdf.salt, before.kdf.salt, "a new salt each time");

        // Old password no longer opens it, new one does.
        assert_eq!(
            Vault::unlock(after.clone(), &master()).expect_err("must fail"),
            VaultError::WrongPassword
        );
        let reopened = Vault::unlock(after, &new_master).expect("unlocks");
        let secret = reopened
            .get("prod/db/password")
            .expect("gets")
            .expect("exists");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn a_key_from_a_keystore_opens_the_vault_without_the_password() {
        let store = MemoryKeyStore::default();
        let vault = vault_with_entries();
        store.store("default", vault.data_key()).expect("stores");
        let file = vault.to_file();

        let key = store.load("default").expect("loads").expect("held");
        let reopened = Vault::unlock_with_key(file, key).expect("unlocks");
        let secret = reopened
            .get("prod/db/password")
            .expect("gets")
            .expect("exists");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn a_stored_key_from_before_a_password_change_is_reported_as_stale() {
        // The password change keeps the same data key, so to get a genuinely stale key we need one
        // from a different vault — which is exactly what a keystore entry left behind by a
        // re-created vault looks like.
        let other = Vault::create(&Secret::new("another vault")).expect("creates");
        let file = vault_with_entries().to_file();

        let error = Vault::unlock_with_key(file, other.data_key().clone()).expect_err("must fail");
        assert_eq!(error, VaultError::StaleStoredKey);
    }

    #[test]
    fn a_password_change_does_not_invalidate_the_stored_key() {
        // Because the data key survives, the fast path keeps working after a password change.
        let mut vault = vault_with_entries();
        let key = vault.data_key().clone();
        vault
            .change_master_password(&Secret::new("new one"))
            .expect("changes");

        let reopened = Vault::unlock_with_key(vault.to_file(), key).expect("unlocks");
        assert_eq!(reopened.len(), 2);
    }

    #[test]
    fn the_no_op_keystore_holds_nothing() {
        let store = NoKeyStore;
        let vault = Vault::create(&master()).expect("creates");
        store.store("default", vault.data_key()).expect("stores");
        assert!(store.load("default").expect("loads").is_none());
        store.clear("default").expect("clears");
    }

    #[test]
    fn a_keystore_forgets_when_cleared() {
        let store = MemoryKeyStore::default();
        let vault = Vault::create(&master()).expect("creates");
        store.store("default", vault.data_key()).expect("stores");
        assert!(store.load("default").expect("loads").is_some());
        store.clear("default").expect("clears");
        assert!(store.load("default").expect("loads").is_none());
        // Clearing again is not an error.
        store.clear("default").expect("clears");
    }

    #[test]
    fn moving_an_entry_to_another_name_in_the_file_is_detected() {
        // Someone with write access to the vault must not be able to promote the staging password to
        // production by editing the file.
        let vault = vault_with_entries();
        let mut file = vault.to_file();
        let staging = file.entries["staging/db/password"].clone();
        file.entries.insert("prod/db/password".to_string(), staging);

        let tampered = Vault::unlock(file, &master()).expect("unlocks");
        assert_eq!(
            tampered.get("prod/db/password").expect_err("must fail"),
            VaultError::Authentication
        );
    }

    #[test]
    fn a_tampered_verifier_is_reported_rather_than_ignored() {
        let vault = vault_with_entries();
        let mut file = vault.to_file();
        let mut bytes = file.verifier.ciphertext.as_slice().to_vec();
        bytes[0] ^= 0xFF;
        file.verifier.ciphertext = Base64Bytes::new(bytes);

        let key = vault.data_key().clone();
        assert_eq!(
            Vault::unlock_with_key(file, key).expect_err("must fail"),
            VaultError::StaleStoredKey
        );
    }

    #[test]
    fn two_vaults_with_the_same_password_do_not_share_a_key() {
        let first = Vault::create(&master()).expect("creates");
        let second = Vault::create(&master()).expect("creates");
        assert_ne!(first.data_key(), second.data_key());
        assert_ne!(first.to_file().kdf.salt, second.to_file().kdf.salt);
    }

    #[test]
    fn an_entry_holding_invalid_utf8_is_reported_as_malformed() {
        let mut vault = Vault::create(&master()).expect("creates");
        // Reach past `set`, which only accepts a `Secret`, to seal raw bytes.
        let blob = crypto::seal(vault.data_key(), &entry_aad("raw"), &[0xFF, 0xFE]).expect("seals");
        vault.entries.insert("raw".to_string(), blob);

        assert!(matches!(
            vault.get("raw"),
            Err(VaultError::MalformedVault(_))
        ));
    }
}
