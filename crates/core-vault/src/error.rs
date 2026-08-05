//! Vault errors.

/// What can go wrong.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VaultError {
    /// The operating system would not supply randomness.
    #[error("could not obtain randomness from the operating system")]
    Random,

    /// Encryption failed. Not caused by anything the user did.
    #[error("encryption failed")]
    Encryption,

    /// A value did not authenticate: the key is wrong, or the file was altered.
    ///
    /// The two are deliberately not distinguished — the difference is not something to hand an
    /// attacker, and the caller knows from context which is likely.
    #[error("the value could not be authenticated: wrong key, or the vault has been altered")]
    Authentication,

    /// The master password did not unlock the vault.
    #[error("incorrect master password")]
    WrongPassword,

    /// The key from the operating system's keystore no longer opens this vault.
    ///
    /// Happens after the master password is changed on another machine while the local keystore still
    /// holds the old key. Recoverable by asking for the password.
    #[error("the stored key does not open this vault; the master password is needed")]
    StaleStoredKey,

    /// The file is not a vault this build can make sense of.
    #[error("the vault file is malformed: {0}")]
    MalformedVault(&'static str),

    /// A base64 field could not be decoded.
    #[error("the vault file contains invalid base64: {0}")]
    InvalidBase64(String),

    /// An entry name is reserved for the vault's own use.
    #[error("`{0}` is not a usable entry name")]
    ReservedName(String),

    /// The operating system's keystore refused.
    #[error("the system keystore refused: {0}")]
    KeyStore(String),
}

/// Result alias for vault operations.
pub type VaultResult<T> = std::result::Result<T, VaultError>;
