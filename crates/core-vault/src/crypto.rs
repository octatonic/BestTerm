//! The primitives: key derivation, and authenticated encryption of one value.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

use crate::base64_bytes::Base64Bytes;
use crate::error::{VaultError, VaultResult};
use crate::secret::DataKey;

/// Nonce length for XChaCha20-Poly1305.
pub(crate) const NONCE_LEN: usize = 24;

/// Salt length for the key derivation.
///
/// 16 bytes is the size recommended by the Argon2 RFC and is comfortably beyond any risk of
/// collision between vaults.
pub(crate) const SALT_LEN: usize = 16;

/// Which key-derivation function protects the vault.
///
/// An enum with one variant on purpose: it puts the algorithm in the file, so a future change can be
/// migrated instead of guessed at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KdfAlgorithm {
    /// Argon2id, version 0x13.
    ///
    /// Argon2**id** rather than 2i or 2d: the hybrid is what the RFC recommends for password
    /// hashing, resisting both side-channel and time-memory-tradeoff attacks.
    #[default]
    Argon2id,
}

/// Parameters the master key was derived with.
///
/// Stored in the file rather than assumed, so raising the cost later does not lock anyone out of an
/// existing vault: an old file keeps working with its own parameters until it is rewritten.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KdfParams {
    /// Algorithm.
    #[serde(default)]
    pub algorithm: KdfAlgorithm,
    /// Per-vault random salt.
    pub salt: Base64Bytes,
    /// Memory cost in KiB.
    pub m_cost: u32,
    /// Iterations.
    pub t_cost: u32,
    /// Degree of parallelism.
    pub p_cost: u32,
}

impl KdfParams {
    /// Fresh parameters with a new random salt and the recommended costs.
    ///
    /// The costs are `argon2`'s own defaults, which follow the OWASP recommendation: 19 MiB, two
    /// iterations, one lane. Enough to make an offline guessing attack expensive without making an
    /// unlock feel slow.
    pub fn generate() -> VaultResult<Self> {
        let mut salt = vec![0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|_| VaultError::Random)?;
        Ok(Self {
            algorithm: KdfAlgorithm::Argon2id,
            salt: Base64Bytes::new(salt),
            m_cost: Params::DEFAULT_M_COST,
            t_cost: Params::DEFAULT_T_COST,
            p_cost: Params::DEFAULT_P_COST,
        })
    }
}

/// Derive the key-encryption key from the master password.
pub(crate) fn derive_kek(password: &str, params: &KdfParams) -> VaultResult<DataKey> {
    if params.salt.as_slice().len() < 8 {
        // Argon2 itself requires 8; a file claiming less has been tampered with or truncated.
        return Err(VaultError::MalformedVault("salt is too short"));
    }

    let argon_params = Params::new(
        params.m_cost,
        params.t_cost,
        params.p_cost,
        Some(DataKey::LEN),
    )
    .map_err(|_| VaultError::MalformedVault("key derivation parameters are out of range"))?;

    let argon = match params.algorithm {
        KdfAlgorithm::Argon2id => Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params),
    };

    let mut key = [0u8; DataKey::LEN];
    argon
        .hash_password_into(password.as_bytes(), params.salt.as_slice(), &mut key)
        .map_err(|_| VaultError::MalformedVault("key derivation failed"))?;

    Ok(DataKey::from_bytes(key))
}

/// One encrypted value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedBlob {
    /// Per-value random nonce.
    pub nonce: Base64Bytes,
    /// Ciphertext with its authentication tag.
    pub ciphertext: Base64Bytes,
}

/// Encrypt `plaintext` under `key`, binding it to `aad`.
///
/// `aad` is the entry's own name. Without it, an attacker with write access to the file could move
/// the ciphertext of one entry onto another key — swapping the staging password onto the production
/// host — and every authentication check would still pass. With it, the move is detected.
pub(crate) fn seal(key: &DataKey, aad: &[u8], plaintext: &[u8]) -> VaultResult<SealedBlob> {
    let cipher = XChaCha20Poly1305::new_from_slice(key.expose())
        .map_err(|_| VaultError::MalformedVault("key is the wrong length"))?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).map_err(|_| VaultError::Random)?;
    let nonce = XNonce::from(nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| VaultError::Encryption)?;

    Ok(SealedBlob {
        nonce: Base64Bytes::new(nonce_bytes.to_vec()),
        ciphertext: Base64Bytes::new(ciphertext),
    })
}

/// Decrypt `blob`, which must have been sealed with the same `aad`.
///
/// Failure is deliberately indistinguishable between "wrong key" and "tampered ciphertext": the
/// caller decides what that means from context, and the distinction is not something an attacker
/// should be handed.
pub(crate) fn open(key: &DataKey, aad: &[u8], blob: &SealedBlob) -> VaultResult<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key.expose())
        .map_err(|_| VaultError::MalformedVault("key is the wrong length"))?;

    let nonce_bytes: [u8; NONCE_LEN] = blob
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::MalformedVault("nonce is the wrong length"))?;
    let nonce = XNonce::from(nonce_bytes);

    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: blob.ciphertext.as_slice(),
                aad,
            },
        )
        .map_err(|_| VaultError::Authentication)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> DataKey {
        DataKey::from_bytes([9u8; 32])
    }

    #[test]
    fn a_sealed_value_opens_again() {
        let blob = seal(&key(), b"entry-name", b"hunter2").expect("seals");
        let plaintext = open(&key(), b"entry-name", &blob).expect("opens");
        assert_eq!(plaintext, b"hunter2");
    }

    #[test]
    fn the_ciphertext_does_not_contain_the_plaintext() {
        let blob = seal(&key(), b"e", b"hunter2").expect("seals");
        let haystack = blob.ciphertext.as_slice();
        assert!(
            !haystack.windows(7).any(|window| window == b"hunter2"),
            "plaintext must not survive in the ciphertext"
        );
    }

    #[test]
    fn each_sealing_uses_a_fresh_nonce() {
        // Reusing a nonce with the same key breaks the cipher outright, so this is the single most
        // important property in the module.
        let first = seal(&key(), b"e", b"same plaintext").expect("seals");
        let second = seal(&key(), b"e", b"same plaintext").expect("seals");
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.ciphertext, second.ciphertext);
    }

    #[test]
    fn a_wrong_key_fails_to_open() {
        let blob = seal(&key(), b"e", b"hunter2").expect("seals");
        let other = DataKey::from_bytes([1u8; 32]);
        assert!(matches!(
            open(&other, b"e", &blob),
            Err(VaultError::Authentication)
        ));
    }

    #[test]
    fn moving_a_value_to_another_entry_is_detected() {
        // The reason the entry name is authenticated data: without it, this would succeed and the
        // staging password would silently become the production one.
        let blob = seal(&key(), b"staging/password", b"hunter2").expect("seals");
        assert!(matches!(
            open(&key(), b"production/password", &blob),
            Err(VaultError::Authentication)
        ));
    }

    #[test]
    fn tampering_with_the_ciphertext_is_detected() {
        let mut blob = seal(&key(), b"e", b"hunter2").expect("seals");
        let mut bytes = blob.ciphertext.as_slice().to_vec();
        bytes[0] ^= 0xFF;
        blob.ciphertext = Base64Bytes::new(bytes);
        assert!(matches!(
            open(&key(), b"e", &blob),
            Err(VaultError::Authentication)
        ));
    }

    #[test]
    fn a_truncated_nonce_is_reported_as_malformed_not_as_a_bad_password() {
        let mut blob = seal(&key(), b"e", b"hunter2").expect("seals");
        blob.nonce = Base64Bytes::new(vec![0u8; 4]);
        assert!(matches!(
            open(&key(), b"e", &blob),
            Err(VaultError::MalformedVault(_))
        ));
    }

    #[test]
    fn generated_parameters_have_a_random_salt_and_recommended_costs() {
        let first = KdfParams::generate().expect("params");
        let second = KdfParams::generate().expect("params");
        assert_ne!(first.salt, second.salt, "salt must be per-vault");
        assert_eq!(first.salt.as_slice().len(), SALT_LEN);
        assert_eq!(first.m_cost, Params::DEFAULT_M_COST);
        assert_eq!(first.t_cost, Params::DEFAULT_T_COST);
        assert_eq!(first.algorithm, KdfAlgorithm::Argon2id);
    }

    #[test]
    fn the_same_password_and_salt_derive_the_same_key() {
        let params = KdfParams::generate().expect("params");
        let first = derive_kek("correct horse", &params).expect("derives");
        let second = derive_kek("correct horse", &params).expect("derives");
        assert_eq!(first, second);
    }

    #[test]
    fn a_different_password_derives_a_different_key() {
        let params = KdfParams::generate().expect("params");
        let right = derive_kek("correct horse", &params).expect("derives");
        let wrong = derive_kek("correct hors", &params).expect("derives");
        assert_ne!(right, wrong);
    }

    #[test]
    fn a_different_salt_derives_a_different_key() {
        // Why the salt is per-vault: two people with the same password must not share a key.
        let first = KdfParams::generate().expect("params");
        let second = KdfParams::generate().expect("params");
        assert_ne!(
            derive_kek("same password", &first).expect("derives"),
            derive_kek("same password", &second).expect("derives")
        );
    }

    #[test]
    fn a_short_salt_is_rejected() {
        let mut params = KdfParams::generate().expect("params");
        params.salt = Base64Bytes::new(vec![0u8; 4]);
        assert!(matches!(
            derive_kek("x", &params),
            Err(VaultError::MalformedVault(_))
        ));
    }

    #[test]
    fn out_of_range_costs_are_rejected_rather_than_panicking() {
        let mut params = KdfParams::generate().expect("params");
        params.m_cost = 0;
        assert!(matches!(
            derive_kek("x", &params),
            Err(VaultError::MalformedVault(_))
        ));
    }
}
