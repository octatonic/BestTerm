//! SSH.
//!
//! Being built in the order the pieces can be verified rather than the order they run in.
//!
//! * [`known_hosts`] decides whether the server on the other end is the one we spoke to last time.
//!   Pure logic, exhaustively testable, and the piece where a mistake is a security bug rather than
//!   an inconvenience.
//! * [`ssh_config`] reads `~/.ssh/config`, so BestTerm inherits a setup someone already has instead
//!   of asking them to enter it all again.
//! * [`host_key`] decides *who answers* when the situation needs a decision, keeping the policy —
//!   prompt the user, accept nothing new, a fixed answer in a test — out of the connection code.
//! * [`transport`] is the connection itself, on `russh`.
//!
//! Public-key and agent authentication, jump chains and port forwarding follow.
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

pub mod host_key;
pub mod known_hosts;
pub mod ssh_config;
pub mod transport;

pub use host_key::{HostKeyDecision, HostKeyOutcome, HostKeyVerifier, StrictVerifier};
pub use known_hosts::{HostKey, HostsError, KnownHosts, Marker, Verdict};
pub use ssh_config::{JumpHop, Query, QueryContext, SshConfig};
pub use transport::{Auth, SshConnection, SshError, Target};
