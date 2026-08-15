//! RDP, on IronRDP.
//!
//! This crate is the protocol half of the RDP support. It runs inside the `bestterm-rdp` helper
//! process rather than the application, so a decoder that falls over takes down one tab instead of
//! everything — see `crates/ipc-frame` for the boundary it speaks across.
//!
//! It also lives in a *cargo workspace* of its own, which is more surprising and has a duller reason:
//! `ironrdp-connector` and `russh` require versions of `ecdsa` that cannot coexist in one dependency
//! graph. `helpers/rdp/Cargo.toml` has the details. The practical consequence is that every cargo
//! command has to name this workspace explicitly — `--manifest-path helpers/rdp/Cargo.toml`.
//!
//! Being built outwards from the parts that can be checked without a screen to look at:
//!
//! * [`config`] turns a session's settings into what IronRDP's connector wants. Thirty fields, no
//!   defaults, and several of them decisions rather than transcriptions.
//! * [`server_key`] decides whether the machine answering on port 3389 is the one we spoke to last
//!   time. `ironrdp-tls` accepts any certificate — deliberately, because RDP servers are self-signed
//!   and CredSSP is what actually binds the server's key — so noticing that the key changed is left
//!   to somebody, and that somebody is this.
//! * [`session`] opens a connection: TCP, then the security negotiation, then TLS, then the server
//!   key check, then everything else inside TLS.
//!
//! The active stage and input follow. RDP is the one protocol here whose correctness is only fully
//! visible on a display, so the parts that *can* be pinned down by a test are being pinned down
//! first.

pub mod active;
pub mod config;
pub mod server_key;
pub mod session;

pub use active::{ActiveSession, FrameLayout, Update};
pub use config::{ConfigError, MAX_DIMENSION, MIN_DIMENSION};
pub use server_key::{KeyFingerprint, KnownServers, ServerKeyChecker, Verdict};
pub use session::{Connected, RdpError, connect};
