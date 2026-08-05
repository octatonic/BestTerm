//! Windows-1252 decoding.
//!
//! `.mxtsessions` files are CP1252, not UTF-8 — the format's documentation points out that you can
//! test this with a `€` in a session title. Decoding them as UTF-8 either fails outright or, worse,
//! silently replaces every accented character in a host name or comment.
//!
//! A whole encoding crate is not warranted for one single-byte codepage. CP1252 agrees with Latin-1
//! everywhere except `0x80..=0x9F`, so the entire mapping is the table below plus an identity
//! function, and it is exhaustively testable.

/// CP1252's twenty-seven deviations from Latin-1, for bytes `0x80..=0x9F`.
///
/// The five holes are unassigned in CP1252; they decode to the replacement character rather than
/// failing, because a stray byte in a comment field is not a reason to refuse someone's whole
/// session tree.
const HIGH_CONTROLS: [char; 32] = [
    '\u{20AC}', // 0x80 €
    '\u{FFFD}', // 0x81 unassigned
    '\u{201A}', // 0x82 ‚
    '\u{0192}', // 0x83 ƒ
    '\u{201E}', // 0x84 „
    '\u{2026}', // 0x85 …
    '\u{2020}', // 0x86 †
    '\u{2021}', // 0x87 ‡
    '\u{02C6}', // 0x88 ˆ
    '\u{2030}', // 0x89 ‰
    '\u{0160}', // 0x8A Š
    '\u{2039}', // 0x8B ‹
    '\u{0152}', // 0x8C Œ
    '\u{FFFD}', // 0x8D unassigned
    '\u{017D}', // 0x8E Ž
    '\u{FFFD}', // 0x8F unassigned
    '\u{FFFD}', // 0x90 unassigned
    '\u{2018}', // 0x91 '
    '\u{2019}', // 0x92 '
    '\u{201C}', // 0x93 "
    '\u{201D}', // 0x94 "
    '\u{2022}', // 0x95 •
    '\u{2013}', // 0x96 –
    '\u{2014}', // 0x97 —
    '\u{02DC}', // 0x98 ˜
    '\u{2122}', // 0x99 ™
    '\u{0161}', // 0x9A š
    '\u{203A}', // 0x9B ›
    '\u{0153}', // 0x9C œ
    '\u{FFFD}', // 0x9D unassigned
    '\u{017E}', // 0x9E ž
    '\u{0178}', // 0x9F Ÿ
];

/// Decode CP1252 bytes.
///
/// Cannot fail: every one of the 256 byte values has a defined result.
pub(crate) fn decode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| match byte {
            0x80..=0x9F => HIGH_CONTROLS[usize::from(byte - 0x80)],
            // ASCII below, Latin-1 above: both are the code point of the byte itself.
            other => char::from(other),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_unchanged() {
        assert_eq!(decode(b"user@host:22"), "user@host:22");
    }

    #[test]
    fn the_euro_sign_is_the_formats_own_test_case() {
        // The format documentation names this exact character as the way to prove the encoding.
        assert_eq!(decode(&[0x80]), "€");
    }

    #[test]
    fn latin1_range_maps_to_itself() {
        // 0xE9 is é in both CP1252 and Latin-1.
        assert_eq!(decode(&[0xE9]), "é");
        assert_eq!(decode(&[0xFF]), "ÿ");
        assert_eq!(decode(&[0xA0]), "\u{A0}");
    }

    #[test]
    fn typographic_quotes_decode_correctly() {
        // These appear in comments copied out of a word processor and would be mangled by a naive
        // Latin-1 decode.
        assert_eq!(decode(&[0x93, 0x94]), "\u{201C}\u{201D}");
        assert_eq!(decode(&[0x92]), "\u{2019}");
    }

    #[test]
    fn unassigned_bytes_become_the_replacement_character() {
        for byte in [0x81u8, 0x8D, 0x8F, 0x90, 0x9D] {
            assert_eq!(decode(&[byte]), "\u{FFFD}", "byte {byte:#04x}");
        }
    }

    #[test]
    fn every_byte_decodes_to_exactly_one_character() {
        let all: Vec<u8> = (0u8..=255).collect();
        assert_eq!(decode(&all).chars().count(), 256);
    }

    #[test]
    fn a_realistic_line_survives() {
        // "Café" in CP1252 followed by ASCII.
        let bytes = [b'C', b'a', b'f', 0xE9, b'-', b's', b'r', b'v'];
        assert_eq!(decode(&bytes), "Café-srv");
    }
}
