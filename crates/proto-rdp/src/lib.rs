//! RDP, on IronRDP.
//!
//! This crate is the protocol half of the RDP support. It runs inside the `bestterm-rdp` helper
//! process rather than the application, so a decoder that falls over takes down one tab instead of
//! everything — see `crates/ipc-frame` for the boundary it speaks across.
//!
//! Being built outwards from the parts that can be checked without a screen to look at:
//!
//! * [`config`] turns a session's settings into what IronRDP's connector wants. Thirty fields, no
//!   defaults, and several of them decisions rather than transcriptions.
//!
//! The handshake, the active stage and input follow. RDP is the one protocol here whose correctness
//! is only fully visible on a display, so the parts that *can* be pinned down by a test are being
//! pinned down first.

pub mod config;

pub use config::{ConfigError, MAX_DIMENSION, MIN_DIMENSION};
