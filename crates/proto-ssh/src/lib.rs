//! SSH.
//!
//! Being built in the order the pieces can be verified rather than the order they run in. What is
//! here now is [`known_hosts`], the part that decides whether the server on the other end is the one
//! we spoke to last time — pure logic, exhaustively testable, and the piece where a mistake is a
//! security bug rather than an inconvenience.
//!
//! The `russh` transport, authentication and channel multiplexing follow.
//!
//! # Example
//!
//! ```
//! use bestterm_proto_ssh::known_hosts::{HostKey, KnownHosts, Verdict};
//!
//! let file = "srv.int ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB1s3lNKzXBFT0mBiVKvJmXLLPPxSSnGKmDpTFVX9Fnv";
//! let hosts = KnownHosts::parse(file);
//!
//! let presented = HostKey::new("ssh-ed25519", vec![0u8; 32]);
//! // A key that is not the recorded one is reported as changed, not as a first connection.
//! assert!(matches!(
//!     hosts.verify("srv.int", 22, &presented),
//!     Verdict::Changed { .. }
//! ));
//! ```

pub mod known_hosts;

pub use known_hosts::{HostKey, HostsError, KnownHosts, Marker, Verdict};
