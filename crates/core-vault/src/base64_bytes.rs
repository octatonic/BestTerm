//! Bytes that persist as base64.
//!
//! Salts, nonces and ciphertexts are binary, and the vault is a TOML file that people will look at
//! and that git will diff. Base64 keeps each field on one line, so adding a credential is a one-line
//! change rather than a rewritten blob.

use std::fmt;

use base64::prelude::{BASE64_STANDARD, Engine as _};
use serde::de::{self, Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};

/// A byte string stored as base64.
///
/// Holds only public material — salts, nonces, ciphertext — so it prints itself in full. Plaintext
/// never reaches this type; see [`crate::Secret`].
#[derive(Clone, PartialEq, Eq)]
pub struct Base64Bytes(Vec<u8>);

impl Base64Bytes {
    /// Wrap bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// The base64 form.
    pub fn to_base64(&self) -> String {
        BASE64_STANDARD.encode(&self.0)
    }
}

impl fmt::Debug for Base64Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Printed in full: none of this is secret, and being able to read it out of a bug report is
        // what makes a malformed vault diagnosable.
        write!(f, "Base64Bytes({})", self.to_base64())
    }
}

impl Serialize for Base64Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for Base64Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let bytes = BASE64_STANDARD
            .decode(text.as_bytes())
            .map_err(|error| de::Error::custom(format!("invalid base64: {error}")))?;
        Ok(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Holder {
        value: Base64Bytes,
    }

    #[test]
    fn bytes_round_trip_through_toml_as_one_line() {
        let holder = Holder {
            value: Base64Bytes::new(vec![0, 1, 2, 250, 255]),
        };
        let text = toml::to_string(&holder).expect("serialises");
        assert_eq!(text.lines().count(), 1, "got:\n{text}");
        assert_eq!(toml::from_str::<Holder>(&text).expect("parses"), holder);
    }

    #[test]
    fn empty_bytes_survive() {
        let holder = Holder {
            value: Base64Bytes::new(Vec::new()),
        };
        let text = toml::to_string(&holder).expect("serialises");
        assert_eq!(toml::from_str::<Holder>(&text).expect("parses"), holder);
    }

    #[test]
    fn invalid_base64_is_a_parse_error_not_a_panic() {
        let error = toml::from_str::<Holder>("value = \"not base64!!\"").expect_err("must fail");
        assert!(error.to_string().contains("base64"), "got {error}");
    }

    #[test]
    fn debug_shows_the_encoded_form() {
        let value = Base64Bytes::new(b"hi".to_vec());
        assert_eq!(format!("{value:?}"), "Base64Bytes(aGk=)");
    }
}
