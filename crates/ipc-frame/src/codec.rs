//! The primitives every message is built from.
//!
//! Hand-written rather than derived. This is an ABI between two separate binaries that a package
//! manager can update independently, so the exact bytes are worth being able to read, version and
//! test — a derived format would put that behind whichever serialiser version each side happened to
//! be built against.
//!
//! Everything is little-endian. Both supported targets are little-endian, and a byte order that is
//! stated is better than one that is inherited.

/// Why a message could not be read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    /// The buffer ended in the middle of a value.
    #[error("the message ended after {read} of {needed} byte(s)")]
    Truncated {
        /// Bytes still available.
        read: usize,
        /// Bytes the value needed.
        needed: usize,
    },

    /// A tag byte that no version of this protocol defines.
    #[error("unknown {what} tag {tag}")]
    UnknownTag {
        /// What was being read, for a message that says where to look.
        what: &'static str,
        /// The byte that was not recognised.
        tag: u8,
    },

    /// A string field held bytes that are not UTF-8.
    #[error("a text field was not valid UTF-8")]
    NotUtf8,

    /// The message held more than its own fields account for.
    ///
    /// Refused rather than ignored. Bytes nobody read mean the sender wrote a field this build does
    /// not know about, which is a version skew — and one that would otherwise pass silently while
    /// the two sides quietly disagreed about what was said.
    #[error("{count} byte(s) were left over after the message")]
    TrailingBytes {
        /// How many bytes were not accounted for.
        count: usize,
    },

    /// A length field described more data than a message may contain.
    ///
    /// Checked rather than trusted: the sender is another process, and a corrupted or hostile length
    /// would otherwise turn into an allocation of that size.
    #[error("a length field of {length} exceeds the {limit}-byte limit")]
    TooLong {
        /// The length that was read.
        length: u64,
        /// The largest value accepted.
        limit: u64,
    },
}

/// Result alias for decoding.
pub type CodecResult<T> = Result<T, CodecError>;

/// The most bytes any single length-prefixed field may describe.
///
/// Clipboard text is the only field that can legitimately be large. 16 MiB is far past anything a
/// person pastes and far short of anything that hurts to allocate.
pub const MAX_FIELD_LEN: u64 = 16 * 1024 * 1024;

/// Appends values to a buffer.
///
/// A plain `Vec<u8>` extension rather than a type of its own: encoding cannot fail, so there is
/// nothing for a wrapper to add.
pub(crate) trait Encode {
    /// Append one byte.
    fn put_u8(&mut self, value: u8);
    /// Append a little-endian `u16`.
    fn put_u16(&mut self, value: u16);
    /// Append a little-endian `u32`.
    fn put_u32(&mut self, value: u32);
    /// Append a little-endian `u64`.
    fn put_u64(&mut self, value: u64);
    /// Append a little-endian `f32`.
    fn put_f32(&mut self, value: f32);
    /// Append `true` as 1 and `false` as 0.
    fn put_bool(&mut self, value: bool);
    /// Append a `u32` length followed by the bytes of the string.
    fn put_str(&mut self, value: &str);
    /// Append a `u32` count, for a sequence whose items follow.
    fn put_len(&mut self, value: usize);
}

impl Encode for Vec<u8> {
    fn put_u8(&mut self, value: u8) {
        self.push(value);
    }

    fn put_u16(&mut self, value: u16) {
        self.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u32(&mut self, value: u32) {
        self.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u64(&mut self, value: u64) {
        self.extend_from_slice(&value.to_le_bytes());
    }

    fn put_f32(&mut self, value: f32) {
        self.extend_from_slice(&value.to_le_bytes());
    }

    fn put_bool(&mut self, value: bool) {
        self.push(u8::from(value));
    }

    fn put_str(&mut self, value: &str) {
        // Truncation is impossible in practice and would be a silent corruption if it happened, so
        // the length is written from the real byte count and checked on the way back in.
        self.put_len(value.len());
        self.extend_from_slice(value.as_bytes());
    }

    fn put_len(&mut self, value: usize) {
        self.put_u32(u32::try_from(value).unwrap_or(u32::MAX));
    }
}

/// Reads values out of a buffer, in the order they were written.
pub(crate) struct Decoder<'a> {
    bytes: &'a [u8],
}

impl<'a> Decoder<'a> {
    /// Read from `bytes`.
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    /// How much is left unread.
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len()
    }

    /// Refuse a message that had bytes left over.
    ///
    /// Called once every message has been read. See [`CodecError::TrailingBytes`] for why leftovers
    /// are a failure rather than something to skip past.
    pub(crate) fn finish(&self) -> CodecResult<()> {
        match self.remaining() {
            0 => Ok(()),
            count => Err(CodecError::TrailingBytes { count }),
        }
    }

    /// Take `count` bytes.
    fn take(&mut self, count: usize) -> CodecResult<&'a [u8]> {
        if self.bytes.len() < count {
            return Err(CodecError::Truncated {
                read: self.bytes.len(),
                needed: count,
            });
        }
        let (head, tail) = self.bytes.split_at(count);
        self.bytes = tail;
        Ok(head)
    }

    /// Read one byte.
    pub(crate) fn u8(&mut self) -> CodecResult<u8> {
        Ok(self.take(1)?[0])
    }

    /// Read a little-endian `u16`.
    pub(crate) fn u16(&mut self) -> CodecResult<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Read a little-endian `u32`.
    pub(crate) fn u32(&mut self) -> CodecResult<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Read a little-endian `u64`.
    pub(crate) fn u64(&mut self) -> CodecResult<u64> {
        let bytes = self.take(8)?;
        let mut value = [0u8; 8];
        value.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(value))
    }

    /// Read a little-endian `f32`.
    pub(crate) fn f32(&mut self) -> CodecResult<f32> {
        let bytes = self.take(4)?;
        Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Read a boolean.
    ///
    /// Anything other than 0 reads as `true`, which is what every wire protocol that has ever
    /// carried a flag byte does, and avoids a decode failure over a byte whose meaning is obvious.
    pub(crate) fn bool(&mut self) -> CodecResult<bool> {
        Ok(self.u8()? != 0)
    }

    /// Read a length-prefixed string.
    pub(crate) fn string(&mut self) -> CodecResult<String> {
        let length = self.len()?;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| CodecError::NotUtf8)
    }

    /// Read a `u32` count, refusing one that is implausibly large.
    pub(crate) fn len(&mut self) -> CodecResult<usize> {
        let length = u64::from(self.u32()?);
        if length > MAX_FIELD_LEN {
            return Err(CodecError::TooLong {
                length,
                limit: MAX_FIELD_LEN,
            });
        }
        // `usize` is 64-bit on both targets; the cast is exact after the check above.
        Ok(length as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_primitive_survives_a_round_trip() {
        let mut buffer = Vec::new();
        buffer.put_u8(0xAB);
        buffer.put_u16(0xBEEF);
        buffer.put_u32(0xDEAD_BEEF);
        buffer.put_u64(u64::MAX);
        buffer.put_f32(-1.5);
        buffer.put_bool(true);
        buffer.put_bool(false);
        buffer.put_str("привет");

        let mut decoder = Decoder::new(&buffer);
        assert_eq!(decoder.u8().unwrap(), 0xAB);
        assert_eq!(decoder.u16().unwrap(), 0xBEEF);
        assert_eq!(decoder.u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(decoder.u64().unwrap(), u64::MAX);
        assert_eq!(decoder.f32().unwrap(), -1.5);
        assert!(decoder.bool().unwrap());
        assert!(!decoder.bool().unwrap());
        assert_eq!(decoder.string().unwrap(), "привет");
        assert_eq!(decoder.remaining(), 0, "nothing was left over");
    }

    #[test]
    fn the_encoding_is_little_endian_whatever_the_host_is() {
        // Stated, not inherited: a helper built for one target must be readable by a host built for
        // another, and this is the only thing that pins that down.
        let mut buffer = Vec::new();
        buffer.put_u32(1);
        assert_eq!(&buffer, &[1, 0, 0, 0]);
    }

    #[test]
    fn a_truncated_value_says_how_much_was_missing() {
        let mut decoder = Decoder::new(&[1, 2]);
        let error = decoder.u32().expect_err("four bytes are not there");
        assert_eq!(error, CodecError::Truncated { read: 2, needed: 4 });
    }

    #[test]
    fn a_truncated_string_body_is_not_mistaken_for_an_empty_one() {
        // The length says eight, only three follow. Reading this as "" would hand the caller a
        // plausible-looking value built from a corrupt message.
        let mut buffer = Vec::new();
        buffer.put_u32(8);
        buffer.extend_from_slice(b"abc");

        let error = Decoder::new(&buffer)
            .string()
            .expect_err("the body is short");
        assert!(matches!(error, CodecError::Truncated { .. }), "{error:?}");
    }

    #[test]
    fn a_length_larger_than_the_limit_is_refused_before_allocating() {
        // The sender is another process. A corrupt length must fail, not turn into an allocation.
        let mut buffer = Vec::new();
        buffer.put_u32(u32::MAX);

        let error = Decoder::new(&buffer).string().expect_err("absurd length");
        assert!(matches!(error, CodecError::TooLong { .. }), "{error:?}");
    }

    #[test]
    fn text_that_is_not_utf8_is_refused_rather_than_replaced() {
        let mut buffer = Vec::new();
        buffer.put_u32(2);
        buffer.extend_from_slice(&[0xFF, 0xFE]);

        let error = Decoder::new(&buffer).string().expect_err("not utf-8");
        assert_eq!(error, CodecError::NotUtf8);
    }

    #[test]
    fn an_empty_string_costs_only_its_length() {
        let mut buffer = Vec::new();
        buffer.put_str("");
        assert_eq!(buffer.len(), 4);
        assert_eq!(Decoder::new(&buffer).string().unwrap(), "");
    }
}
