//! The boundary between the host and a graphical helper process.
//!
//! RDP and VNC run in processes of their own. That buys three things: a decoder that crashes takes
//! down a tab rather than the application, a GPL C library can be kept out of the main binary, and
//! the two can be released on different schedules. It costs one thing — everything has to be said
//! rather than shared — and this crate is that saying.
//!
//! Two halves, because frames and control messages want opposite treatment:
//!
//! * [`shared`] carries pixels through a memory mapping both processes open. Eight megabytes a frame
//!   is not something to copy down a pipe thirty times a second.
//! * [`message`] carries everything else — connect, input, resize, "a frame is ready" — as small
//!   messages over a stream, encoded by [`codec`] in a format written out by hand so that two
//!   separately built binaries can be held to it.
//!
//! # Example
//!
//! ```
//! use bestterm_ipc_frame::{HostMessage, HelperMessage, PROTOCOL_VERSION};
//! use bestterm_surface::FrameSize;
//!
//! let asked = HostMessage::Resize(FrameSize::new(1280, 720));
//! let bytes = asked.encode();
//!
//! // The helper reads the same bytes back out.
//! match HostMessage::decode(&bytes).expect("decodes") {
//!     HostMessage::Resize(size) => assert_eq!(size, FrameSize::new(1280, 720)),
//!     other => panic!("expected a resize, got {other:?}"),
//! }
//!
//! // A message this build does not know is refused rather than guessed at.
//! assert!(HelperMessage::decode(&[0xFF]).is_err());
//! # let _ = PROTOCOL_VERSION;
//! ```

pub mod codec;
pub mod message;
pub mod shared;

pub use codec::{CodecError, CodecResult, MAX_FIELD_LEN};
pub use message::{
    ConnectRequest, FrameReady, HelperMessage, HostMessage, MAX_MESSAGE_LEN, PROTOCOL_VERSION,
};
pub use shared::{SLOT_COUNT, SharedFrames};
