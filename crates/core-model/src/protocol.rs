//! Per-protocol connection settings.
//!
//! # No secrets here
//!
//! Nothing in this module holds a password, a passphrase or a private key. Credentials live in the
//! vault and are referenced by an opaque [`CredentialRef`]. That separation is what lets the session
//! tree be a plain, human-readable, git-synchronisable TOML file while the secrets stay encrypted —
//! and it is the difference between BestTerm and MobaXterm, whose `.mxtsessions` files carry SFTP
//! passwords in clear text.

use serde::{Deserialize, Serialize};

/// A handle to a secret held in the vault.
///
/// Deliberately opaque: the model can say *which* credential a session uses without being able to
/// read it, so dumping the tree can never leak one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialRef(String);

impl CredentialRef {
    /// Wrap a vault key.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The vault key.
    pub fn key(&self) -> &str {
        &self.0
    }
}

/// Which protocol a session speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// A shell on this machine.
    LocalShell,
    /// SSH.
    Ssh,
    /// Telnet.
    Telnet,
    /// A serial port.
    Serial,
    /// Microsoft Remote Desktop.
    Rdp,
    /// VNC / RFB.
    Vnc,
}

impl Protocol {
    /// Stable identifier, safe to persist and to key an icon off.
    pub fn id(self) -> &'static str {
        match self {
            Self::LocalShell => "shell",
            Self::Ssh => "ssh",
            Self::Telnet => "telnet",
            Self::Serial => "serial",
            Self::Rdp => "rdp",
            Self::Vnc => "vnc",
        }
    }

    /// Whether this protocol presents as a character grid rather than a framebuffer.
    ///
    /// Decides which of the two protocol abstractions a session will use — `Transport` or
    /// `GraphicalSurface`. See `docs/ARCHITECTURE.md`.
    pub fn is_text(self) -> bool {
        match self {
            Self::LocalShell | Self::Ssh | Self::Telnet | Self::Serial => true,
            Self::Rdp | Self::Vnc => false,
        }
    }
}

/// How to reach the far end.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "kebab-case")]
pub enum ProtocolConfig {
    /// A shell on this machine.
    LocalShell(LocalShellConfig),
    /// SSH.
    Ssh(SshConfig),
    /// Telnet.
    Telnet(TelnetConfig),
    /// A serial port.
    Serial(SerialConfig),
    /// Microsoft Remote Desktop.
    Rdp(RdpConfig),
    /// VNC / RFB.
    Vnc(VncConfig),
}

impl ProtocolConfig {
    /// Which protocol this is.
    pub fn protocol(&self) -> Protocol {
        match self {
            Self::LocalShell(_) => Protocol::LocalShell,
            Self::Ssh(_) => Protocol::Ssh,
            Self::Telnet(_) => Protocol::Telnet,
            Self::Serial(_) => Protocol::Serial,
            Self::Rdp(_) => Protocol::Rdp,
            Self::Vnc(_) => Protocol::Vnc,
        }
    }

    /// The host this session connects to, where the notion applies.
    ///
    /// Used by search, so that typing a hostname finds the session even when it is named something
    /// else entirely — which, in a tree of five hundred hosts, is most of them.
    pub fn host(&self) -> Option<&str> {
        match self {
            Self::Ssh(c) => Some(&c.host),
            Self::Telnet(c) => Some(&c.host),
            Self::Rdp(c) => Some(&c.host),
            Self::Vnc(c) => Some(&c.host),
            Self::Serial(c) => Some(&c.device),
            Self::LocalShell(_) => None,
        }
    }

    /// The TCP port this session connects to, where the notion applies.
    ///
    /// `None` for a serial line and a local shell, which have no port rather than port zero -- the
    /// distinction matters to anything deciding whether it can try to reach the thing.
    pub fn port(&self) -> Option<u16> {
        match self {
            Self::Ssh(c) => Some(c.port),
            Self::Telnet(c) => Some(c.port),
            Self::Rdp(c) => Some(c.port),
            Self::Vnc(c) => Some(c.port),
            Self::Serial(_) | Self::LocalShell(_) => None,
        }
    }
    /// A one-line description for the status bar.
    pub fn summary(&self) -> String {
        match self {
            Self::LocalShell(c) => c
                .program
                .clone()
                .unwrap_or_else(|| "local shell".to_string()),
            Self::Ssh(c) => match &c.user {
                Some(user) => format!("{user}@{}:{}", c.host, c.port),
                None => format!("{}:{}", c.host, c.port),
            },
            Self::Telnet(c) => format!("{}:{}", c.host, c.port),
            Self::Serial(c) => format!("{} @ {}", c.device, c.baud),
            Self::Rdp(c) => format!("{}:{}", c.host, c.port),
            Self::Vnc(c) => format!("{}:{}", c.host, c.port),
        }
    }
}

/// A shell on this machine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalShellConfig {
    /// Identifier from shell discovery, e.g. `wsl:Ubuntu-22.04`. Preferred over `program`, because
    /// it survives the shell moving to a different path.
    pub shell_id: Option<String>,
    /// Explicit executable, when the user wants something discovery did not find.
    pub program: Option<String>,
    /// Arguments to pass.
    pub args: Vec<String>,
    /// Working directory to start in.
    pub cwd: Option<String>,
}

/// How to authenticate an SSH connection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum SshAuth {
    /// Try the running ssh-agent. The default, because it is what already works for most people.
    #[default]
    Agent,
    /// Password, held in the vault.
    Password {
        /// Vault handle, absent when the user chose not to store it.
        credential: Option<CredentialRef>,
    },
    /// Public key.
    PublicKey {
        /// Path to the private key.
        path: String,
        /// Vault handle for the passphrase, absent when the key has none or it is not stored.
        passphrase: Option<CredentialRef>,
    },
    /// Keyboard-interactive, which is how most one-time-password setups present themselves.
    KeyboardInteractive,
    /// Hand the connection to the system `ssh` binary.
    ///
    /// The escape hatch for what a pure-Rust client cannot do: GSSAPI/Kerberos, FIDO2 `sk-` keys,
    /// certificates, corporate `ProxyCommand`. Recorded in `docs/ARCHITECTURE.md`.
    ExternalOpenSsh,
}

/// Which direction a port forward runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForwardKind {
    /// Listen locally, connect from the remote end. `ssh -L`.
    Local,
    /// Listen remotely, connect from here. `ssh -R`.
    Remote,
    /// A local SOCKS proxy. `ssh -D`.
    Dynamic,
}

/// One port forward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortForward {
    /// Direction.
    pub kind: ForwardKind,
    /// Address to bind. Defaults to loopback, which is the safe choice.
    #[serde(default = "loopback")]
    pub bind_address: String,
    /// Port to bind.
    pub bind_port: u16,
    /// Target host. Unused for [`ForwardKind::Dynamic`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_host: Option<String>,
    /// Target port. Unused for [`ForwardKind::Dynamic`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port: Option<u16>,
    /// Whether to open it as soon as the session connects.
    #[serde(default)]
    pub auto_open: bool,
}

fn loopback() -> String {
    "127.0.0.1".to_string()
}

/// SSH.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SshConfig {
    /// Hostname or address.
    pub host: String,
    /// Port.
    pub port: u16,
    /// Login name. Absent means "whatever `ssh_config` or the local username says".
    pub user: Option<String>,
    /// Authentication method.
    pub auth: SshAuth,
    /// Jump hosts, nearest first, as ids of other sessions in the tree.
    ///
    /// Referencing sessions rather than copying their host and credentials is the point: a bastion
    /// whose address changes is edited once.
    pub jump_hosts: Vec<crate::NodeId>,
    /// Port forwards belonging to this session.
    pub forwards: Vec<PortForward>,
    /// Command to run instead of a login shell.
    pub command: Option<String>,
    /// Keep the session open after `command` finishes.
    ///
    /// Without it a session that ran one command closes the moment it ends, taking its output
    /// with it — which is exactly wrong when the command was the thing you wanted to read.
    #[serde(default)]
    pub keep_open_after_command: bool,
    /// Ask for the connection to be compressed.
    ///
    /// Worth having on a slow link and worth not having on a fast one, where it costs processor
    /// time to save bandwidth nobody was short of.
    #[serde(default)]
    pub compression: bool,
    /// A proxy to reach the server through.
    ///
    /// Distinct from a jump host, which is an SSH connection carrying another; this is a proxy
    /// protocol in front of the socket. The reference calls its own version experimental.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<Proxy>,
}

/// How to reach a server through something else.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Proxy {
    /// Which protocol the proxy speaks.
    pub kind: ProxyKind,
    /// The proxy's address.
    pub host: String,
    /// And its port.
    pub port: u16,
    /// The login to give it, when it wants one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
}

/// The proxy protocols the reference offers.
///
/// Recorded in full because the list is measured, and a session configured against one this build
/// cannot speak should keep its setting rather than have it silently rewritten to `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyKind {
    /// Straight to the server.
    #[default]
    None,
    /// SOCKS4.
    Socks4,
    /// SOCKS5.
    Socks5,
    /// HTTP `CONNECT`.
    Http,
    /// A telnet proxy.
    Telnet,
    /// A local command that provides the socket.
    Local,
    /// An existing SSH forward.
    SshForwarding,
    /// An `ssh` command that provides the socket.
    SshCommand,
}

impl ProxyKind {
    /// Every kind, in the order the reference lists them.
    pub const ALL: [Self; 8] = [
        Self::None,
        Self::Socks4,
        Self::Socks5,
        Self::Http,
        Self::Telnet,
        Self::Local,
        Self::SshForwarding,
        Self::SshCommand,
    ];

    /// The label the reference uses.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Socks4 => "Socks4",
            Self::Socks5 => "Socks5",
            Self::Http => "Http",
            Self::Telnet => "Telnet",
            Self::Local => "Local",
            Self::SshForwarding => "SSH forwarding",
            Self::SshCommand => "SSH command",
        }
    }

    /// Whether `proto-ssh` can actually route through this one yet.
    ///
    /// Only SOCKS5 and HTTP, and only once the dialing code exists. The rest are stored so a
    /// session keeps what it was configured with; the interface says which are inert.
    pub fn is_implemented(self) -> bool {
        false
    }
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: None,
            auth: SshAuth::default(),
            jump_hosts: Vec::new(),
            keep_open_after_command: false,
            compression: false,
            proxy: None,
            forwards: Vec::new(),
            command: None,
        }
    }
}

/// Telnet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelnetConfig {
    /// Hostname or address.
    pub host: String,
    /// Port.
    pub port: u16,
}

impl Default for TelnetConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 23,
        }
    }
}

/// Serial line parity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Parity {
    /// No parity bit.
    #[default]
    None,
    /// Odd parity.
    Odd,
    /// Even parity.
    Even,
}

/// Serial flow control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlowControl {
    /// None.
    #[default]
    None,
    /// XON/XOFF.
    Software,
    /// RTS/CTS.
    Hardware,
}

/// A serial port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SerialConfig {
    /// Device path or name: `COM3`, `/dev/ttyUSB0`.
    pub device: String,
    /// Baud rate.
    pub baud: u32,
    /// Data bits, 5 to 8.
    pub data_bits: u8,
    /// Parity.
    pub parity: Parity,
    /// Stop bits, 1 or 2.
    pub stop_bits: u8,
    /// Flow control.
    pub flow_control: FlowControl,
}

impl Default for SerialConfig {
    fn default() -> Self {
        // 115200 8N1 is what console cables are wired for.
        Self {
            device: String::new(),
            baud: 115_200,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: 1,
            flow_control: FlowControl::None,
        }
    }
}

/// Microsoft Remote Desktop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RdpConfig {
    /// Hostname or address.
    pub host: String,
    /// Port.
    pub port: u16,
    /// Login name.
    pub user: Option<String>,
    /// Windows domain.
    pub domain: Option<String>,
    /// Vault handle for the password.
    pub credential: Option<CredentialRef>,
    /// Share the clipboard with the remote session.
    pub clipboard: bool,
    /// Span the session across all local monitors.
    pub multi_monitor: bool,
}

impl Default for RdpConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 3389,
            user: None,
            domain: None,
            credential: None,
            clipboard: true,
            multi_monitor: false,
        }
    }
}

/// VNC / RFB.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VncConfig {
    /// Hostname or address.
    pub host: String,
    /// Port. 5900 is display `:0`.
    pub port: u16,
    /// Vault handle for the password.
    pub credential: Option<CredentialRef>,
    /// Observe without sending input.
    pub view_only: bool,
}

impl Default for VncConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 5900,
            credential: None,
            view_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_ids_are_unique_and_stable() {
        let all = [
            Protocol::LocalShell,
            Protocol::Ssh,
            Protocol::Telnet,
            Protocol::Serial,
            Protocol::Rdp,
            Protocol::Vnc,
        ];
        let mut ids: Vec<&str> = all.iter().map(|p| p.id()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn text_and_frame_protocols_are_split_correctly() {
        // This split decides which abstraction a session uses, so it is worth pinning.
        assert!(Protocol::Ssh.is_text());
        assert!(Protocol::LocalShell.is_text());
        assert!(Protocol::Telnet.is_text());
        assert!(Protocol::Serial.is_text());
        assert!(!Protocol::Rdp.is_text());
        assert!(!Protocol::Vnc.is_text());
    }

    #[test]
    fn default_ports_match_the_protocols() {
        assert_eq!(SshConfig::default().port, 22);
        assert_eq!(TelnetConfig::default().port, 23);
        assert_eq!(RdpConfig::default().port, 3389);
        assert_eq!(VncConfig::default().port, 5900);
    }

    #[test]
    fn serial_defaults_are_8n1_at_115200() {
        let s = SerialConfig::default();
        assert_eq!(
            (s.baud, s.data_bits, s.parity, s.stop_bits),
            (115_200, 8, Parity::None, 1)
        );
    }

    #[test]
    fn config_reports_its_own_protocol() {
        let cases: Vec<(ProtocolConfig, Protocol)> = vec![
            (
                ProtocolConfig::LocalShell(LocalShellConfig::default()),
                Protocol::LocalShell,
            ),
            (ProtocolConfig::Ssh(SshConfig::default()), Protocol::Ssh),
            (
                ProtocolConfig::Telnet(TelnetConfig::default()),
                Protocol::Telnet,
            ),
            (
                ProtocolConfig::Serial(SerialConfig::default()),
                Protocol::Serial,
            ),
            (ProtocolConfig::Rdp(RdpConfig::default()), Protocol::Rdp),
            (ProtocolConfig::Vnc(VncConfig::default()), Protocol::Vnc),
        ];
        for (config, expected) in cases {
            assert_eq!(config.protocol(), expected);
        }
    }

    #[test]
    fn host_is_exposed_for_search_where_it_exists() {
        let ssh = ProtocolConfig::Ssh(SshConfig {
            host: "db-1.internal".to_string(),
            ..Default::default()
        });
        assert_eq!(ssh.host(), Some("db-1.internal"));

        let shell = ProtocolConfig::LocalShell(LocalShellConfig::default());
        assert_eq!(shell.host(), None);
    }

    #[test]
    fn ssh_summary_includes_the_user_when_set() {
        let with_user = ProtocolConfig::Ssh(SshConfig {
            host: "h".to_string(),
            user: Some("root".to_string()),
            ..Default::default()
        });
        assert_eq!(with_user.summary(), "root@h:22");

        let without = ProtocolConfig::Ssh(SshConfig {
            host: "h".to_string(),
            ..Default::default()
        });
        assert_eq!(without.summary(), "h:22");
    }

    #[test]
    fn ssh_auth_defaults_to_the_agent() {
        assert_eq!(SshAuth::default(), SshAuth::Agent);
    }

    #[test]
    fn a_forward_binds_loopback_unless_told_otherwise() {
        let toml_text = r#"
            kind = "local"
            bind_port = 5432
            target_host = "db"
            target_port = 5432
        "#;
        let forward: PortForward = toml::from_str(toml_text).expect("parses");
        assert_eq!(forward.bind_address, "127.0.0.1");
        assert!(!forward.auto_open);
    }

    #[test]
    fn protocol_config_round_trips_through_toml_tagged_by_protocol() {
        let original = ProtocolConfig::Ssh(SshConfig {
            host: "bastion.example".to_string(),
            port: 2222,
            user: Some("ops".to_string()),
            ..Default::default()
        });
        let text = toml::to_string(&original).expect("serialises");
        assert!(
            text.contains("protocol = \"ssh\""),
            "the tag must be explicit in the file, got:\n{text}"
        );

        let back: ProtocolConfig = toml::from_str(&text).expect("deserialises");
        assert_eq!(back, original);
    }

    #[test]
    fn an_unknown_field_is_rejected_rather_than_silently_dropped() {
        // A typo in a hand-edited file must be reported, not ignored: silently losing a setting the
        // user believes they configured is worse than refusing to load.
        let text = r#"
            host = "h"
            prot = 22
        "#;
        assert!(toml::from_str::<SshConfig>(text).is_err());
    }

    #[test]
    fn credentials_are_references_not_values() {
        let auth = SshAuth::Password {
            credential: Some(CredentialRef::new("vault:session-1/password")),
        };
        let text = toml::to_string(&auth).expect("serialises");
        // The vault key may appear; a secret never can, because the type cannot hold one.
        assert!(text.contains("vault:session-1/password"));
    }
}
