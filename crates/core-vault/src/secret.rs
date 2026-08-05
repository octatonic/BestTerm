//! Types that hold secrets without leaking them.
//!
//! Two rules, both enforced by the types rather than by discipline:
//!
//! * **Nothing secret has a revealing `Debug`.** A password that prints itself ends up in a log, a
//!   panic message, or a bug report. Reading the value requires calling something named
//!   [`Secret::expose`], which is grep-able and hard to do by accident.
//! * **Memory is cleared on drop.** Not a complete defence — a `String` may have been reallocated
//!   and left copies behind — but it shortens the window in which a core dump or a swapped page
//!   contains the plaintext.

use std::fmt;

use zeroize::Zeroize;

/// A plaintext secret: a password, a passphrase.
///
/// Cloneable, because handing one to a protocol layer should not require moving it out of the vault,
/// and every clone clears itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a plaintext value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Read the plaintext.
    ///
    /// Named to be conspicuous: every call site is a place where a secret leaves its container, and
    /// should be reviewable by searching for this name.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The plaintext bytes.
    pub fn expose_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Whether the secret is empty.
    ///
    /// Useful without exposing it: an empty password is worth rejecting before it is used.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately says nothing about the value, not even its length: a length leak is enough to
        // narrow a brute-force search.
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A 32-byte symmetric key.
///
/// Used for both halves of the envelope: the key-encryption key derived from the master password,
/// and the data-encryption key that actually protects the entries.
#[derive(Clone, PartialEq, Eq)]
pub struct DataKey([u8; 32]);

impl DataKey {
    /// Length in bytes.
    pub const LEN: usize = 32;

    /// Wrap key material.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// A fresh random key.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// The raw key material.
    ///
    /// As with [`Secret::expose`], named so that call sites are easy to audit.
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for DataKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DataKey(<redacted>)")
    }
}

impl Drop for DataKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_prints_itself() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert_eq!(format!("{secret}"), "<redacted>");
        // The obvious accident: interpolating a struct that contains one.
        #[derive(Debug)]
        struct Holder {
            #[allow(dead_code)]
            password: Secret,
        }
        let printed = format!("{:?}", Holder { password: secret });
        assert!(!printed.contains("hunter2"), "got {printed}");
    }

    #[test]
    fn debug_output_does_not_leak_the_length() {
        // Even a length narrows a search, so short and long must look identical.
        let short = Secret::new("a");
        let long = Secret::new("a-very-long-passphrase-indeed");
        assert_eq!(format!("{short:?}"), format!("{long:?}"));
    }

    #[test]
    fn exposing_returns_the_value() {
        let secret = Secret::new("hunter2");
        assert_eq!(secret.expose(), "hunter2");
        assert_eq!(secret.expose_bytes(), b"hunter2");
        assert!(!secret.is_empty());
        assert!(Secret::new("").is_empty());
    }

    #[test]
    fn a_clone_is_equal_and_independent() {
        let secret = Secret::new("hunter2");
        let copy = secret.clone();
        assert_eq!(secret, copy);
        drop(copy);
        // Dropping the clone must not have disturbed the original.
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn a_key_never_prints_itself() {
        let key = DataKey::from_bytes([7u8; 32]);
        assert_eq!(format!("{key:?}"), "DataKey(<redacted>)");
    }

    #[test]
    fn generated_keys_differ_and_are_not_all_zero() {
        let a = DataKey::generate().expect("random");
        let b = DataKey::generate().expect("random");
        assert_ne!(a, b);
        assert_ne!(a.expose(), &[0u8; 32]);
        assert_eq!(a.expose().len(), DataKey::LEN);
    }
}
