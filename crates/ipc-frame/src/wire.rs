//! Putting messages on a stream and taking them off again.
//!
//! [`crate::message`] says what a message *is*; this says where one ends. A pipe is a stream of
//! bytes with no record boundaries, so each message is preceded by its length as a little-endian
//! `u32` — the same encoding [`crate::codec`] uses for every other number, so there is one byte
//! order to remember rather than two.
//!
//! # Why the length is checked before the read
//!
//! The length arrives from another process, and the natural implementation — allocate that many
//! bytes, then fill them — hands a stranger the allocator. A corrupt or hostile four bytes becomes a
//! four-gigabyte allocation. So the length is compared against [`MAX_MESSAGE_LEN`] first, and a
//! message claiming more than that is refused without allocating anything.
//!
//! # Blocking, not async
//!
//! Both sides read this on a thread of their own. The helper's other job is a socket, and mixing a
//! pipe into that select would mean an async stdin, which on Windows is a thread underneath anyway.
//! Being honest about the thread is simpler than hiding it.

use std::io::{self, Read, Write};

use crate::message::MAX_MESSAGE_LEN;

/// Bytes of length prefix in front of every message.
const PREFIX: usize = 4;

/// Write one message, prefixed with its length.
///
/// The whole thing goes out in a single `write_all` so that a reader on the far side cannot observe
/// a prefix without the message behind it. Flushing is the caller's business: a helper sending a
/// burst of frame notifications should flush once, not four times.
pub fn write_message(out: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|&length| length as usize <= MAX_MESSAGE_LEN)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a {} byte message exceeds the {MAX_MESSAGE_LEN} byte limit",
                    payload.len()
                ),
            )
        })?;

    let mut framed = Vec::with_capacity(PREFIX + payload.len());
    framed.extend_from_slice(&length.to_le_bytes());
    framed.extend_from_slice(payload);
    out.write_all(&framed)
}

/// Read one message into `buf`, replacing whatever was there.
///
/// Returns `false` when the stream ended cleanly between messages, which is how the far side says it
/// is finished and is not an error. A stream that ends *inside* a message is an error, because
/// something was lost.
pub fn read_message(input: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<bool> {
    let mut prefix = [0u8; PREFIX];
    if !read_exact_or_eof(input, &mut prefix)? {
        return Ok(false);
    }

    let length = u32::from_le_bytes(prefix) as usize;
    // Before the allocation, not after. See the module documentation.
    if length > MAX_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a message claiming {length} bytes exceeds the {MAX_MESSAGE_LEN} byte limit"),
        ));
    }

    buf.clear();
    buf.resize(length, 0);
    input.read_exact(buf)?;
    Ok(true)
}

/// Fill `buf`, distinguishing "the stream ended before anything arrived" from "it ended midway".
///
/// [`Read::read_exact`] reports both as [`io::ErrorKind::UnexpectedEof`], and the difference is the
/// difference between a clean shutdown and a truncated message.
fn read_exact_or_eof(input: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "the stream ended after {filled} of {} prefix bytes",
                        buf.len()
                    ),
                ));
            }
            Ok(read) => filled += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_survives_the_round_trip() {
        let mut stream = Vec::new();
        write_message(&mut stream, b"first").expect("writes");
        write_message(&mut stream, b"second").expect("writes");

        let mut input = stream.as_slice();
        let mut buf = Vec::new();

        assert!(read_message(&mut input, &mut buf).expect("reads"));
        assert_eq!(buf, b"first");
        assert!(read_message(&mut input, &mut buf).expect("reads"));
        assert_eq!(buf, b"second");
        assert!(
            !read_message(&mut input, &mut buf).expect("reads"),
            "the end of the stream between messages is not an error"
        );
    }

    #[test]
    fn an_empty_message_is_a_message() {
        // Not a curiosity: `HostMessage::Shutdown` encodes to a single tag byte today, but a variant
        // that encodes to nothing at all would otherwise read as end-of-stream.
        let mut stream = Vec::new();
        write_message(&mut stream, b"").expect("writes");
        assert_eq!(stream.len(), PREFIX);

        let mut input = stream.as_slice();
        let mut buf = vec![1, 2, 3];
        assert!(read_message(&mut input, &mut buf).expect("reads"));
        assert!(buf.is_empty(), "the buffer is replaced, not appended to");
    }

    #[test]
    fn a_truncated_message_is_an_error_and_a_clean_end_is_not() {
        let mut stream = Vec::new();
        write_message(&mut stream, b"abcdef").expect("writes");
        stream.truncate(PREFIX + 2);

        let mut input = stream.as_slice();
        let mut buf = Vec::new();
        let error = read_message(&mut input, &mut buf).expect_err("the message was cut short");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_half_written_prefix_is_an_error_too() {
        let mut input: &[u8] = &[0x01, 0x00];
        let mut buf = Vec::new();
        let error = read_message(&mut input, &mut buf).expect_err("the prefix was cut short");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn an_oversized_length_is_refused_without_allocating() {
        // The point of the check: this is four bytes from another process, and allocating what they
        // ask for would hand a stranger the allocator.
        let mut stream = u32::MAX.to_le_bytes().to_vec();
        stream.extend_from_slice(b"nowhere near that many");

        let mut input = stream.as_slice();
        let mut buf = Vec::new();
        let error = read_message(&mut input, &mut buf).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(buf.is_empty(), "nothing was allocated for it");
    }

    #[test]
    fn writing_more_than_the_limit_is_refused_at_the_sender() {
        // Caught on the way out as well as on the way in, so a bug here surfaces in the process that
        // has the bug rather than the one receiving it.
        let payload = vec![0u8; MAX_MESSAGE_LEN + 1];
        let mut stream = Vec::new();
        let error = write_message(&mut stream, &payload).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(stream.is_empty(), "nothing was written");
    }

    #[test]
    fn a_real_message_goes_through_it() {
        use crate::{HostMessage, message::HelperMessage};
        use bestterm_surface::FrameSize;

        let mut stream = Vec::new();
        write_message(
            &mut stream,
            &HostMessage::Resize(FrameSize::new(1280, 720)).encode(),
        )
        .expect("writes");
        write_message(
            &mut stream,
            &HelperMessage::Closed { reason: None }.encode(),
        )
        .expect("writes");

        let mut input = stream.as_slice();
        let mut buf = Vec::new();

        read_message(&mut input, &mut buf).expect("reads");
        assert!(matches!(
            HostMessage::decode(&buf).expect("decodes"),
            HostMessage::Resize(size) if size == FrameSize::new(1280, 720)
        ));

        read_message(&mut input, &mut buf).expect("reads");
        assert!(matches!(
            HelperMessage::decode(&buf).expect("decodes"),
            HelperMessage::Closed { reason: None }
        ));
    }
}
