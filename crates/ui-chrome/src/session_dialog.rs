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
    /// Character-stream settings.
    Terminal,
    /// Proxy and jump settings.
    Network,
}

impl SecondaryTab {
    /// The tab's label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal settings",
            Self::Network => "Network settings",
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
#[derive(Clone, Debug, Default)]
pub struct SessionFields {
    /// Host or address.
    pub host: String,
    /// Login name.
    pub user: String,
    /// Port, as typed. Kept as text so a half-typed number is not silently rounded to zero.
    pub port: String,
    /// Windows domain, for RDP.
    pub domain: String,
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
}

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
    outcome: Option<DialogOutcome>,
}

impl Default for SessionDialog {
    fn default() -> Self {
        Self {
            open: false,
            protocol: DialogProtocol::Ssh,
            fields: SessionFields::default(),
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
            }
            ProtocolConfig::Vnc(vnc) => {
                self.protocol = DialogProtocol::Vnc;
                self.fields.host = vnc.host.clone();
                self.fields.port = vnc.port.to_string();
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
                // `auth` is deliberately untouched. This dialog has no field for it, so whatever it
                // produced is a default rather than a choice — and copying that over would turn
                // every edited session into an agent session, which is precisely the bug that left
                // 128 imported sessions unable to connect.
            }
            (ProtocolConfig::Telnet(new), ProtocolConfig::Telnet(old)) => *old = new,
            (ProtocolConfig::Rdp(new), ProtocolConfig::Rdp(old)) => {
                old.host = new.host;
                old.port = new.port;
                old.user = new.user;
                old.domain = new.domain;
            }
            (ProtocolConfig::Vnc(new), ProtocolConfig::Vnc(old)) => {
                old.host = new.host;
                old.port = new.port;
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
                DialogOutcome::Accepted(Box::new(ProtocolConfig::Ssh(SshConfig {
                    host: host.to_owned(),
                    port,
                    user: optional_user,
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
                    ..RdpConfig::default()
                })))
            }
            DialogProtocol::Vnc => {
                DialogOutcome::Accepted(Box::new(ProtocolConfig::Vnc(VncConfig {
                    host: host.to_owned(),
                    port,
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
    secondary_tab_row(ui, theme, dialog.protocol);
    ui.add_space(6.0);
    description_area(ui, theme, dialog.protocol);
    ui.add_space(8.0);

    // Centred, as measured, and both carrying a glyph. `vertical_centered` centres the layout it
    // creates and not a horizontal row nested inside it, so the row is offset by hand.
    let buttons = 200.0;
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - buttons) / 2.0).max(0.0));
        if ui.button("✓  OK").clicked() {
            dialog.outcome = Some(dialog.build());
            dialog.open = false;
        }
        ui.add_space(12.0);
        if ui.button("✕  Cancel").clicked() {
            dialog.outcome = Some(DialogOutcome::Cancelled);
            dialog.open = false;
        }
    });
}

fn title_row(ui: &mut Ui, theme: &ChromeTheme, dialog: &mut SessionDialog) {
    ui.horizontal(|ui| {
        ui.label("Session settings");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("✕").clicked() {
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

        // The hollow square that stands in for the icon set, as everywhere else.
        let side = 20.0;
        let icon = Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 6.0 + side / 2.0),
            vec2(side, side),
        );
        painter.rect_stroke(
            icon,
            CornerRadius::ZERO,
            Stroke::new(1.0, theme.text_dim),
            egui::StrokeKind::Inside,
        );
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

fn secondary_tab_row(ui: &mut Ui, theme: &ChromeTheme, protocol: DialogProtocol) {
    ui.horizontal(|ui| {
        // The advanced tab is named after the protocol, which is why it is not in the measured list.
        let advanced = format!("Advanced {} settings", advanced_name(protocol));
        let _ = ui.selectable_label(false, advanced);
        for tab in protocol.secondary_tabs() {
            let _ = ui.selectable_label(false, tab.label());
        }
        let _ = ui.selectable_label(false, "★ Bookmark settings");
    });
    ui.label(
        egui::RichText::new(
            "These tabs hold dozens of fields each and have not been measured yet.",
        )
        .small()
        .color(theme.text_dim),
    );
}

/// The word the reference puts in `Advanced … settings`, which is not always the tab's label.
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
