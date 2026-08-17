//! VNC authentication, which is DES with a famous mistake in it.
//!
//! The server sends sixteen random bytes; the client encrypts them with the password as a DES key and
//! sends the result back. Three things about that are wrong in ways that have to be reproduced
//! exactly, because every VNC server in the world has the same bugs and interoperating means having
//! them too:
//!
//! 1. **The password is a DES key**, so it is eight bytes: longer ones are truncated and shorter ones
//!    padded with zeros. A sixteen-character VNC password has eight characters of strength.
//! 2. **Each key byte has its bits reversed.** AT&T's original implementation read the key
//!    least-significant-bit first, and every implementation since has copied it. Without this the
//!    response is wrong against every real server.
//! 3. **The two blocks are independent** — ECB, with the same key. Sixteen bytes of challenge is two
//!    DES blocks and nothing chains them.
//!
//! # This is not a secure scheme and is not presented as one
//!
//! Eight characters, DES, and no protection for anything after the handshake: the desktop and every
//! keystroke on it travel in clear text. It exists because it is what unencrypted VNC servers speak.
//! Whoever connects should be told, which is why [`Security::is_encrypted`] exists and why the helper
//! says so.

use bestterm_core_vault::Secret;
use des::Des;
use des::cipher::{Array, BlockCipherEncrypt, KeyInit};

/// How the server said it wants to be authenticated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    /// No authentication at all.
    None,
    /// The DES challenge described above.
    VncAuth,
}

impl Security {
    /// The number the protocol uses.
    pub fn code(self) -> u8 {
        match self {
            Self::None => 1,
            Self::VncAuth => 2,
        }
    }

    /// What a code means, or `None` for a scheme this build cannot speak.
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::None),
            2 => Some(Self::VncAuth),
            _ => None,
        }
    }

    /// Whether anything after the handshake is protected.
    ///
    /// Always false, for both schemes. VNC's own security types encrypt the handshake and nothing
    /// else; the desktop and every keystroke on it are in clear text either way. Kept as a method
    /// rather than a comment so the interface can say so without knowing why.
    pub fn is_encrypted(self) -> bool {
        false
    }

    /// A name for a person, and for a log.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "no authentication",
            Self::VncAuth => "VNC password",
        }
    }
}

/// Reverse the bits in a byte.
///
/// Point 2 above. `0b0000_0001` becomes `0b1000_0000`.
fn reverse_bits(byte: u8) -> u8 {
    byte.reverse_bits()
}

/// The eight-byte DES key a password becomes.
pub fn key_from_password(password: &Secret) -> [u8; 8] {
    let mut key = [0u8; 8];
    for (slot, byte) in key.iter_mut().zip(password.expose().as_bytes()) {
        *slot = reverse_bits(*byte);
    }
    key
}

/// Answer a challenge.
///
/// The challenge is always sixteen bytes; anything else is a server this build does not understand,
/// and returning `None` is better than encrypting whatever arrived and failing at the far end for a
/// reason nobody could see.
pub fn respond(challenge: &[u8], password: &Secret) -> Option<[u8; 16]> {
    let challenge: &[u8; 16] = challenge.try_into().ok()?;
    let key = key_from_password(password);
    let cipher = Des::new(&Array(key));

    let mut response = [0u8; 16];
    response.copy_from_slice(challenge);
    // Two independent blocks. Chaining them would be better cryptography and would not interoperate
    // with anything.
    for block in response.chunks_exact_mut(8) {
        let mut chunk = Array(<[u8; 8]>::try_from(&*block).expect("chunks_exact_mut gives eight"));
        cipher.encrypt_block(&mut chunk);
        block.copy_from_slice(&chunk.0);
    }
    Some(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_is_eight_bytes_with_its_bits_reversed() {
        // The quirk that makes this interoperate. Without the reversal the response is wrong against
        // every real server, and the failure looks exactly like a wrong password.
        let key = key_from_password(&Secret::new("a".to_string()));
        assert_eq!(key[0], 0b1000_0110, "'a' is 0x61, reversed is 0x86");
        assert_eq!(&key[1..], &[0u8; 7], "a short password is zero-padded");
    }

    #[test]
    fn a_long_password_is_truncated_to_eight_characters() {
        // Worth knowing rather than hiding: a sixteen-character VNC password has eight characters of
        // strength, and the ninth onwards is decoration.
        let eight = key_from_password(&Secret::new("password".to_string()));
        let longer = key_from_password(&Secret::new("password-and-more".to_string()));
        assert_eq!(eight, longer);
    }

    #[test]
    fn the_response_is_sixteen_bytes_and_depends_on_the_password() {
        let challenge = [0x11u8; 16];
        let a = respond(&challenge, &Secret::new("one".to_string())).expect("sixteen bytes");
        let b = respond(&challenge, &Secret::new("two".to_string())).expect("sixteen bytes");
        assert_ne!(a, b);
        assert_ne!(a, challenge, "the response is not the challenge");
    }

    #[test]
    fn the_two_blocks_are_independent() {
        // ECB, which is the bug and also the requirement. Two identical halves of a challenge must
        // produce two identical halves of a response; if they did not, something is chaining and no
        // server would accept it.
        let challenge = [0x42u8; 16];
        let response = respond(&challenge, &Secret::new("secret".to_string())).expect("valid");
        assert_eq!(&response[..8], &response[8..]);
    }

    #[test]
    fn a_challenge_of_the_wrong_length_is_refused() {
        // Better than encrypting whatever arrived and failing at the far end for a reason nobody
        // could see from here.
        let password = Secret::new("x".to_string());
        assert!(respond(&[0u8; 8], &password).is_none());
        assert!(respond(&[0u8; 17], &password).is_none());
        assert!(respond(&[], &password).is_none());
    }

    #[test]
    fn neither_scheme_protects_anything_after_the_handshake() {
        // The thing whoever connects has to be told. VNC's security types authenticate and then stop.
        for security in [Security::None, Security::VncAuth] {
            assert!(!security.is_encrypted(), "{security:?}");
        }
    }

    #[test]
    fn the_codes_round_trip_and_unknown_ones_are_refused() {
        for security in [Security::None, Security::VncAuth] {
            assert_eq!(Security::from_code(security.code()), Some(security));
        }
        // 0 means the connection failed, 5 is RA2, 16 is Tight, 18 is TLS: all real, none spoken here.
        for code in [0u8, 5, 16, 18, 30, 255] {
            assert_eq!(Security::from_code(code), None, "code {code}");
        }
    }
}
