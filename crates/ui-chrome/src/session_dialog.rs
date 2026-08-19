//! The Session settings dialog.
//!
//! The largest single piece of interface work in the project, and the one that decides whether somebody
//! moving across from MobaXterm finds their settings where they expect them. Measured tab by tab; the
//! transcription lives in `docs/ui-parity/session-dialog.md` and this module is that document in code.
//!
//! # What is here and what is not
//!
//! The frame, the fifteen protocol tabs and each protocol's *basic* fields — the ones that identify
//! what to connect to. The `Advanced`, `Terminal` and `Network` tabs hold dozens of fields each and
//! have not been measured yet, so they are drawn as tabs with a note rather than invented.
//!
//! Nine of the fifteen protocols have no representation in [`bestterm_core_model`] yet. Rather than let
//! their `OK` button do nothing, [`SessionDialog::take_outcome`] reports them as unsupported by name,
//! so the dialog is honest about the difference between "not filled in" and "not built".
//!
//! # Shared fields, deliberately
//!
//! One state struct holds a superset of every protocol's fields rather than fifteen separate ones.
//! That is not laziness: the reference keeps what you typed when you switch tabs, so a host name typed
//! under SSH is still there under SFTP, and a per-protocol state would have to work to reproduce that.

use egui::{Align, Align2, CornerRadius, Layout, Rect, Sense, Stroke, Ui, vec2};

use bestterm_core_model::{
    ProtocolConfig, RdpConfig, SerialConfig, SshConfig, TelnetConfig, VncConfig,
};

use crate::ChromeTheme;

/// A tab in the dialog's protocol strip.
///
/// Fifteen, in the reference's order. Several have no counterpart in the session model yet and are
/// present because the strip is part of the layout: a dialog missing four of its tabs is not the
/// dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogProtocol {
    /// Secure Shell.
    Ssh,
    /// Telnet.
    Telnet,
    /// Remote shell.
    Rsh,
    /// A remote Unix desktop over XDMCP.
    Xdmcp,
    /// Microsoft Remote Desktop.
    Rdp,
    /// VNC / RFB.
    Vnc,
    /// File transfer over FTP.
    Ftp,
    /// File transfer over SSH.
    Sftp,
    /// A serial port.
    Serial,
    /// Open a local file, folder or URL.
    File,
    /// A shell on this machine.
    Shell,
    /// An embedded web browser.
    Browser,
    /// Mobile Shell.
    Mosh,
    /// Amazon S3.
    AwsS3,
    /// Windows Subsystem for Linux.
    Wsl,
}

impl DialogProtocol {
    /// Every tab, in the order the reference draws them.
    pub const ALL: [Self; 15] = [
        Self::Ssh,
        Self::Telnet,
        Self::Rsh,
        Self::Xdmcp,
        Self::Rdp,
        Self::Vnc,
        Self::Ftp,
        Self::Sftp,
        Self::Serial,
        Self::File,
        Self::Shell,
        Self::Browser,
        Self::Mosh,
        Self::AwsS3,
        Self::Wsl,
    ];

    /// The tab's label, transcribed exactly — including `Aws S3` rather than `AWS S3`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ssh => "SSH",
            Self::Telnet => "Telnet",
            Self::Rsh => "Rsh",
            Self::Xdmcp => "Xdmcp",
            Self::Rdp => "RDP",
            Self::Vnc => "VNC",
            Self::Ftp => "FTP",
            Self::Sftp => "SFTP",
            Self::Serial => "Serial",
            Self::File => "File",
            Self::Shell => "Shell",
            Self::Browser => "Browser",
            Self::Mosh => "Mosh",
            Self::AwsS3 => "Aws S3",
            Self::Wsl => "WSL",
        }
    }

    /// The name inside the `Basic … settings` group box.
    ///
    /// Not always the tab's label: the RDP tab says `Basic Rdp settings`, and the File tab says
    /// `Basic File/folder settings`. The inconsistent capitalisation is the reference's.
    pub fn group_name(self) -> &'static str {
        match self {
            Self::Ssh => "Basic SSH settings",
            Self::Telnet => "Basic Telnet settings",
            Self::Rsh => "Basic Rsh settings",
            Self::Xdmcp => "Basic Xdmcp settings",
            Self::Rdp => "Basic Rdp settings",
            Self::Vnc => "Basic Vnc settings",
            Self::Ftp => "Basic Ftp settings",
            Self::Sftp => "Basic Sftp settings",
            Self::Serial => "Basic Serial settings",
            Self::File => "Basic File/folder settings",
            Self::Shell => "Basic Shell settings",
            Self::Browser => "Basic Browser settings",
            Self::Mosh => "Basic Mosh settings",
            Self::AwsS3 => "Basic Aws S3 (experimental) settings",
            Self::Wsl => "Basic WSL settings",
        }
    }

    /// The line of prose in the description area.
    pub fn description(self) -> &'static str {
        match self {
            Self::Ssh => "Secure Shell (SSH) session",
            Self::Telnet => "Telnet session",
            Self::Rsh => "RSH session",
            Self::Xdmcp => "XDMCP (remote Unix desktop) session",
            Self::Rdp => "RDP (terminal services) session",
            Self::Vnc => "VNC session",
            Self::Ftp => "FTP session",
            Self::Sftp => "SFTP session",
            Self::Serial => "Serial (COM) session",
            Self::File => "Launch a given URL, a local folder or a local file",
            Self::Shell => "Local shell session",
            Self::Browser => "Embedded internet browser",
            Self::Mosh => "Mosh (Mobile Shell) session",
            Self::AwsS3 => "Amazon Web Services S3 session",
            Self::Wsl => "Windows Subsystem for Linux (WSL)",
        }
    }

    /// The default port, where the protocol has one.
    ///
    /// `None` for the protocols whose basic row has no port field at all — Rsh, Mosh and everything
    /// that does not connect to a socket. That is measured, not assumed: Rsh and Mosh both take a host
    /// and a user and no port.
    pub fn default_port(self) -> Option<u16> {
        match self {
            Self::Ssh | Self::Sftp => Some(22),
            Self::Telnet => Some(23),
            Self::Ftp => Some(21),
            Self::Rdp => Some(3389),
            Self::Vnc => Some(5900),
            _ => None,
        }
    }

    /// The secondary tabs, in order, not counting `Bookmark settings` which every protocol has.
    ///
    /// Worth reading as a rule rather than a list: `Terminal settings` appears where there is a
    /// character stream to configure, `Network settings` where there is a socket to a named host that
    /// might need a proxy. Two entries break the rule and are reproduced anyway — `Shell` has no
    /// terminal tab and `SFTP` has no network tab, both of which look like oversights in the reference.
    pub fn secondary_tabs(self) -> &'static [SecondaryTab] {
        use SecondaryTab::{Network, Terminal};
        match self {
            Self::Ssh | Self::Telnet => &[Terminal, Network],
            Self::Rsh | Self::Serial | Self::Mosh | Self::Wsl => &[Terminal],
            Self::Rdp | Self::Vnc => &[Network],
            _ => &[],
        }
    }
}

/// The tabs below the basic settings, other than the advanced one named after the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecondaryTab {
    /// The one named after the protocol.
    ///
    /// In the enum even though `secondary_tabs` does not list it, because selection is one value
    /// and a tab that cannot be selected is the state this row was in before: four labels that
    /// could be clicked and did nothing.
    Advanced,
    /// Character-stream settings.
    Terminal,
    /// Proxy and jump settings.
    Network,
    /// The session's name, its icon, and where it opens.
    Bookmark,
}

impl SecondaryTab {
    /// The tab's label.
    ///
    /// `Advanced` has none of its own: the reference names it after the protocol, so the caller
    /// composes it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Advanced => "Advanced settings",
            Self::Terminal => "Terminal settings",
            Self::Network => "Network settings",
            Self::Bookmark => "Bookmark settings",
        }
    }
}

/// The choices in the Shell tab's terminal-shell list.
///
/// The reference offers six, two of which are its bundled Cygwin environment. Those two are absent
/// here: `docs/ARCHITECTURE.md` lists cloning that environment as a permanent non-goal, and a shell
/// that cannot be launched is worse than one that is not offered.
pub const SHELL_CHOICES: &[&str] = &["Cmd", "Windows PowerShell", "PowerShell", "Bash (external)"];

/// Everything the dialog's fields hold.
///
/// A superset across protocols rather than one struct each, so that switching tabs keeps what was
/// typed — which is what the reference does.
#[derive(Clone, Debug)]
pub struct SessionFields {
    /// Host or address.
    pub host: String,
    /// Login name.
    pub user: String,
    /// Port, as typed. Kept as text so a half-typed number is not silently rounded to zero.
    pub port: String,
    /// Windows domain, for RDP.
    pub domain: String,
    /// Share the local clipboard with an RDP session.
    pub rdp_clipboard: bool,
    /// Span an RDP session across every local monitor.
    pub rdp_multi_monitor: bool,
    /// Watch a VNC desktop without sending anything to it.
    pub vnc_view_only: bool,
    /// Serial device.
    pub serial_port: String,
    /// Serial speed in bits per second.
    pub baud: String,
    /// Path for the File tab.
    pub path: String,
    /// URL for the Browser tab.
    pub url: String,
    /// Access key identifier for the Aws S3 tab.
    pub key_id: String,
    /// WSL distribution name.
    pub distribution: String,
    /// Which shell the Shell tab will launch, as an index into [`SHELL_CHOICES`].
    pub shell_choice: usize,
    /// Working directory for the Shell tab.
    pub startup_directory: String,
    /// Whether the Xdmcp tab is set to a named server rather than any.
    pub xdmcp_specific: bool,

    // ---- Advanced SSH settings, measured ----------------------------------------------------
    /// Ask the server for X11 forwarding.
    pub x11_forwarding: bool,
    /// Ask for the connection to be compressed.
    pub compression: bool,
    /// What to run instead of a login shell.
    pub remote_environment: usize,
    /// A command to run instead of a shell.
    pub execute_command: String,
    /// Keep the session open once that command ends.
    pub keep_open_after_command: bool,
    /// Which protocol the file browser uses, as an index into [`BROWSER_TYPES`].
    pub browser_type: usize,
    /// Make the browser follow the shell's working directory.
    pub follow_ssh_path: bool,
    /// Authenticate with a key file rather than the agent.
    pub use_private_key: bool,
    /// Where that key is.
    pub private_key: String,
    /// A macro to run when the session opens, as an index into [`MACRO_CHOICES`].
    pub macro_at_start: usize,

    // ---- Terminal settings, measured -------------------------------------------------------
    /// Whether Backspace sends `^H`.
    pub backspace_sends_ctrl_h: bool,
    /// Put the Windows `PATH` into the session's environment.
    pub use_windows_path: bool,
    /// What to tell the far end this terminal is, as an index into [`TERMINAL_TYPES`].
    pub terminal_type: usize,
    /// Write a transcript to disk.
    pub log_output: bool,
    /// And where.
    pub log_path: String,
    /// How long to wait between pasted lines, as an index into [`PASTE_DELAYS`].
    pub paste_delay: usize,
    /// Which highlighting to apply, as an index into [`HIGHLIGHTING`].
    pub highlighting: usize,

    // ---- Network settings, measured --------------------------------------------------------
    /// Which proxy protocol to use, as an index into [`bestterm_core_model::ProxyKind::ALL`].
    pub proxy_type: usize,
    /// The proxy's address.
    pub proxy_host: String,
    /// The login to give it.
    pub proxy_login: String,
    /// And its port, as typed.
    pub proxy_port: String,

    // ---- Bookmark settings, measured -------------------------------------------------------
    /// What the session is called in the tree.
    ///
    /// The field that made editing possible: without it a saved session could be renamed only
    /// through the tree's own menu, and the dialog that claimed to edit it could not touch its
    /// name.
    pub session_name: String,
    /// Keep the tab's title as the name rather than letting the program change it.
    pub lock_terminal_title: bool,
    /// Where the session opens, as an index into [`START_IN`].
    pub start_in: usize,
    /// Say so in the terminal when the session ends.
    pub reconnection_message: bool,
    /// Give the tab a colour of its own.
    pub customize_tab_color: bool,
    /// A note.
    pub comments: String,
}

impl Default for SessionFields {
    /// The reference's own defaults, so a fresh dialog agrees with it rather than starting from
    /// every box unticked: X11 forwarding and compression on, an interactive shell, the SFTP
    /// browser, `xterm`, Backspace sending `^H`, and the reconnection message shown.
    fn default() -> Self {
        Self {
            host: String::new(),
            user: String::new(),
            port: String::new(),
            domain: String::new(),
            // The reference has clipboard sharing on by default, and so does every other client
            // people arrive from. Multi-monitor is off, because spanning is a deliberate choice.
            rdp_clipboard: true,
            rdp_multi_monitor: false,
            vnc_view_only: false,
            serial_port: String::new(),
            baud: String::new(),
            path: String::new(),
            url: String::new(),
            key_id: String::new(),
            distribution: String::new(),
            shell_choice: 0,
            startup_directory: String::new(),
            xdmcp_specific: false,
            x11_forwarding: true,
            compression: true,
            remote_environment: 0,
            execute_command: String::new(),
            keep_open_after_command: false,
            browser_type: 0,
            follow_ssh_path: false,
            use_private_key: false,
            private_key: String::new(),
            macro_at_start: 0,
            backspace_sends_ctrl_h: true,
            use_windows_path: true,
            terminal_type: 0,
            log_output: false,
            log_path: String::new(),
            paste_delay: 0,
            highlighting: 0,
            proxy_type: 0,
            proxy_host: String::new(),
            proxy_login: String::new(),
            proxy_port: "1080".to_string(),
            session_name: String::new(),
            lock_terminal_title: true,
            start_in: 0,
            reconnection_message: true,
            customize_tab_color: false,
            comments: String::new(),
        }
    }
}

/// What the Advanced tab's `Remote environment` offers.
///
/// Only the first is honoured: the rest ask the server for a session shape this build does not
/// arrange yet, and a value stored is a value kept for when it does.
pub const REMOTE_ENVIRONMENTS: &[&str] = &[
    "Interactive shell",
    "LXDE desktop",
    "Xfce desktop",
    "Mate desktop",
    "Enlightenment desktop",
    "2D OpenGL desktop",
    "3D OpenGL desktop",
];

/// What the Advanced tab's `SSH-browser type` offers.
pub const BROWSER_TYPES: &[&str] = &["SFTP protocol", "SCP protocol", "None"];

/// What the Advanced tab's `Execute macro at session start` offers.
///
/// One entry, because there are no macros to list: recording them is phase 7. The list exists so
/// the control is the right shape when they arrive.
pub const MACRO_CHOICES: &[&str] = &["<none>"];

/// What the Terminal tab's `Terminal type` offers.
pub const TERMINAL_TYPES: &[&str] = &["xterm", "xterm-256color", "vt100", "linux", "ansi"];

/// What the Terminal tab's `Paste delay` offers.
pub const PASTE_DELAYS: &[&str] = &["Auto", "None", "10ms", "25ms", "50ms", "100ms"];

/// What the Terminal tab's `Syntax highlighting` offers.
pub const HIGHLIGHTING: &[&str] = &["Standard keywords (OK/warning/error/...)", "None"];

/// What the Bookmark tab's `Start session in` offers.
pub const START_IN: &[&str] = &["Normal tab", "Split pane", "New window"];

/// What the dialog produced.
#[derive(Clone, Debug)]
pub enum DialogOutcome {
    /// A session to open or save.
    Accepted(Box<ProtocolConfig>),
    /// The person cancelled.
    Cancelled,
    /// `OK` on a protocol the session model has no representation for yet.
    ///
    /// Reported rather than swallowed: "nothing happened" is indistinguishable from a bug, and the
    /// name is what makes the message worth reading.
    Unsupported(&'static str),
    /// `OK` with a required field empty.
    Incomplete {
        /// The field's label, as the dialog shows it.
        field: &'static str,
    },
}

/// The Session settings dialog.
#[derive(Clone, Debug)]
pub struct SessionDialog {
    /// Whether it is on screen.
    pub open: bool,
    /// Which protocol tab is selected.
    pub protocol: DialogProtocol,
    /// The field contents.
    pub fields: SessionFields,
    /// Which of the lower tabs is showing.
    pub secondary: SecondaryTab,
    outcome: Option<DialogOutcome>,
}

impl Default for SessionDialog {
    fn default() -> Self {
        Self {
            open: false,
            protocol: DialogProtocol::Ssh,
            fields: SessionFields::default(),
            secondary: SecondaryTab::Advanced,
            outcome: None,
        }
    }
}

impl SessionDialog {
    /// Show the dialog filled in from an existing session.
    ///
    /// The counterpart of [`SessionDialog::open_fresh`], and the reason the dialog was unusable for
    /// anything but creating: a saved session could be opened and deleted and never *changed*.
    ///
    /// Only the fields this dialog collects are loaded, which is the same set it can produce.
    /// Anything a session carries that it cannot show — jump hosts, forwards, a stored credential,
    /// a private key — is left alone by [`SessionDialog::merge_into`] rather than lost on the way
    /// through. A dialog that silently drops what it cannot display is worse than one that cannot
    /// edit at all.
    pub fn open_for(&mut self, config: &ProtocolConfig) {
        *self = Self {
            open: true,
            ..Self::default()
        };

        match config {
            ProtocolConfig::Ssh(ssh) => {
                self.protocol = DialogProtocol::Ssh;
                self.fields.host = ssh.host.clone();
                self.fields.port = ssh.port.to_string();
                self.fields.user = ssh.user.clone().unwrap_or_default();
                self.fields.compression = ssh.compression;
                self.fields.execute_command = ssh.command.clone().unwrap_or_default();
                self.fields.keep_open_after_command = ssh.keep_open_after_command;
                if let bestterm_core_model::SshAuth::PublicKey { path, .. } = &ssh.auth {
                    self.fields.use_private_key = true;
                    self.fields.private_key = path.clone();
                }
                if let Some(proxy) = &ssh.proxy {
                    self.fields.proxy_type = bestterm_core_model::ProxyKind::ALL
                        .iter()
                        .position(|kind| *kind == proxy.kind)
                        .unwrap_or(0);
                    self.fields.proxy_host = proxy.host.clone();
                    self.fields.proxy_port = proxy.port.to_string();
                    self.fields.proxy_login = proxy.login.clone().unwrap_or_default();
                }
            }
            ProtocolConfig::Telnet(telnet) => {
                self.protocol = DialogProtocol::Telnet;
                self.fields.host = telnet.host.clone();
                self.fields.port = telnet.port.to_string();
            }
            ProtocolConfig::Rdp(rdp) => {
                self.protocol = DialogProtocol::Rdp;
                self.fields.host = rdp.host.clone();
                self.fields.port = rdp.port.to_string();
                self.fields.user = rdp.user.clone().unwrap_or_default();
                self.fields.domain = rdp.domain.clone().unwrap_or_default();
                self.fields.rdp_clipboard = rdp.clipboard;
                self.fields.rdp_multi_monitor = rdp.multi_monitor;
            }
            ProtocolConfig::Vnc(vnc) => {
                self.protocol = DialogProtocol::Vnc;
                self.fields.host = vnc.host.clone();
                self.fields.port = vnc.port.to_string();
                self.fields.vnc_view_only = vnc.view_only;
            }
            ProtocolConfig::Serial(serial) => {
                self.protocol = DialogProtocol::Serial;
                self.fields.serial_port = serial.device.clone();
                self.fields.baud = serial.baud.to_string();
            }
            // A protocol this dialog cannot show opens on its own tab with nothing filled in,
            // rather than on the SSH tab with the wrong fields: an empty form says "not yet" and a
            // populated wrong one says something false.
            other => {
                self.protocol = match other.protocol() {
                    bestterm_core_model::Protocol::LocalShell => DialogProtocol::Shell,
                    _ => DialogProtocol::Ssh,
                };
            }
        }
    }

    /// Carry what this dialog collected back onto an existing session.
    ///
    /// Not a replacement. `ProtocolConfig` holds things the dialog has no field for, and building a
    /// fresh one would drop them — a session's jump hosts would vanish because somebody corrected
    /// its port. So it is merged field by field, and only where the protocol still matches; changing
    /// a saved session's protocol is a different operation, and that one replaces.
    pub fn merge_into(produced: ProtocolConfig, existing: &mut ProtocolConfig) {
        match (produced, existing) {
            (ProtocolConfig::Ssh(new), ProtocolConfig::Ssh(old)) => {
                old.host = new.host;
                old.port = new.port;
                old.user = new.user;
                old.command = new.command;
                old.keep_open_after_command = new.keep_open_after_command;
                old.compression = new.compression;
                old.proxy = new.proxy;
                // `auth` moves only when the dialog actually said something about it, which is the
                // one rule this merge exists for. `SshAuth` has more shapes than the dialog has
                // fields -- a vault password, a keyboard-interactive login, the external `ssh`
                // binary -- and copying a default over the top of one of those is the bug that left
                // 128 imported sessions authenticating with an agent that was not running.
                match new.auth {
                    // A key was named, so that is a choice.
                    bestterm_core_model::SshAuth::PublicKey { .. } => old.auth = new.auth,
                    // The box was cleared on a session that had a key, which is also a choice: back
                    // to the agent.
                    bestterm_core_model::SshAuth::Agent
                        if matches!(old.auth, bestterm_core_model::SshAuth::PublicKey { .. }) =>
                    {
                        old.auth = bestterm_core_model::SshAuth::Agent;
                    }
                    // Anything else the dialog produced is a default it had no field for.
                    _ => {}
                }
            }
            (ProtocolConfig::Telnet(new), ProtocolConfig::Telnet(old)) => *old = new,
            (ProtocolConfig::Rdp(new), ProtocolConfig::Rdp(old)) => {
                old.host = new.host;
                old.port = new.port;
                old.user = new.user;
                old.domain = new.domain;
                // Copied, now that the tab can set them. Left out while it could not, which was
                // right then and would silently reset an imported session now.
                old.clipboard = new.clipboard;
                old.multi_monitor = new.multi_monitor;
            }
            (ProtocolConfig::Vnc(new), ProtocolConfig::Vnc(old)) => {
                old.host = new.host;
                old.port = new.port;
                old.view_only = new.view_only;
            }
            (ProtocolConfig::Serial(new), ProtocolConfig::Serial(old)) => {
                old.device = new.device;
                old.baud = new.baud;
            }
            // The protocol changed, which is a replacement rather than an edit: nothing of the old
            // configuration means anything under the new one.
            (produced, existing) => *existing = produced,
        }
    }

    /// Show the dialog, resetting it to the state a fresh one starts in.
    pub fn open_fresh(&mut self) {
        *self = Self {
            open: true,
            ..Self::default()
        };
        self.set_port_for_protocol();
    }

    /// Take whatever the dialog produced, leaving nothing behind.
    ///
    /// Taken rather than read so that one press of `OK` cannot be acted on twice.
    pub fn take_outcome(&mut self) -> Option<DialogOutcome> {
        self.outcome.take()
    }

    /// Put the selected protocol's default port in the port field.
    ///
    /// Called when the tab changes, and only when the field still holds a default: somebody who typed
    /// 2222 and then switched from SSH to SFTP meant to keep it.
    fn set_port_for_protocol(&mut self) {
        let was_default = DialogProtocol::ALL
            .iter()
            .filter_map(|p| p.default_port())
            .any(|port| self.fields.port == port.to_string());

        if self.fields.port.is_empty() || was_default {
            self.fields.port = self
                .protocol
                .default_port()
                .map(|port| port.to_string())
                .unwrap_or_default();
        }
    }

    /// The proxy the Network tab describes, or `None` for a direct connection.
    fn proxy(&self) -> Option<bestterm_core_model::Proxy> {
        let kind = *bestterm_core_model::ProxyKind::ALL
            .get(self.fields.proxy_type)
            .unwrap_or(&bestterm_core_model::ProxyKind::None);
        if kind == bestterm_core_model::ProxyKind::None {
            return None;
        }
        let login = self.fields.proxy_login.trim();
        Some(bestterm_core_model::Proxy {
            kind,
            host: self.fields.proxy_host.trim().to_owned(),
            // Zero rather than a guess: the reference shows 1080 because SOCKS uses it, and a
            // silently substituted port is worse than one that is plainly unset.
            port: self.fields.proxy_port.trim().parse().unwrap_or(0),
            login: (!login.is_empty()).then(|| login.to_owned()),
        })
    }

    /// What the session's name should be, or `None` to leave it as it is.
    ///
    /// Trimmed and checked for emptiness here rather than at the caller: a name is the thing a
    /// session is found by in a tree of five hundred, and a blank one is a session somebody has
    /// lost.
    pub fn session_name(&self) -> Option<String> {
        let name = self.fields.session_name.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }

    /// The note on the Bookmark tab, or `None` when it was cleared.
    pub fn comment(&self) -> Option<String> {
        let comment = self.fields.comments.trim();
        (!comment.is_empty()).then(|| comment.to_owned())
    }

    /// The per-session settings the dialog collects, merged onto what a node already has.
    ///
    /// Merged rather than replaced, for the same reason `merge_into` exists: a node carries
    /// settings this dialog has no field for -- a font, a palette, a tab colour imported from
    /// `.mxtsessions` -- and building a fresh set from the form would throw them away.
    pub fn apply_settings(&self, settings: &mut bestterm_core_model::SettingsOverride) {
        settings.x11_forwarding = Some(self.fields.x11_forwarding);
        settings.backspace_sends_ctrl_h = Some(self.fields.backspace_sends_ctrl_h);
        settings.lock_terminal_title = Some(self.fields.lock_terminal_title);
        settings.reconnection_message = Some(self.fields.reconnection_message);
        settings.terminal_type = TERMINAL_TYPES
            .get(self.fields.terminal_type)
            .map(|term| (*term).to_owned());
        settings.log_session = Some(self.fields.log_output);
        let path = self.fields.log_path.trim();
        settings.log_path = (!path.is_empty()).then(|| path.to_owned());
    }

    /// Fill the dialog's settings fields from a node's.
    pub fn load_settings(&mut self, settings: &bestterm_core_model::SettingsOverride) {
        if let Some(value) = settings.x11_forwarding {
            self.fields.x11_forwarding = value;
        }
        if let Some(value) = settings.backspace_sends_ctrl_h {
            self.fields.backspace_sends_ctrl_h = value;
        }
        if let Some(value) = settings.lock_terminal_title {
            self.fields.lock_terminal_title = value;
        }
        if let Some(value) = settings.reconnection_message {
            self.fields.reconnection_message = value;
        }
        if let Some(term) = &settings.terminal_type
            && let Some(index) = TERMINAL_TYPES.iter().position(|known| known == term)
        {
            self.fields.terminal_type = index;
        }
        if let Some(value) = settings.log_session {
            self.fields.log_output = value;
        }
        if let Some(path) = &settings.log_path {
            self.fields.log_path = path.clone();
        }
    }

    /// Build the session the fields describe.
    fn build(&self) -> DialogOutcome {
        let host = self.fields.host.trim();
        let user = self.fields.user.trim();
        let port = self.fields.port.trim().parse::<u16>().ok();

        // The port is only defaulted when the protocol has one, so a protocol with no port field
        // cannot fail this.
        let port = match (port, self.protocol.default_port()) {
            (Some(port), _) => port,
            (None, Some(default)) => default,
            (None, None) => 0,
        };

        let needs_host = matches!(
            self.protocol,
            DialogProtocol::Ssh
                | DialogProtocol::Telnet
                | DialogProtocol::Rsh
                | DialogProtocol::Rdp
                | DialogProtocol::Vnc
                | DialogProtocol::Ftp
                | DialogProtocol::Sftp
                | DialogProtocol::Mosh
        );
        if needs_host && host.is_empty() {
            return DialogOutcome::Incomplete {
                field: "Remote host",
            };
        }

        let optional_user = (!user.is_empty()).then(|| user.to_owned());

        match self.protocol {
            DialogProtocol::Ssh => {
                let command = self.fields.execute_command.trim();
                let key = self.fields.private_key.trim();
                DialogOutcome::Accepted(Box::new(ProtocolConfig::Ssh(SshConfig {
                    host: host.to_owned(),
                    port,
                    user: optional_user,
                    // A key only when the box is ticked *and* a path was given: a ticked box with
                    // nothing in it would produce a session that authenticates against an empty
                    // path, which fails in a way that reads as a broken key rather than a blank
                    // field.
                    auth: if self.fields.use_private_key && !key.is_empty() {
                        bestterm_core_model::SshAuth::PublicKey {
                            path: key.to_owned(),
                            passphrase: None,
                        }
                    } else {
                        bestterm_core_model::SshAuth::Agent
                    },
                    command: (!command.is_empty()).then(|| command.to_owned()),
                    keep_open_after_command: self.fields.keep_open_after_command,
                    compression: self.fields.compression,
                    proxy: self.proxy(),
                    ..SshConfig::default()
                })))
            }
            DialogProtocol::Telnet => {
                DialogOutcome::Accepted(Box::new(ProtocolConfig::Telnet(TelnetConfig {
                    host: host.to_owned(),
                    port,
                })))
            }
            DialogProtocol::Rdp => {
                DialogOutcome::Accepted(Box::new(ProtocolConfig::Rdp(RdpConfig {
                    host: host.to_owned(),
                    port,
                    user: optional_user,
                    domain: (!self.fields.domain.trim().is_empty())
                        .then(|| self.fields.domain.trim().to_owned()),
                    clipboard: self.fields.rdp_clipboard,
                    multi_monitor: self.fields.rdp_multi_monitor,
                    ..RdpConfig::default()
                })))
            }
            DialogProtocol::Vnc => {
                DialogOutcome::Accepted(Box::new(ProtocolConfig::Vnc(VncConfig {
                    host: host.to_owned(),
                    port,
                    view_only: self.fields.vnc_view_only,
                    ..VncConfig::default()
                })))
            }
            DialogProtocol::Serial => {
                let device = self.fields.serial_port.trim();
                if device.is_empty() {
                    return DialogOutcome::Incomplete {
                        field: "Serial port",
                    };
                }
                // The speed is the one setting that is wrong when a console shows rubbish, so an
                // unreadable one is refused rather than quietly defaulted -- a session that silently
                // opened at 115200 when somebody typed 9600 would look like a broken cable.
                let Ok(baud) = self.fields.baud.trim().parse::<u32>() else {
                    return DialogOutcome::Incomplete {
                        field: "Speed (bps)",
                    };
                };
                DialogOutcome::Accepted(Box::new(ProtocolConfig::Serial(SerialConfig {
                    device: device.to_owned(),
                    baud,
                    // 8N1 with no flow control, which is what console cables are wired for. The
                    // dialog does not collect the rest yet; `docs/ui-parity.md` has not measured that
                    // tab, and inventing fields would put settings on screen the reference does not
                    // have.
                    ..SerialConfig::default()
                })))
            }
            // Shell and the rest need fields this dialog does not yet collect, or a model variant
            // that does not exist. Named individually so the message says which.
            other => DialogOutcome::Unsupported(other.label()),
        }
    }
}

/// Draw the dialog.
///
/// Returns nothing: the outcome is left on the dialog for [`SessionDialog::take_outcome`], so the
/// caller is not forced to handle it in the middle of drawing a frame.
pub fn session_dialog(ui: &mut Ui, theme: &ChromeTheme, dialog: &mut SessionDialog) {
    if !dialog.open {
        return;
    }

    // A title row with a close cross, not a floating window with a system title bar: the reference
    // docks this over the session area with the ribbon still visible above it.
    title_row(ui, theme, dialog);
    ui.add_space(4.0);
    protocol_strip(ui, theme, dialog);
    ui.add_space(6.0);

    group_box(ui, theme, dialog.protocol.group_name(), |ui| {
        basic_fields(ui, dialog);
    });

    ui.add_space(6.0);
    secondary_tab_row(ui, theme, dialog);
    ui.add_space(8.0);

    // Centred, as measured, and both carrying a glyph. `vertical_centered` centres the layout it
    // creates and not a horizontal row nested inside it, so the row is offset by hand.
    let buttons = 200.0;
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - buttons) / 2.0).max(0.0));
        // Plain words. `✓` and `✕` are not in egui's bundled font, so they drew as hollow boxes --
        // the same trap that put an empty square on every folder in the session tree. The reference
        // has a tick and a cross here; ours will too once the icons are drawn into buttons rather
        // than typed as text.
        if ui.button("  OK  ").clicked() {
            dialog.outcome = Some(dialog.build());
            dialog.open = false;
        }
        ui.add_space(12.0);
        if ui.button("Cancel").clicked() {
            dialog.outcome = Some(DialogOutcome::Cancelled);
            dialog.open = false;
        }
    });
}

fn title_row(ui: &mut Ui, theme: &ChromeTheme, dialog: &mut SessionDialog) {
    ui.horizontal(|ui| {
        ui.label("Session settings");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("x").clicked() {
                dialog.outcome = Some(DialogOutcome::Cancelled);
                dialog.open = false;
            }
        });
    });
    let line = ui.available_rect_before_wrap();
    ui.painter().hline(
        line.x_range(),
        line.top(),
        Stroke::new(1.0, theme.separator),
    );
}

/// One row of fifteen tabs, icon above label. It does not scroll or wrap in the reference.
fn protocol_strip(ui: &mut Ui, theme: &ChromeTheme, dialog: &mut SessionDialog) {
    ui.horizontal_wrapped(|ui| {
        for protocol in DialogProtocol::ALL {
            let selected = dialog.protocol == protocol;
            if protocol_tab(ui, theme, protocol.label(), selected).clicked() {
                dialog.protocol = protocol;
                dialog.set_port_for_protocol();
            }
        }
    });
}

/// Which icon a protocol tab carries.
///
/// Beside the tab that draws it, and keyed on the label so the list is one place. Anything without
/// a picture of its own gets the terminal, which is what the protocols without one are.
fn dialog_icon(label: &str) -> crate::icons::Icon {
    use crate::icons::Icon;
    match label {
        "SSH" => Icon::Ssh,
        "Telnet" | "Rsh" | "Mosh" => Icon::Session,
        "Xdmcp" => Icon::X11,
        "RDP" => Icon::Rdp,
        "VNC" => Icon::Vnc,
        "FTP" | "SFTP" => Icon::Folder,
        "Serial" => Icon::Toolbar,
        "File" => Icon::File,
        "Shell" | "WSL" => Icon::Session,
        "Browser" => Icon::Help,
        "Aws S3" => Icon::Packages,
        _ => Icon::Session,
    }
}

fn protocol_tab(ui: &mut Ui, theme: &ChromeTheme, label: &str, selected: bool) -> egui::Response {
    let width = 52.0;
    let height = 52.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, CornerRadius::ZERO, theme.selected_bg);
            painter.rect_stroke(
                rect,
                CornerRadius::ZERO,
                Stroke::new(1.0, theme.border),
                egui::StrokeKind::Inside,
            );
        } else if response.hovered() {
            painter.rect_filled(rect, CornerRadius::ZERO, theme.hover_bg);
        }

        // The real icon. This drew a hollow square until now -- a leftover from before the icon set
        // existed -- so all fifteen tabs looked like empty checkboxes above their labels.
        let side = 20.0;
        let icon = Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 6.0 + side / 2.0),
            vec2(side, side),
        );
        crate::icons::draw(painter, icon, dialog_icon(label));
        painter.text(
            egui::pos2(rect.center().x, icon.bottom() + 3.0),
            Align2::CENTER_TOP,
            label,
            egui::TextStyle::Small.resolve(ui.style()),
            theme.text,
        );
    }

    response
}

/// A bordered box with its name in a tab at the top left, as the reference draws its settings groups.
fn group_box(ui: &mut Ui, theme: &ChromeTheme, name: &str, contents: impl FnOnce(&mut Ui)) {
    ui.label(egui::RichText::new(name).small());
    egui::Frame::NONE
        .stroke(Stroke::new(1.0, theme.border))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| contents(ui));
}

fn basic_fields(ui: &mut Ui, dialog: &mut SessionDialog) {
    let protocol = dialog.protocol;
    let fields = &mut dialog.fields;

    ui.horizontal(|ui| match protocol {
        DialogProtocol::Xdmcp => {
            ui.radio_value(&mut fields.xdmcp_specific, false, "Connect to any server");
            ui.add_space(12.0);
            ui.radio_value(
                &mut fields.xdmcp_specific,
                true,
                "Specify server to connect to:",
            );
            ui.add_enabled_ui(fields.xdmcp_specific, |ui| {
                ui.text_edit_singleline(&mut fields.host);
            });
        }
        DialogProtocol::Serial => {
            ui.label("Serial port *");
            ui.text_edit_singleline(&mut fields.serial_port);
            ui.add_space(12.0);
            ui.label("Speed (bps) *");
            ui.text_edit_singleline(&mut fields.baud);
        }
        DialogProtocol::File => {
            ui.label("File/folder to open *");
            ui.text_edit_singleline(&mut fields.path);
        }
        DialogProtocol::Shell => {
            ui.label("Terminal shell");
            let selected = SHELL_CHOICES
                .get(fields.shell_choice)
                .copied()
                .unwrap_or("Cmd");
            egui::ComboBox::from_id_salt("bestterm_shell_choice")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for (index, choice) in SHELL_CHOICES.iter().enumerate() {
                        ui.selectable_value(&mut fields.shell_choice, index, *choice);
                    }
                });
            ui.add_space(12.0);
            ui.label("Startup directory");
            ui.text_edit_singleline(&mut fields.startup_directory);
        }
        DialogProtocol::Browser => {
            ui.label("URL *");
            ui.text_edit_singleline(&mut fields.url);
        }
        DialogProtocol::AwsS3 => {
            ui.label("Key ID *");
            ui.text_edit_singleline(&mut fields.key_id);
        }
        DialogProtocol::Wsl => {
            ui.label("Distribution");
            ui.text_edit_singleline(&mut fields.distribution);
            ui.add_space(12.0);
            ui.label("Username");
            ui.text_edit_singleline(&mut fields.user);
        }
        DialogProtocol::Rdp => {
            ui.label("Remote host *");
            ui.text_edit_singleline(&mut fields.host);
            ui.add_space(12.0);
            ui.label("Username");
            ui.text_edit_singleline(&mut fields.user);
            ui.add_space(12.0);
            // The one field a domain account cannot connect without, and it had nowhere to be
            // typed: it was loaded from a session and written back, so an imported session kept
            // its domain and no one could change it.
            ui.label("Domain");
            ui.add(egui::TextEdit::singleline(&mut fields.domain).desired_width(90.0));
            ui.add_space(12.0);
            ui.label("Port");
            ui.add(egui::TextEdit::singleline(&mut fields.port).desired_width(52.0));
        }
        DialogProtocol::Vnc => {
            ui.label("Remote hostname or IP address *");
            ui.text_edit_singleline(&mut fields.host);
            ui.add_space(12.0);
            ui.label("Port");
            ui.add(egui::TextEdit::singleline(&mut fields.port).desired_width(52.0));
        }
        // Everything else takes a host, a user, and a port where the protocol has one.
        other => {
            ui.label("Remote host *");
            ui.text_edit_singleline(&mut fields.host);
            ui.add_space(12.0);
            ui.label("Username");
            ui.text_edit_singleline(&mut fields.user);
            if other.default_port().is_some() {
                ui.add_space(12.0);
                ui.label("Port");
                ui.add(egui::TextEdit::singleline(&mut fields.port).desired_width(52.0));
            }
        }
    });
}

/// The row of lower tabs, and whichever one is selected.
fn secondary_tab_row(ui: &mut Ui, theme: &ChromeTheme, dialog: &mut SessionDialog) {
    let protocol = dialog.protocol;
    // The advanced tab is named after the protocol, which is why `secondary_tabs` does not list it.
    let advanced = format!("Advanced {} settings", advanced_name(protocol));

    ui.horizontal(|ui| {
        if ui
            .selectable_label(dialog.secondary == SecondaryTab::Advanced, advanced)
            .clicked()
        {
            dialog.secondary = SecondaryTab::Advanced;
        }
        for tab in protocol.secondary_tabs() {
            if ui
                .selectable_label(dialog.secondary == *tab, tab.label())
                .clicked()
            {
                dialog.secondary = *tab;
            }
        }
        if ui
            .selectable_label(
                dialog.secondary == SecondaryTab::Bookmark,
                SecondaryTab::Bookmark.label(),
            )
            .clicked()
        {
            dialog.secondary = SecondaryTab::Bookmark;
        }
    });

    // A tab the current protocol does not have cannot stay selected across a protocol change.
    let available = protocol.secondary_tabs();
    if !matches!(
        dialog.secondary,
        SecondaryTab::Advanced | SecondaryTab::Bookmark
    ) && !available.contains(&dialog.secondary)
    {
        dialog.secondary = SecondaryTab::Advanced;
    }

    ui.add_space(6.0);
    group_box(ui, theme, "", |ui| {
        ui.set_min_height(180.0);
        match dialog.secondary {
            SecondaryTab::Advanced => advanced_tab(ui, theme, dialog),
            SecondaryTab::Terminal => terminal_tab(ui, theme, &mut dialog.fields),
            SecondaryTab::Network => network_tab(ui, theme, &mut dialog.fields),
            SecondaryTab::Bookmark => bookmark_tab(ui, theme, &mut dialog.fields),
        }
    });
}

/// The advanced tab, which is per-protocol.
///
/// Only SSH is measured. The others get a line saying so rather than SSH's fields under another
/// protocol's name, which would be a form that lies about what it sets.
fn advanced_tab(ui: &mut Ui, theme: &ChromeTheme, dialog: &mut SessionDialog) {
    if dialog.protocol == DialogProtocol::Rdp {
        rdp_advanced_tab(ui, theme, &mut dialog.fields);
        return;
    }
    if dialog.protocol == DialogProtocol::Vnc {
        vnc_advanced_tab(ui, theme, &mut dialog.fields);
        return;
    }
    if dialog.protocol != DialogProtocol::Ssh {
        // What the reference itself shows in an advanced tab it has nothing to put in: the protocol's
        // name and its icon, filling the space. Measured from the RDP tab, which looks exactly like
        // this.
        description_area(ui, theme, dialog.protocol);
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!(
                "The advanced {} settings have not been measured from the reference yet.",
                advanced_name(dialog.protocol)
            ))
            .small()
            .color(theme.text_dim),
        );
        return;
    }
    let fields = &mut dialog.fields;

    ui.horizontal(|ui| {
        ui.checkbox(&mut fields.x11_forwarding, "X11-Forwarding");
        ui.add_space(16.0);
        ui.checkbox(&mut fields.compression, "Compression");
        ui.add_space(16.0);
        ui.label("Remote environment:");
        choice(
            ui,
            "remote-env",
            &mut fields.remote_environment,
            REMOTE_ENVIRONMENTS,
            150.0,
        );
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label("Execute command:");
        ui.add(egui::TextEdit::singleline(&mut fields.execute_command).desired_width(210.0));
        ui.add_space(16.0);
        ui.checkbox(
            &mut fields.keep_open_after_command,
            "Do not exit after command ends",
        );
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label("SSH-browser type:");
        choice(
            ui,
            "browser-type",
            &mut fields.browser_type,
            BROWSER_TYPES,
            210.0,
        );
        ui.add_space(16.0);
        ui.checkbox(
            &mut fields.follow_ssh_path,
            "Try to follow SSH path in browser",
        );
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.checkbox(&mut fields.use_private_key, "Use private key");
        // Editable whether or not the box is ticked, so a path can be typed before it is turned on --
        // which is the order people do it in.
        ui.add(egui::TextEdit::singleline(&mut fields.private_key).desired_width(210.0));
        if ui.button("Browse…").clicked()
            && let Some(path) = pick_private_key(&fields.private_key)
        {
            fields.private_key = path;
            // Choosing a key is choosing to use one. Leaving the box unticked after somebody went
            // and found the file would be a form that ignored what they just did.
            fields.use_private_key = true;
        }
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label("Execute macro at session start:");
        choice(
            ui,
            "macro",
            &mut fields.macro_at_start,
            MACRO_CHOICES,
            210.0,
        );
    });

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "X11 forwarding, the command, the key and compression are saved and used. The browser \
             type, the remote environment and the macro are saved and not yet acted on.",
        )
        .small()
        .color(theme.text_dim),
    );
}

/// The terminal tab.
/// Advanced VNC settings.
///
/// One control, and it is the one that matters most: the importer has been reading `view only` out
/// of .mxtsessions since it was written, and nothing could see or change it. The reference's tab
/// also has scaling, colour depth and an encoding list; those wait until the helper can be told
/// about them.
fn vnc_advanced_tab(ui: &mut Ui, theme: &ChromeTheme, fields: &mut SessionFields) {
    ui.checkbox(
        &mut fields.vnc_view_only,
        "View only — do not send keyboard or mouse to the remote desktop",
    );

    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "Acted on: nothing typed or clicked leaves this machine, and the status bar says so \
             while the session is open.",
        )
        .small()
        .color(theme.text_dim),
    );
}

/// Advanced RDP settings.
///
/// Not measured against the reference, which has a much larger tab -- display resolution, sound,
/// device redirection, gateways, a program to start. What is here is what the session model
/// carries, so every control changes something that is saved. The reference's remaining fields
/// arrive when the helper can act on them; a checkbox for a feature that does not exist would be
/// the fake tab this replaces.
fn rdp_advanced_tab(ui: &mut Ui, theme: &ChromeTheme, fields: &mut SessionFields) {
    ui.checkbox(
        &mut fields.rdp_clipboard,
        "Share the local clipboard with the remote session",
    );
    ui.add_space(6.0);
    ui.checkbox(
        &mut fields.rdp_multi_monitor,
        "Span the session across all local monitors",
    );

    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "Both are saved with the session and neither is acted on yet: the helper has no \
              clipboard channel and opens one monitor's worth of desktop. The domain is on the \
              first row, and it is used.",
        )
        .small()
        .color(theme.text_dim),
    );
}

fn terminal_tab(ui: &mut Ui, theme: &ChromeTheme, fields: &mut SessionFields) {
    ui.horizontal(|ui| {
        for label in ["Font settings", "Color settings", "Expert settings"] {
            let _ = ui
                .button(label)
                .on_hover_text("Not measured from the reference yet");
        }
    });
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        ui.checkbox(&mut fields.backspace_sends_ctrl_h, "Backspace sends ^H");
        ui.add_space(16.0);
        ui.checkbox(&mut fields.use_windows_path, "Use Windows PATH");
        ui.add_space(16.0);
        ui.label("Terminal type:");
        choice(
            ui,
            "term-type",
            &mut fields.terminal_type,
            TERMINAL_TYPES,
            130.0,
        );
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.checkbox(&mut fields.log_output, "Log terminal output to:");
        ui.add(egui::TextEdit::singleline(&mut fields.log_path).desired_width(210.0));
        if ui.button("Browse…").clicked()
            && let Some(path) = pick_log_file(&fields.log_path)
        {
            fields.log_path = path;
            fields.log_output = true;
        }
        ui.add_space(16.0);
        ui.label("Paste delay:");
        choice(
            ui,
            "paste-delay",
            &mut fields.paste_delay,
            PASTE_DELAYS,
            90.0,
        );
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label("Syntax highlighting:");
        choice(
            ui,
            "highlighting",
            &mut fields.highlighting,
            HIGHLIGHTING,
            260.0,
        );
        let _ = ui
            .button("Customize")
            .on_hover_text("Not measured from the reference yet");
    });

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "The terminal type, the log and the Backspace behaviour are saved. Paste delay and \
             highlighting are saved and not yet acted on.",
        )
        .small()
        .color(theme.text_dim),
    );
}

/// The network tab.
fn network_tab(ui: &mut Ui, theme: &ChromeTheme, fields: &mut SessionFields) {
    let _ = ui
        .button("    SSH gateway (jump host)")
        .on_hover_text("Jump hosts are in the model and have no editor yet");
    ui.add_space(10.0);

    group_box(ui, theme, "Proxy settings (experimental)", |ui| {
        ui.horizontal(|ui| {
            ui.label("Proxy type:");
            let kinds: Vec<&'static str> = bestterm_core_model::ProxyKind::ALL
                .iter()
                .map(|kind| kind.label())
                .collect();
            choice(ui, "proxy-type", &mut fields.proxy_type, &kinds, 130.0);
            ui.add_space(12.0);
            ui.label("Host:");
            ui.add(egui::TextEdit::singleline(&mut fields.proxy_host).desired_width(110.0));
            ui.add_space(12.0);
            ui.label("Login:");
            ui.add(egui::TextEdit::singleline(&mut fields.proxy_login).desired_width(90.0));
            ui.add_space(12.0);
            ui.label("Port:");
            ui.add(egui::TextEdit::singleline(&mut fields.proxy_port).desired_width(60.0));
        });
    });

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "A proxy is saved with the session. Nothing routes through one yet, which is why the \
             reference calls its own version experimental and this one says so outright.",
        )
        .small()
        .color(theme.text_dim),
    );
}

/// The bookmark tab.
fn bookmark_tab(ui: &mut Ui, theme: &ChromeTheme, fields: &mut SessionFields) {
    egui::Grid::new("bookmark")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Session name:");
            ui.horizontal(|ui| {
                // The field that made editing possible at all. Left blank it falls back to the
                // address, which is what the reference shows for a session nobody named.
                ui.add(
                    egui::TextEdit::singleline(&mut fields.session_name)
                        .hint_text("the address")
                        .desired_width(160.0),
                );
                ui.add_space(12.0);
                ui.checkbox(&mut fields.lock_terminal_title, "Lock terminal title");
                ui.add_space(12.0);
                let _ = ui
                    .button("Session Icon")
                    .on_hover_text("Icons are imported and cannot be chosen here yet");
            });
            ui.end_row();

            ui.label("Start session in");
            ui.horizontal(|ui| {
                choice(ui, "start-in", &mut fields.start_in, START_IN, 150.0);
                ui.add_space(12.0);
                ui.checkbox(
                    &mut fields.reconnection_message,
                    "Display reconnection message at session end",
                );
            });
            ui.end_row();

            ui.checkbox(&mut fields.customize_tab_color, "Customize tab color");
            ui.horizontal(|ui| {
                ui.label("Comments:");
                ui.add(egui::TextEdit::singleline(&mut fields.comments).desired_width(240.0));
            });
            ui.end_row();
        });

    ui.add_space(8.0);
    let _ = ui
        .button("Create a desktop shortcut to this session")
        .on_hover_text("Not implemented yet");

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "The name, the comment and the title lock are saved. Splits and separate windows do not \
             exist yet, so `Start session in` is saved and ignored.",
        )
        .small()
        .color(theme.text_dim),
    );
}

/// Ask for a private key file.
///
/// The platform's own dialog, through `rfd`. Blocking on purpose: the interface is already stopped
/// while a modal is open, and a picker that returned later would have to find its way back to a
/// field that may no longer be on screen.
///
/// The filter names the formats people actually have. `.ppk` is PuTTY's, which is what an imported
/// MobaXterm session points at — and `proto-ssh` reads OpenSSH keys, so a `.ppk` will be refused
/// later with a message about the format rather than silently.
fn pick_private_key(current: &str) -> Option<String> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("Choose a private key")
        .add_filter(
            "Private keys",
            &["ppk", "pem", "key", "id_rsa", "id_ed25519"],
        )
        .add_filter("All files", &["*"]);
    if let Some(directory) = starting_directory(current) {
        dialog = dialog.set_directory(directory);
    }
    dialog
        .pick_file()
        .map(|path| path.to_string_lossy().into_owned())
}

/// Ask where a transcript should go.
///
/// A save dialog rather than an open one: the file does not exist yet, and an open dialog would
/// refuse to hand back a name that is not already there.
fn pick_log_file(current: &str) -> Option<String> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("Where to write the transcript")
        .add_filter("Text files", &["txt", "log"])
        .add_filter("All files", &["*"]);
    if let Some(directory) = starting_directory(current) {
        dialog = dialog.set_directory(directory);
    }
    dialog
        .save_file()
        .map(|path| path.to_string_lossy().into_owned())
}

/// Where a picker should open, given whatever is in the field.
///
/// The field's own directory when it names one that exists, so a second visit starts where the
/// first left off. `None` lets the platform decide, which is better than starting somewhere
/// arbitrary of ours.
fn starting_directory(current: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(current.trim());
    if current.trim().is_empty() {
        return None;
    }
    let directory = if path.is_dir() { path } else { path.parent()? };
    directory.is_dir().then(|| directory.to_path_buf())
}

/// A dropdown over a fixed list of labels, selected by index.
///
/// An index rather than an enum per list: the lists are measured strings, several of them name things
/// this build cannot do yet, and a value outside what it understands has to survive being loaded and
/// saved rather than collapse to the first entry.
fn choice(ui: &mut Ui, id: &str, selected: &mut usize, options: &[&str], width: f32) {
    let label = options.get(*selected).copied().unwrap_or("");
    egui::ComboBox::from_id_salt(id)
        .selected_text(label)
        .width(width)
        .show_ui(ui, |ui| {
            for (index, option) in options.iter().enumerate() {
                ui.selectable_value(selected, index, *option);
            }
        });
}
fn advanced_name(protocol: DialogProtocol) -> &'static str {
    match protocol {
        DialogProtocol::Rdp => "Rdp",
        DialogProtocol::Vnc => "Vnc",
        DialogProtocol::Ftp => "Ftp",
        DialogProtocol::Sftp => "Sftp",
        DialogProtocol::File => "File/folder",
        DialogProtocol::AwsS3 => "Aws S3 (experimental)",
        other => other.label(),
    }
}

fn description_area(ui: &mut Ui, theme: &ChromeTheme, protocol: DialogProtocol) {
    egui::Frame::NONE
        .stroke(Stroke::new(1.0, theme.border))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_height(80.0);
            ui.horizontal(|ui| {
                ui.label(protocol.description());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
                    if ui.is_rect_visible(rect) {
                        ui.painter().rect_stroke(
                            rect,
                            CornerRadius::ZERO,
                            Stroke::new(1.0, theme.text_dim),
                            egui::StrokeKind::Inside,
                        );
                    }
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dialog on an SSH session, for the round-trip tests below.
    fn ssh_dialog() -> SessionDialog {
        let mut dialog = SessionDialog::default();
        dialog.open_for(&ProtocolConfig::Ssh(SshConfig {
            host: "srv.int".to_string(),
            port: 2222,
            user: Some("ops".to_string()),
            auth: bestterm_core_model::SshAuth::PublicKey {
                path: r"D:\keys\ops.ppk".to_string(),
                passphrase: None,
            },
            ..SshConfig::default()
        }));
        dialog
    }

    #[test]
    fn a_private_key_survives_being_loaded_and_saved() {
        // The field the whole Advanced tab was needed for. Before it, opening a session in the dialog
        // and pressing OK replaced its key with the agent -- silently, and on a machine where the
        // agent is not running.
        let dialog = ssh_dialog();
        assert!(dialog.fields.use_private_key);
        assert_eq!(dialog.fields.private_key, r"D:\keys\ops.ppk");

        let DialogOutcome::Accepted(produced) = dialog.build() else {
            panic!("a complete form is accepted");
        };
        let ProtocolConfig::Ssh(ssh) = produced.as_ref() else {
            panic!("ssh");
        };
        assert_eq!(
            ssh.auth,
            bestterm_core_model::SshAuth::PublicKey {
                path: r"D:\keys\ops.ppk".to_string(),
                passphrase: None
            }
        );
    }

    #[test]
    fn a_ticked_box_with_no_path_is_not_a_key() {
        // It would produce a session authenticating against an empty path, which fails in a way that
        // reads as a broken key rather than a blank field.
        let mut dialog = ssh_dialog();
        dialog.fields.private_key.clear();
        let DialogOutcome::Accepted(produced) = dialog.build() else {
            panic!("accepted");
        };
        let ProtocolConfig::Ssh(ssh) = produced.as_ref() else {
            panic!("ssh");
        };
        assert_eq!(ssh.auth, bestterm_core_model::SshAuth::Agent);
    }

    #[test]
    fn a_merge_never_replaces_an_auth_method_the_dialog_cannot_show() {
        // The rule the merge exists for. A session authenticating with a vault password has to keep
        // doing so after somebody corrects its port, because the dialog has no password field and
        // whatever it produced for `auth` is a default rather than a choice.
        let mut existing = ProtocolConfig::Ssh(SshConfig {
            host: "srv.int".to_string(),
            auth: bestterm_core_model::SshAuth::Password { credential: None },
            ..SshConfig::default()
        });
        let produced = ProtocolConfig::Ssh(SshConfig {
            host: "srv.int".to_string(),
            port: 2200,
            ..SshConfig::default()
        });

        SessionDialog::merge_into(produced, &mut existing);
        let ProtocolConfig::Ssh(ssh) = &existing else {
            panic!("ssh");
        };
        assert_eq!(ssh.port, 2200, "the port was the edit");
        assert_eq!(
            ssh.auth,
            bestterm_core_model::SshAuth::Password { credential: None },
            "and the password survived it"
        );
    }

    #[test]
    fn clearing_the_key_box_does_go_back_to_the_agent() {
        // The other direction, which has to work or the box is one-way: a session with a key whose box
        // is unticked is somebody choosing the agent, and that is a choice.
        let mut existing = ProtocolConfig::Ssh(SshConfig {
            auth: bestterm_core_model::SshAuth::PublicKey {
                path: "old".to_string(),
                passphrase: None,
            },
            ..SshConfig::default()
        });
        SessionDialog::merge_into(ProtocolConfig::Ssh(SshConfig::default()), &mut existing);
        let ProtocolConfig::Ssh(ssh) = &existing else {
            panic!("ssh");
        };
        assert_eq!(ssh.auth, bestterm_core_model::SshAuth::Agent);
    }

    #[test]
    fn a_session_name_is_trimmed_and_an_empty_one_is_no_name_at_all() {
        // Blank means "leave it as it is", not "call it nothing": a nameless row in a tree of five
        // hundred is a session somebody has lost.
        let mut dialog = SessionDialog::default();
        assert_eq!(dialog.session_name(), None);
        dialog.fields.session_name = "   ".to_string();
        assert_eq!(dialog.session_name(), None);
        dialog.fields.session_name = "  db106  ".to_string();
        assert_eq!(dialog.session_name(), Some("db106".to_string()));
    }

    #[test]
    fn the_settings_the_tabs_collect_reach_a_node_and_leave_the_rest_alone() {
        // Merged, not replaced. A node carries settings this dialog has no field for -- a font, a
        // palette, a tab colour that came in with an import -- and building a fresh set from the form
        // would throw them away.
        let mut settings = bestterm_core_model::SettingsOverride {
            font_family: Some("Consolas".to_string()),
            tab_color: Some([1, 2, 3]),
            ..Default::default()
        };

        let mut dialog = SessionDialog::default();
        dialog.fields.x11_forwarding = false;
        dialog.fields.terminal_type = 1;
        dialog.fields.log_output = true;
        dialog.fields.log_path = "  D:/logs/session.txt ".to_string();
        dialog.apply_settings(&mut settings);

        assert_eq!(settings.x11_forwarding, Some(false));
        assert_eq!(settings.terminal_type.as_deref(), Some(TERMINAL_TYPES[1]));
        assert_eq!(settings.log_session, Some(true));
        assert_eq!(settings.log_path.as_deref(), Some("D:/logs/session.txt"));
        assert_eq!(
            settings.font_family.as_deref(),
            Some("Consolas"),
            "a setting the dialog cannot show must survive it"
        );
        assert_eq!(settings.tab_color, Some([1, 2, 3]));
    }

    #[test]
    fn settings_round_trip_through_the_dialog() {
        let mut settings = bestterm_core_model::SettingsOverride::default();
        let mut out = SessionDialog::default();
        out.fields.x11_forwarding = false;
        out.fields.backspace_sends_ctrl_h = false;
        out.fields.terminal_type = 2;
        out.apply_settings(&mut settings);

        let mut back = SessionDialog::default();
        back.load_settings(&settings);
        assert!(!back.fields.x11_forwarding);
        assert!(!back.fields.backspace_sends_ctrl_h);
        assert_eq!(back.fields.terminal_type, 2);
    }

    #[test]
    fn a_proxy_is_only_built_when_one_was_chosen() {
        let mut dialog = SessionDialog::default();
        assert!(dialog.proxy().is_none(), "None means a direct connection");

        // Index 2 is Socks5, per the measured order.
        dialog.fields.proxy_type = 2;
        dialog.fields.proxy_host = " gate.int ".to_string();
        let proxy = dialog.proxy().expect("a proxy was chosen");
        assert_eq!(proxy.kind, bestterm_core_model::ProxyKind::Socks5);
        assert_eq!(proxy.host, "gate.int");
        assert_eq!(proxy.port, 1080);
        assert_eq!(proxy.login, None, "an empty login is no login");
    }

    #[test]
    fn the_measured_dropdowns_are_not_empty_and_their_defaults_are_the_reference_s() {
        // A dropdown with nothing in it draws as a blank box, which reads as a broken control.
        for list in [
            REMOTE_ENVIRONMENTS,
            BROWSER_TYPES,
            MACRO_CHOICES,
            TERMINAL_TYPES,
            PASTE_DELAYS,
            HIGHLIGHTING,
            START_IN,
        ] {
            assert!(!list.is_empty());
        }

        // What the reference has ticked and selected on a fresh dialog.
        let fields = SessionFields::default();
        assert!(fields.x11_forwarding);
        assert!(fields.compression);
        assert!(fields.backspace_sends_ctrl_h);
        assert!(fields.use_windows_path);
        assert!(fields.lock_terminal_title);
        assert!(fields.reconnection_message);
        assert_eq!(
            REMOTE_ENVIRONMENTS[fields.remote_environment],
            "Interactive shell"
        );
        assert_eq!(BROWSER_TYPES[fields.browser_type], "SFTP protocol");
        assert_eq!(TERMINAL_TYPES[fields.terminal_type], "xterm");
        assert_eq!(PASTE_DELAYS[fields.paste_delay], "Auto");
        assert_eq!(START_IN[fields.start_in], "Normal tab");
        assert_eq!(fields.proxy_port, "1080");
    }

    #[test]
    fn all_fifteen_protocol_tabs_are_laid_out_inside_the_dialog() {
        // The bug this exists for was visible and I did not see it: the strip drew one tab of fifteen,
        // because the dialog was wrapped in a vertical scroll area that reported an unbounded width to
        // the wrapped row. Fourteen tabs were laid out past the right edge of the window.
        //
        // Checked as geometry rather than by eye: every tab has to land inside the width it was given.
        let ctx = egui::Context::default();
        let theme = ChromeTheme::light();
        let mut dialog = SessionDialog {
            open: true,
            ..SessionDialog::default()
        };

        let width = 886.0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, 589.0),
                )),
                ..Default::default()
            },
            |ui| {
                session_dialog(ui, &theme, &mut dialog);
            },
        );
        output.textures_delta.clear();

        // Every rectangle the frame emitted, so a tab pushed off the edge is caught wherever it went.
        let mut widest = 0.0f32;
        for clipped in &output.shapes {
            let rect = clipped.shape.visual_bounding_rect();
            if rect.is_finite() && rect.is_positive() {
                widest = widest.max(rect.right());
            }
        }
        assert!(
            widest <= width + 1.0,
            "something was drawn {widest} across a {width} dialog, which is how fourteen protocol \
             tabs ended up outside the window"
        );
    }

    #[test]
    fn every_protocol_tab_has_an_icon_of_its_own_kind() {
        // It drew a hollow placeholder square for every one of the fifteen until now, so they all looked
        // like empty checkboxes above their labels.
        for protocol in DialogProtocol::ALL {
            let icon = dialog_icon(protocol.label());
            // Not a specific icon per protocol -- several genuinely share one, and a terminal is what a
            // protocol without a picture of its own is -- but the two that have pictures must use them.
            match protocol {
                DialogProtocol::Rdp => assert_eq!(icon, crate::icons::Icon::Rdp),
                DialogProtocol::Vnc => assert_eq!(icon, crate::icons::Icon::Vnc),
                DialogProtocol::Ssh => assert_eq!(icon, crate::icons::Icon::Ssh),
                _ => {}
            }
        }
    }

    #[test]
    fn every_label_the_dialog_shows_is_one_the_bundled_font_can_draw() {
        // `✓`, `✕` and `★` are not in egui's bundled font and drew as hollow boxes — the same trap
        // that put an empty square on every folder in the session tree earlier in this project.
        //
        // Checked on the strings themselves. The first version of this searched this file's own source
        // and tripped on the comment above, which has to name the glyphs to talk about them — exactly
        // how the artwork test in `icons.rs` failed twice before it was deleted. A rule about what is
        // in a font is a rule about values, not about text.
        let mut labels: Vec<String> = Vec::new();
        labels.extend(DialogProtocol::ALL.iter().map(|p| p.label().to_string()));
        labels.extend(
            DialogProtocol::ALL
                .iter()
                .map(|p| p.group_name().to_string()),
        );
        labels.extend(
            DialogProtocol::ALL
                .iter()
                .map(|p| p.description().to_string()),
        );
        labels.extend(
            [
                SecondaryTab::Advanced,
                SecondaryTab::Terminal,
                SecondaryTab::Network,
                SecondaryTab::Bookmark,
            ]
            .iter()
            .map(|tab| tab.label().to_string()),
        );
        for list in [
            REMOTE_ENVIRONMENTS,
            BROWSER_TYPES,
            MACRO_CHOICES,
            TERMINAL_TYPES,
            PASTE_DELAYS,
            HIGHLIGHTING,
            START_IN,
            SHELL_CHOICES,
        ] {
            labels.extend(list.iter().map(|entry| (*entry).to_string()));
        }

        for label in labels {
            for character in label.chars() {
                // Latin-1 and the handful of punctuation the bundled font covers. Anything outside it
                // is a box on screen, and a box beside a label reads as a broken control.
                assert!(
                    character.is_ascii() || (0xA0..=0xFF).contains(&(character as u32)),
                    "{character:?} in {label:?} is outside what the bundled font covers"
                );
            }
        }
    }
    #[test]
    fn a_picker_opens_where_the_field_already_points() {
        // So a second visit starts where the first left off, rather than somewhere arbitrary of ours.
        let temp = std::env::temp_dir();
        let inside = temp.join("a-key-that-need-not-exist.ppk");
        assert_eq!(
            starting_directory(&inside.to_string_lossy()),
            Some(temp.clone()),
            "the file's directory"
        );
        assert_eq!(
            starting_directory(&temp.to_string_lossy()),
            Some(temp),
            "a directory is its own starting point"
        );

        // Nothing typed, and a path whose directory does not exist: let the platform decide.
        assert_eq!(starting_directory("   "), None);
        assert_eq!(starting_directory(""), None);
        assert_eq!(starting_directory("Z:/no/such/place/key.ppk"), None);
    }

    #[test]
    fn editing_a_session_keeps_what_the_dialog_cannot_show() {
        // Tunnels and bastion chains have no fields in this dialog, so the merge leaves them alone.
        // That is right, and it is only true by omission -- which is how the RDP clipboard flag came
        // to be silently reset the moment a checkbox for it appeared. Pinned here so that adding a
        // field for one of these has to be a deliberate change to a failing test rather than a
        // discovery made by somebody whose port forward disappeared after they renamed a session.
        let bastion = bestterm_core_model::NodeId::new();
        let forward = bestterm_core_model::PortForward {
            kind: bestterm_core_model::ForwardKind::Local,
            bind_address: "127.0.0.1".to_owned(),
            bind_port: 15432,
            target_host: Some("a-database".to_owned()),
            target_port: Some(5432),
            auto_open: true,
        };
        let mut target = ProtocolConfig::Ssh(SshConfig {
            host: "old.invalid".to_owned(),
            port: 22,
            jump_hosts: vec![bastion],
            forwards: vec![forward.clone()],
            ..SshConfig::default()
        });

        let mut dialog = SessionDialog::default();
        dialog.open_for(&target);
        dialog.fields.host = "new.invalid".to_owned();
        match dialog.build() {
            DialogOutcome::Accepted(produced) => SessionDialog::merge_into(*produced, &mut target),
            other => panic!("the dialog refused a complete SSH session: {other:?}"),
        }

        let ProtocolConfig::Ssh(ssh) = target else {
            panic!("an SSH session stopped being one")
        };
        assert_eq!(ssh.host, "new.invalid", "the edit still lands");
        assert_eq!(ssh.jump_hosts, vec![bastion], "the bastion chain survives");
        assert_eq!(ssh.forwards, vec![forward], "and so does the tunnel");
    }

    #[test]
    fn a_vnc_session_can_be_told_not_to_send_input() {
        // Read from .mxtsessions since the importer was written, and until now there was nowhere
        // to see it and no way to change it -- so a session marked view-only typed into the
        // desktop anyway. Checked in both directions for the same reason as the RDP case.
        let existing = ProtocolConfig::Vnc(VncConfig {
            host: "host.invalid".to_owned(),
            port: 5901,
            view_only: true,
            ..VncConfig::default()
        });

        let mut dialog = SessionDialog::default();
        dialog.open_for(&existing);
        assert!(dialog.fields.vnc_view_only, "the setting reaches the tab");

        dialog.fields.vnc_view_only = false;
        let mut target = existing.clone();
        match dialog.build() {
            DialogOutcome::Accepted(produced) => SessionDialog::merge_into(*produced, &mut target),
            other => panic!("the dialog refused a complete VNC session: {other:?}"),
        }

        let ProtocolConfig::Vnc(vnc) = target else {
            panic!("a VNC session stopped being one")
        };
        assert!(
            !vnc.view_only,
            "turning it off has to stick, or it cannot be undone"
        );
    }

    #[test]
    fn an_rdp_session_keeps_the_settings_only_it_has() {
        // Three fields that the model carried and the dialog could not touch. `domain` was loaded
        // and written back with no widget anywhere, so an imported domain account could never be
        // corrected; `clipboard` and `multi_monitor` were left out of the merge, which was right
        // while nothing could set them and would quietly reset a session once something could.
        //
        // Checked both ways round, because each direction fails on its own: produce-only would
        // write defaults over a stored session, and merge-only would drop what was just typed.
        let existing = ProtocolConfig::Rdp(RdpConfig {
            host: "host.invalid".to_owned(),
            port: 3389,
            user: Some("someone".to_owned()),
            domain: Some("CORP".to_owned()),
            clipboard: false,
            multi_monitor: true,
            ..RdpConfig::default()
        });

        let mut dialog = SessionDialog::default();
        dialog.open_for(&existing);
        assert_eq!(dialog.fields.domain, "CORP", "the domain reaches the field");
        assert!(
            !dialog.fields.rdp_clipboard,
            "and so does a cleared checkbox"
        );
        assert!(dialog.fields.rdp_multi_monitor);

        // Edited the way somebody would: a different domain, clipboard back on.
        dialog.fields.domain = "OTHER".to_owned();
        dialog.fields.rdp_clipboard = true;

        let mut target = existing.clone();
        match dialog.build() {
            DialogOutcome::Accepted(produced) => SessionDialog::merge_into(*produced, &mut target),
            other => panic!("the dialog refused a complete RDP session: {other:?}"),
        }

        let ProtocolConfig::Rdp(rdp) = target else {
            panic!("an RDP session stopped being one")
        };
        assert_eq!(rdp.domain.as_deref(), Some("OTHER"), "the edit lands");
        assert!(rdp.clipboard, "and so does the checkbox");
        assert!(rdp.multi_monitor, "what was not edited is kept");
    }

    #[test]
    fn every_protocol_has_an_advanced_and_a_bookmark_tab() {
        // Both are in the enum precisely so they can be selected. Before that the row was four labels
        // that could be clicked and did nothing.
        for protocol in DialogProtocol::ALL {
            let dialog = SessionDialog {
                protocol,
                ..SessionDialog::default()
            };
            assert_eq!(dialog.secondary, SecondaryTab::Advanced, "{protocol:?}");
        }
        assert_eq!(SecondaryTab::Bookmark.label(), "Bookmark settings");
    }

    #[test]
    fn there_are_fifteen_protocol_tabs_in_the_measured_order() {
        let labels: Vec<&str> = DialogProtocol::ALL.iter().map(|p| p.label()).collect();
        assert_eq!(
            labels,
            vec![
                "SSH", "Telnet", "Rsh", "Xdmcp", "RDP", "VNC", "FTP", "SFTP", "Serial", "File",
                "Shell", "Browser", "Mosh", "Aws S3", "WSL",
            ]
        );
    }

    #[test]
    fn the_measured_default_ports_are_the_ones_the_reference_shows() {
        assert_eq!(DialogProtocol::Ssh.default_port(), Some(22));
        assert_eq!(DialogProtocol::Sftp.default_port(), Some(22));
        assert_eq!(DialogProtocol::Telnet.default_port(), Some(23));
        assert_eq!(DialogProtocol::Ftp.default_port(), Some(21));
        assert_eq!(DialogProtocol::Rdp.default_port(), Some(3389));
        assert_eq!(DialogProtocol::Vnc.default_port(), Some(5900));

        // Measured absences, not omissions: neither tab has a port field.
        assert_eq!(DialogProtocol::Rsh.default_port(), None);
        assert_eq!(DialogProtocol::Mosh.default_port(), None);
    }

    #[test]
    fn the_secondary_tabs_follow_the_rule_including_where_it_breaks() {
        use SecondaryTab::{Network, Terminal};

        assert_eq!(DialogProtocol::Ssh.secondary_tabs(), &[Terminal, Network]);
        assert_eq!(DialogProtocol::Rdp.secondary_tabs(), &[Network]);
        assert_eq!(DialogProtocol::Serial.secondary_tabs(), &[Terminal]);

        // The two the reference is inconsistent about, reproduced deliberately.
        assert!(DialogProtocol::Shell.secondary_tabs().is_empty());
        assert_eq!(
            DialogProtocol::Sftp.secondary_tabs(),
            &[] as &[SecondaryTab]
        );
    }

    #[test]
    fn group_names_keep_the_references_own_capitalisation() {
        // `Basic Rdp settings`, not `Basic RDP settings`, even though the tab says RDP. Copying the
        // layout means copying this.
        assert_eq!(DialogProtocol::Rdp.group_name(), "Basic Rdp settings");
        assert_eq!(DialogProtocol::Ssh.group_name(), "Basic SSH settings");
        assert_eq!(
            DialogProtocol::File.group_name(),
            "Basic File/folder settings"
        );
    }

    #[test]
    fn the_embedded_cygwin_shells_are_not_offered() {
        // The reference lists Bash (embedded) and Zsh (embedded), which are its bundled Cygwin. That
        // environment is a permanent non-goal, and a shell that cannot launch is worse than absent.
        assert!(!SHELL_CHOICES.iter().any(|s| s.contains("embedded")));
        assert!(SHELL_CHOICES.contains(&"Windows PowerShell"));
        assert!(SHELL_CHOICES.contains(&"PowerShell"));
    }

    #[test]
    fn opening_fresh_fills_in_the_default_port() {
        let mut dialog = SessionDialog::default();
        dialog.open_fresh();
        assert!(dialog.open);
        assert_eq!(dialog.protocol, DialogProtocol::Ssh);
        assert_eq!(dialog.fields.port, "22");
    }

    #[test]
    fn switching_protocol_replaces_a_default_port_but_keeps_a_typed_one() {
        // Somebody who typed 2222 and then noticed they wanted SFTP meant to keep 2222.
        let mut dialog = SessionDialog::default();
        dialog.open_fresh();
        dialog.protocol = DialogProtocol::Rdp;
        dialog.set_port_for_protocol();
        assert_eq!(dialog.fields.port, "3389");

        dialog.fields.port = "2222".to_string();
        dialog.protocol = DialogProtocol::Sftp;
        dialog.set_port_for_protocol();
        assert_eq!(dialog.fields.port, "2222");
    }

    #[test]
    fn a_missing_host_is_reported_as_the_field_it_is() {
        let mut dialog = SessionDialog::default();
        dialog.open_fresh();
        match dialog.build() {
            DialogOutcome::Incomplete { field } => assert_eq!(field, "Remote host"),
            other => panic!("expected an incomplete report, got {other:?}"),
        }
    }

    #[test]
    fn ssh_builds_a_session_with_what_was_typed() {
        let mut dialog = SessionDialog::default();
        dialog.open_fresh();
        dialog.fields.host = "  srv.int  ".to_string();
        dialog.fields.user = " admin ".to_string();
        dialog.fields.port = "2222".to_string();

        match dialog.build() {
            DialogOutcome::Accepted(config) => match *config {
                ProtocolConfig::Ssh(ssh) => {
                    assert_eq!(ssh.host, "srv.int", "surrounding space was not trimmed");
                    assert_eq!(ssh.user.as_deref(), Some("admin"));
                    assert_eq!(ssh.port, 2222);
                }
                other => panic!("expected an SSH session, got {other:?}"),
            },
            other => panic!("expected acceptance, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_username_stays_absent_rather_than_becoming_an_empty_one() {
        // A session with no user means "use the default", which is not the same as a user named "".
        let mut dialog = SessionDialog::default();
        dialog.open_fresh();
        dialog.fields.host = "srv.int".to_string();
        dialog.fields.user = "   ".to_string();

        match dialog.build() {
            DialogOutcome::Accepted(config) => match *config {
                ProtocolConfig::Ssh(ssh) => assert_eq!(ssh.user, None),
                other => panic!("expected SSH, got {other:?}"),
            },
            other => panic!("expected acceptance, got {other:?}"),
        }
    }

    #[test]
    fn an_unbuilt_protocol_says_which_one_it_was() {
        // "Nothing happened" is indistinguishable from a bug. Nine of the fifteen have no model
        // representation yet, and the name is what makes the message worth reading.
        let mut dialog = SessionDialog::default();
        dialog.open_fresh();
        dialog.protocol = DialogProtocol::AwsS3;
        dialog.fields.key_id = "AKIA".to_string();

        match dialog.build() {
            DialogOutcome::Unsupported(name) => assert_eq!(name, "Aws S3"),
            other => panic!("expected an unsupported report, got {other:?}"),
        }
    }

    #[test]
    fn an_outcome_can_only_be_taken_once() {
        // Two presses of OK from one click would open two sessions.
        let mut dialog = SessionDialog {
            outcome: Some(DialogOutcome::Cancelled),
            ..SessionDialog::default()
        };
        assert!(dialog.take_outcome().is_some());
        assert!(dialog.take_outcome().is_none());
    }
}
