//! The session tree: folders, sessions, and the settings that flow between them.
//!
//! This is the backbone the rest of BestTerm hangs off. SSH reads connection settings from it, the
//! `.mxtsessions` importer writes into it, the SFTP browser is bound to a session in it, and the
//! saved layout refers to its nodes by id.
//!
//! Three decisions here are worth knowing before using it:
//!
//! * **Ids are UUIDs.** Neither paths nor counters survive what this tree is put through — see
//!   [`NodeId`].
//! * **Settings inherit down the folder tree**, `None` meaning "inherit". A tree of five hundred
//!   hosts is only maintainable if the keepalive is set once — see [`SettingsOverride`].
//! * **No secrets live here.** Credentials are referenced by [`CredentialRef`] and held in the
//!   vault, which is what allows the tree itself to be a readable, git-synchronisable TOML file.
//!
//! # Example
//!
//! ```
//! use bestterm_core_model::{ProtocolConfig, SessionTree, SettingsOverride, SshConfig};
//!
//! let mut tree = SessionTree::new();
//! let prod = tree.add_folder(None, "Production")?;
//!
//! // The folder sets a keepalive; everything inside inherits it.
//! tree.get_mut(prod).expect("just added").settings = SettingsOverride {
//!     keepalive_secs: Some(30),
//!     ..Default::default()
//! };
//!
//! let db = tree.add_session(
//!     Some(prod),
//!     "mongo-1",
//!     ProtocolConfig::Ssh(SshConfig {
//!         host: "mongo-1.internal".to_string(),
//!         ..Default::default()
//!     }),
//! )?;
//!
//! assert_eq!(tree.resolve_settings(db).keepalive_secs, 30);
//! assert_eq!(tree.path_string(db), "Production / mongo-1");
//! # Ok::<(), bestterm_core_model::ModelError>(())
//! ```

pub mod doc;
mod id;
mod protocol;
mod settings;
mod tree;

pub use doc::{DocError, NodeDoc, TreeDoc};
pub use id::NodeId;
pub use protocol::{
    CredentialRef, FlowControl, ForwardKind, LocalShellConfig, Parity, PortForward, Protocol,
    ProtocolConfig, RdpConfig, SerialConfig, SshAuth, SshConfig, TelnetConfig, VncConfig,
};
pub use settings::{ResolvedSettings, SettingsOverride};
pub use tree::{ModelError, Node, NodeKind, SessionTree};
