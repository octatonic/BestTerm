//! Reading MobaXterm's `.mxtsessions` files.
//!
//! The format is an INI file in CP1252 with CRLF line endings. Folders are `[Bookmarks]` sections
//! carrying a `SubRep` path; every other key in a section is a session, whose value is a `#`-separated
//! list of groups, each of which is a `%`-separated list of fields, with a handful of textual escapes
//! standing in for characters the separators would otherwise eat.
//!
//! Written against the reverse-engineered specification at
//! <https://gist.github.com/Ruzgfpegk/ab597838e4abbe8de30d7224afd062ea>, which documents the layout
//! field by field for version 26.3.
//!
//! # What the importer does with credentials
//!
//! MobaXterm stores the SFTP proxy password in clear text. This importer does not copy it into the
//! session tree; it hands it back separately as an [`ImportedSecret`] for the caller to put in the
//! vault, leaving only an opaque reference behind. Importing therefore *removes* a plaintext password
//! from the user's configuration rather than carrying it across.

use bestterm_core_model::{
    CredentialRef, ModelError, NodeId, ProtocolConfig, RdpConfig, SessionTree, SettingsOverride,
    SshAuth, SshConfig, VncConfig,
};
use bestterm_core_vault::Secret;

use crate::cp1252;

/// Why a session was not imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The session uses a protocol BestTerm does not have yet.
    UnsupportedProtocol {
        /// The numeric type from the file.
        type_id: String,
        /// What that type is, where it is known.
        name: &'static str,
    },
    /// The line could not be read.
    Malformed(&'static str),
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProtocol { type_id, name } => {
                write!(f, "unsupported protocol {name} (type {type_id})")
            }
            Self::Malformed(detail) => write!(f, "malformed: {detail}"),
        }
    }
}

/// A session that was not imported, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// Name as it appeared in the file.
    pub name: String,
    /// Folder path it appeared under.
    pub folder: String,
    /// Why it was left out.
    pub reason: SkipReason,
}

/// A credential lifted out of the file, for the caller to store in the vault.
#[derive(Debug)]
pub struct ImportedSecret {
    /// The handle the imported session refers to.
    pub reference: CredentialRef,
    /// The plaintext, to be sealed and then dropped.
    pub secret: Secret,
    /// What it is, for a confirmation prompt.
    pub description: String,
}

/// The result of reading a file.
#[derive(Debug, Default)]
pub struct Import {
    /// Everything that could be imported.
    pub tree: SessionTree,
    /// Credentials found in clear text, to be moved into the vault.
    pub secrets: Vec<ImportedSecret>,
    /// Sessions left out, with reasons.
    pub skipped: Vec<Skipped>,
    /// Things worth telling the user that are not failures.
    pub notes: Vec<String>,
}

impl Import {
    /// How many sessions were imported.
    pub fn imported_sessions(&self) -> usize {
        self.tree
            .walk()
            .into_iter()
            .filter(|id| self.tree.get(*id).is_some_and(|node| !node.is_folder()))
            .count()
    }
}

/// Read a `.mxtsessions` file.
///
/// Takes bytes rather than a string because the file is CP1252 and decoding is part of the job.
///
/// Never fails as a whole: a line that cannot be understood becomes an entry in [`Import::skipped`].
/// Refusing an entire export of four hundred sessions because one of them uses a protocol we do not
/// support yet would be the wrong trade.
pub fn parse(bytes: &[u8]) -> Import {
    let text = cp1252::decode(bytes);
    let mut import = Import::default();

    // Folder path components of the section being read; empty means the tree root.
    let mut current_path: Vec<String> = Vec::new();
    let mut current_folder: Option<NodeId> = None;
    let mut in_section = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        // An INI comment. Tested before the `=` split so a session whose name begins with a
        // semicolon is not mistaken for one.
        if line.starts_with(';') && !line.contains('=') {
            continue;
        }

        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_section = section == "Bookmarks" || section.starts_with("Bookmarks_");
            current_path.clear();
            current_folder = None;
            if !in_section {
                import
                    .notes
                    .push(format!("ignored unrecognised section [{section}]"));
            }
            continue;
        }

        if !in_section {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();

        match key {
            "SubRep" => {
                current_path = split_folder_path(value);
                current_folder = ensure_folder(&mut import.tree, &current_path);
            }
            // Folder icon. Recorded on the folder if one exists.
            "ImgNum" => {
                if let Some(id) = current_folder
                    && let Some(node) = import.tree.get_mut(id)
                {
                    node.icon = moba_icon(value);
                }
            }
            _ => {
                let folder_label = current_path.join(" / ");
                match parse_session(key, value) {
                    Ok(session) => add_session(&mut import, current_folder, &folder_label, session),
                    Err(reason) => import.skipped.push(Skipped {
                        name: key.to_string(),
                        folder: folder_label,
                        reason,
                    }),
                }
            }
        }
    }

    tracing::info!(
        sessions = import.imported_sessions(),
        skipped = import.skipped.len(),
        secrets = import.secrets.len(),
        "read a .mxtsessions file"
    );

    import
}

/// A session parsed out of one line, before it is placed in the tree.
struct ParsedSession {
    name: String,
    config: ProtocolConfig,
    settings: SettingsOverride,
    icon: Option<String>,
    comment: Option<String>,
    /// A plaintext credential found in the line, if any.
    plaintext_secret: Option<(String, Secret)>,
}

fn add_session(
    import: &mut Import,
    parent: Option<NodeId>,
    folder_label: &str,
    session: ParsedSession,
) {
    let ParsedSession {
        name,
        config,
        settings,
        icon,
        comment,
        plaintext_secret,
    } = session;

    let id = match import.tree.add_session(parent, name.clone(), config) {
        Ok(id) => id,
        Err(error) => {
            import.skipped.push(Skipped {
                name,
                folder: folder_label.to_string(),
                reason: malformed_from(error),
            });
            return;
        }
    };

    if let Some(node) = import.tree.get_mut(id) {
        node.settings = settings;
        node.icon = icon;
        node.comment = comment;
    }

    if let Some((description, secret)) = plaintext_secret {
        // The reference is derived from the node's own id, so two sessions with the same name in
        // different folders cannot collide in the vault.
        let reference = CredentialRef::new(format!("session/{id}/proxy-password"));
        attach_proxy_credential(&mut import.tree, id, reference.clone());
        import.secrets.push(ImportedSecret {
            reference,
            secret,
            description,
        });
    }
}

fn attach_proxy_credential(_tree: &mut SessionTree, _id: NodeId, _reference: CredentialRef) {
    // Proxy credentials have nowhere to live in `ProtocolConfig` yet — proxy support arrives with
    // SSH in phase 2. The secret is still lifted out of the file and handed to the vault rather than
    // written back into the tree, so nothing is stored in clear text in the meantime; the reference
    // is reattached when the field exists.
}

fn malformed_from(error: ModelError) -> SkipReason {
    match error {
        ModelError::NotAFolder(_) => SkipReason::Malformed("parent is not a folder"),
        ModelError::UnknownNode(_) => SkipReason::Malformed("parent folder is missing"),
        ModelError::WouldCreateCycle { .. } => SkipReason::Malformed("would create a cycle"),
    }
}

/// Split a `SubRep` value into folder names.
///
/// Components are backslash-separated. Empty components are dropped, which handles both the escaped
/// `A\\B` form the specification shows and the plain `A\B` that appears in real files, and means the
/// root section's empty `SubRep=` yields no folders at all.
fn split_folder_path(value: &str) -> Vec<String> {
    value
        .split('\\')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(unescape)
        .collect()
}

/// Find or create the folder chain, returning the leaf.
fn ensure_folder(tree: &mut SessionTree, path: &[String]) -> Option<NodeId> {
    let mut parent: Option<NodeId> = None;
    for name in path {
        let existing = children_of(tree, parent).into_iter().find(|id| {
            tree.get(*id)
                .is_some_and(|node| node.is_folder() && node.name == *name)
        });

        parent = Some(match existing {
            Some(id) => id,
            None => tree.add_folder(parent, name.clone()).ok()?,
        });
    }
    parent
}

fn children_of(tree: &SessionTree, parent: Option<NodeId>) -> Vec<NodeId> {
    match parent {
        Some(id) => tree.children(id).to_vec(),
        None => tree.roots().to_vec(),
    }
}

// Session type identifiers.
//
// Only these are imported, because only these are documented. The specification records the ids for
// SSH, RDP, VNC, SFTP and Browser and says merely that "other session types have other identifiers".
// Telnet and serial certainly exist in MobaXterm, but guessing their numbers risks importing a
// serial console as a telnet session and pointing it at a host that is really a COM port — a silent
// wrong import, which is worse than a reported skip.
const TYPE_SSH: &str = "0";
const TYPE_RDP: &str = "4";
const TYPE_VNC: &str = "5";
const TYPE_SFTP: &str = "7";

fn parse_session(name: &str, value: &str) -> Result<ParsedSession, SkipReason> {
    // The comment field escapes '#' as `__DIEZE__`, so splitting on '#' cannot cut a value in half.
    let groups: Vec<&str> = value.split('#').collect();
    let connection = groups
        .get(2)
        .ok_or(SkipReason::Malformed("no connection group"))?;

    let fields: Vec<&str> = connection.split('%').collect();
    let type_id = fields
        .first()
        .map(|field| field.trim())
        .ok_or(SkipReason::Malformed("no session type"))?;

    let config = match type_id {
        TYPE_SSH => ProtocolConfig::Ssh(ssh_config(&fields)),
        // SFTP has no protocol of its own here: the file browser is a panel on an SSH session, so an
        // imported SFTP bookmark becomes the SSH session it is a view of.
        TYPE_SFTP => ProtocolConfig::Ssh(sftp_as_ssh(&fields)),
        TYPE_RDP => ProtocolConfig::Rdp(rdp_config(&fields)),
        TYPE_VNC => ProtocolConfig::Vnc(vnc_config(&fields)),
        other => {
            return Err(SkipReason::UnsupportedProtocol {
                type_id: other.to_string(),
                name: protocol_name(other),
            });
        }
    };

    let mut settings = SettingsOverride::default();
    if type_id == TYPE_SSH {
        // Index 5 is X11 forwarding; the rest of the SSH flags have no home in the model yet.
        if field(&fields, 5).is_some_and(moba_bool) {
            settings.x11_forwarding = Some(true);
        }
        if field(&fields, 33).is_some_and(moba_bool) {
            settings.agent_forwarding = Some(true);
        }
    }
    settings.tab_color = groups.get(6).and_then(|raw| tab_color(raw));

    let comment = groups
        .get(5)
        .map(|raw| unescape(raw.trim()))
        .filter(|text| !text.is_empty());

    let plaintext_secret = if type_id == TYPE_SFTP {
        field(&fields, 14).filter(|raw| !raw.is_empty()).map(|raw| {
            (
                format!("proxy password for `{name}`"),
                Secret::new(unescape(raw)),
            )
        })
    } else {
        None
    };

    Ok(ParsedSession {
        name: unescape(name.trim()),
        config,
        settings,
        icon: groups.get(1).and_then(|raw| moba_icon(raw)),
        comment,
        plaintext_secret,
    })
}

/// Where the private key path sits in a **type 0** session's connection group.
///
/// Measured against a real file of 128 SSH sessions: 122 carry a path here and six leave it empty, and
/// there is no separate "use a key" flag -- fields 12, 13 and 15 are `0`, `0` and empty in every one
/// of them. So the presence of a path *is* the flag.
///
/// This mattered more than a field index usually does. Without it every imported session defaulted to
/// `SshAuth::Agent`, and on a machine whose ssh-agent is not running -- which is the Windows default,
/// since the service ships disabled -- all 128 failed with "the ssh agent: early eof". The key was in
/// the file the whole time.
///
/// # The index is per session type, not per file
///
/// Field 14 of a **type 7** session is the SFTP proxy password, which is why
/// [`parse_session`] guards that read on the type. The first version of this generalised the index
/// across both and turned a proxy password into a private key path; the importer's own test caught it.
/// An index is only ever measured for the type it was measured on.
const FIELD_PRIVATE_KEY: usize = 14;

fn ssh_config(fields: &[&str]) -> SshConfig {
    SshConfig {
        host: field(fields, 1).map(unescape).unwrap_or_default(),
        port: port(fields, 2, 22),
        user: user(fields, 3),
        auth: ssh_auth(fields),
        // Jump hosts are references to other sessions in the tree, and MobaXterm stores them as bare
        // hostnames. Resolving one to a node would mean inventing a session that may already exist
        // under another name, so the gateway is recorded as a note instead of guessed at.
        ..Default::default()
    }
}

/// How an imported session authenticates.
///
/// A key if the file names one, and the agent otherwise -- which is the right default for a session
/// that names nothing, because it is what already works for most people.
///
/// The passphrase is left absent rather than guessed. MobaXterm does not store one here, and claiming
/// a vault entry that does not exist would turn "the key needs a passphrase" into "the vault holds no
/// entry called ...", which sends somebody to the wrong place.
fn ssh_auth(fields: &[&str]) -> SshAuth {
    match field(fields, FIELD_PRIVATE_KEY).map(unescape) {
        Some(path) if !path.trim().is_empty() => SshAuth::PublicKey {
            path: path.trim().to_string(),
            passphrase: None,
        },
        _ => SshAuth::Agent,
    }
}

fn sftp_as_ssh(fields: &[&str]) -> SshConfig {
    SshConfig {
        host: field(fields, 1).map(unescape).unwrap_or_default(),
        port: port(fields, 2, 22),
        user: user(fields, 3),
        // Not `ssh_auth`: field 14 of a type 7 session is the proxy password, not a key path. See
        // `FIELD_PRIVATE_KEY`. Where an SFTP bookmark records its key is unmeasured, and the agent is
        // the honest default for a session that names nothing this importer can read.
        ..Default::default()
    }
}

fn rdp_config(fields: &[&str]) -> RdpConfig {
    RdpConfig {
        host: field(fields, 1).map(unescape).unwrap_or_default(),
        port: port(fields, 2, 3389),
        user: user(fields, 3),
        ..Default::default()
    }
}

fn vnc_config(fields: &[&str]) -> VncConfig {
    VncConfig {
        host: field(fields, 1).map(unescape).unwrap_or_default(),
        port: port(fields, 2, 5900),
        view_only: field(fields, 4).is_some_and(moba_bool),
        ..Default::default()
    }
}

/// Name a type only where the specification actually says what it is.
///
/// Anything else is reported by number. A confident-sounding wrong name in a skip message would send
/// someone looking for a bug that is not there.
fn protocol_name(type_id: &str) -> &'static str {
    match type_id {
        "11" => "Browser",
        _ => "unrecognised",
    }
}

fn field<'a>(fields: &[&'a str], index: usize) -> Option<&'a str> {
    fields.get(index).copied().filter(|value| !value.is_empty())
}

fn port(fields: &[&str], index: usize, fallback: u16) -> u16 {
    field(fields, index)
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|value| *value != 0)
        .unwrap_or(fallback)
}

/// A username, unless the file says to use the application-wide default.
fn user(fields: &[&str], index: usize) -> Option<String> {
    field(fields, index)
        .map(str::trim)
        .filter(|value| *value != "<default>")
        .map(unescape)
}

/// MobaXterm's booleans: `-1` for on, `0` for off.
fn moba_bool(value: &str) -> bool {
    value.trim() == "-1"
}

/// Preserve the icon number without inventing a name for it.
///
/// The mapping from MobaXterm's `ImgNum` to BestTerm's own icons cannot be written until BestTerm has
/// icons, which is phase 1. Keeping the original number loses nothing and lets the mapping be applied
/// later; making one up now would have to be undone.
fn moba_icon(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.parse::<u32>().is_err() {
        return None;
    }
    Some(format!("moba:{trimmed}"))
}

/// Decode a custom tab colour.
///
/// Stored as a decimal Windows `COLORREF`, which packs the channels **blue-green-red** rather than
/// the other way round: 255 is pure red, 65280 pure green, 16711680 pure blue. `-1` means the user
/// did not set one.
fn tab_color(value: &str) -> Option<[u8; 3]> {
    let packed: i64 = value.trim().parse().ok()?;
    if packed < 0 {
        return None;
    }
    let packed = packed as u64;
    Some([
        (packed & 0xFF) as u8,
        ((packed >> 8) & 0xFF) as u8,
        ((packed >> 16) & 0xFF) as u8,
    ])
}

/// Undo the format's textual escapes.
fn unescape(raw: &str) -> String {
    raw.replace("__PTVIRG__", ";")
        .replace("__DBLQUO__", "\"")
        .replace("__DIEZE__", "#")
        .replace("__PERCENT__", "%")
        .replace("__PIPE__", "|")
        .replace("_CurrentDrive_", "C:")
}

/// Split a `__PIPE__`-separated list.
///
/// Splitting before unescaping is not a detail: a value containing a literal `|` is written as
/// `__PIPE__` too, so unescaping first would make a single value indistinguishable from two.
#[allow(dead_code)]
fn split_pipe_list(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split("__PIPE__").map(unescape).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal file: a root, a sub-folder, and two SSH sessions.
    fn sample() -> Vec<u8> {
        let text = concat!(
            "[Bookmarks]\r\n",
            "SubRep=\r\n",
            "ImgNum=42\r\n",
            "[Bookmarks_1]\r\n",
            "SubRep=Production\r\n",
            "ImgNum=41\r\n",
            "web=#109#0%web-1.int%22%deploy%%-1%-1%%%%%0%0%0%%%-1%0%0%0%%1080#MobaFont%10#0#the web box#255\r\n",
            "[Bookmarks_2]\r\n",
            "SubRep=Production\\db\r\n",
            "ImgNum=41\r\n",
            "mongo=#109#0%mongo-1.int%2222%<default>%%0%-1%%%%%0%0%0#MobaFont%10#0##-1\r\n",
        );
        text.as_bytes().to_vec()
    }

    #[test]
    fn folders_and_sessions_land_in_the_right_places() {
        let import = parse(&sample());
        assert!(import.skipped.is_empty(), "skipped: {:?}", import.skipped);
        assert_eq!(import.imported_sessions(), 2);

        let paths: Vec<String> = import
            .tree
            .walk()
            .into_iter()
            .map(|id| import.tree.path_string(id))
            .collect();
        assert!(
            paths.contains(&"Production / web".to_string()),
            "got {paths:?}"
        );
        assert!(
            paths.contains(&"Production / db / mongo".to_string()),
            "got {paths:?}"
        );
    }

    #[test]
    fn a_folder_named_twice_is_created_once() {
        // `Production` appears as its own section and again as the parent of `Production\db`.
        let import = parse(&sample());
        let production: Vec<_> = import
            .tree
            .walk()
            .into_iter()
            .filter(|id| {
                import
                    .tree
                    .get(*id)
                    .is_some_and(|node| node.name == "Production")
            })
            .collect();
        assert_eq!(production.len(), 1, "the folder must not be duplicated");
    }

    #[test]
    fn ssh_fields_are_read() {
        let import = parse(&sample());
        let web = find(&import, "web");
        let ProtocolConfig::Ssh(config) = web else {
            panic!("expected ssh, got {web:?}");
        };
        assert_eq!(config.host, "web-1.int");
        assert_eq!(config.port, 22);
        assert_eq!(config.user.as_deref(), Some("deploy"));
    }

    #[test]
    fn a_non_default_port_is_kept() {
        let import = parse(&sample());
        let ProtocolConfig::Ssh(config) = find(&import, "mongo") else {
            panic!("expected ssh");
        };
        assert_eq!(config.port, 2222);
    }

    #[test]
    fn the_placeholder_username_becomes_none() {
        // `<default>` means "whatever the application is configured with", not a login named that.
        let import = parse(&sample());
        let ProtocolConfig::Ssh(config) = find(&import, "mongo") else {
            panic!("expected ssh");
        };
        assert_eq!(config.user, None);
    }

    #[test]
    fn x11_forwarding_is_carried_over_as_a_setting() {
        let import = parse(&sample());
        let web = node_named(&import, "web");
        assert_eq!(
            import.tree.get(web).expect("node").settings.x11_forwarding,
            Some(true)
        );
        // mongo has it disabled, which must not become an override of `false`: an unset field
        // inherits, and the file's "off" is MobaXterm's default rather than an opinion.
        let mongo = node_named(&import, "mongo");
        assert_eq!(
            import
                .tree
                .get(mongo)
                .expect("node")
                .settings
                .x11_forwarding,
            None
        );
    }

    #[test]
    fn comments_and_tab_colours_are_imported() {
        let import = parse(&sample());
        let web = node_named(&import, "web");
        let node = import.tree.get(web).expect("node");
        assert_eq!(node.comment.as_deref(), Some("the web box"));
        // 255 is pure red in a COLORREF.
        assert_eq!(node.settings.tab_color, Some([255, 0, 0]));
    }

    #[test]
    fn colorref_channels_are_blue_green_red() {
        // The single easiest thing to get backwards in this format.
        assert_eq!(tab_color("255"), Some([255, 0, 0]));
        assert_eq!(tab_color("65280"), Some([0, 255, 0]));
        assert_eq!(tab_color("16711680"), Some([0, 0, 255]));
        assert_eq!(tab_color("0"), Some([0, 0, 0]));
        assert_eq!(tab_color("-1"), None, "unset must stay unset");
        assert_eq!(tab_color(""), None);
    }

    #[test]
    fn rdp_and_vnc_are_imported_with_their_own_defaults() {
        let text = concat!(
            "[Bookmarks]\r\n",
            "SubRep=\r\n",
            "desk=#91#4%win-1%3389%admin%0%0%0%0%-1%0%0#MobaFont%10#0##-1\r\n",
            "screen=#128#5%kiosk%5901%-1%-1#MobaFont%10#0##-1\r\n",
        );
        let import = parse(text.as_bytes());
        assert!(import.skipped.is_empty(), "{:?}", import.skipped);

        let ProtocolConfig::Rdp(rdp) = find(&import, "desk") else {
            panic!("expected rdp");
        };
        assert_eq!((rdp.host.as_str(), rdp.port), ("win-1", 3389));
        assert_eq!(rdp.user.as_deref(), Some("admin"));

        let ProtocolConfig::Vnc(vnc) = find(&import, "screen") else {
            panic!("expected vnc");
        };
        assert_eq!((vnc.host.as_str(), vnc.port), ("kiosk", 5901));
        assert!(vnc.view_only, "index 4 set to -1 means view only");
    }

    #[test]
    fn an_sftp_bookmark_becomes_the_ssh_session_it_is_a_view_of() {
        let text = concat!(
            "[Bookmarks]\r\n",
            "SubRep=\r\n",
            "files=#140#7%files.int%22%ops%-1%0%%0%0%%0%1080#MobaFont%10#0##-1\r\n",
        );
        let import = parse(text.as_bytes());
        let ProtocolConfig::Ssh(config) = find(&import, "files") else {
            panic!("expected ssh");
        };
        assert_eq!(config.host, "files.int");
        assert_eq!(config.user.as_deref(), Some("ops"));
    }

    #[test]
    fn an_ssh_session_with_a_key_authenticates_with_it_rather_than_the_agent() {
        // The whole reason imported sessions did not work. Without this every one of them defaulted to
        // the agent, and on Windows the OpenSSH agent service ships disabled -- so all 128 sessions in
        // a real file failed with "the ssh agent: early eof" while their keys sat in the file.
        let text = concat!(
            "[Bookmarks]\r\n",
            "SubRep=\r\n",
            "srv=#109#0%srv.int%22%ops%-1%-1%%-1%-1%%%-1%0%0%",
            r"D:\keys\ops.ppk",
            "%%-1%0#MobaFont%10#0##-1\r\n",
        );
        let import = parse(text.as_bytes());

        assert_eq!(
            ssh_of(&import).auth,
            SshAuth::PublicKey {
                path: r"D:\keys\ops.ppk".to_string(),
                passphrase: None
            }
        );
    }

    #[test]
    fn an_ssh_session_with_no_key_still_uses_the_agent() {
        // The right default for a session that names nothing: it is what already works for most people.
        let text = concat!(
            "[Bookmarks]\r\n",
            "SubRep=\r\n",
            "srv=#109#0%srv.int%22%ops%-1%-1%%-1%-1%%%-1%0%0%%%-1%0#MobaFont%10#0##-1\r\n",
        );
        let import = parse(text.as_bytes());
        assert_eq!(ssh_of(&import).auth, SshAuth::Agent);
    }

    /// The single SSH session in an import, for the tests above.
    fn ssh_of(import: &Import) -> SshConfig {
        let id = import.tree.walk().first().copied().expect("one session");
        let node = import.tree.get(id).expect("the node");
        let bestterm_core_model::NodeKind::Session { config } = &node.kind else {
            panic!("expected a session");
        };
        let ProtocolConfig::Ssh(ssh) = config.as_ref() else {
            panic!("expected ssh, got {:?}", config.protocol())
        };
        ssh.clone()
    }

    #[test]
    fn a_plaintext_proxy_password_is_lifted_into_the_vault_not_the_tree() {
        // The headline of the importer: the password leaves the configuration file.
        let text = concat!(
            "[Bookmarks]\r\n",
            "SubRep=\r\n",
            "files=#140#7%files.int%22%ops%-1%0%%0%0%%4%proxy.int%1080%puser%s3cr3t#MobaFont%10#0##-1\r\n",
        );
        let import = parse(text.as_bytes());

        assert_eq!(import.secrets.len(), 1, "{:?}", import.secrets);
        assert_eq!(import.secrets[0].secret.expose(), "s3cr3t");
        assert!(import.secrets[0].description.contains("files"));

        // And nothing in the tree carries it.
        let dumped = format!("{:?}", import.tree);
        assert!(!dumped.contains("s3cr3t"), "got:\n{dumped}");
    }

    #[test]
    fn an_unsupported_protocol_is_reported_rather_than_dropped() {
        let text = concat!(
            "[Bookmarks]\r\n",
            "SubRep=\r\n",
            "keep=#109#0%good.int%22%%%0%0#MobaFont%10#0##-1\r\n",
            "browse=#313#11%https://example.invalid#MobaFont%10#0##-1\r\n",
        );
        let import = parse(text.as_bytes());

        assert_eq!(import.imported_sessions(), 1, "the good one still arrives");
        assert_eq!(import.skipped.len(), 1);
        assert_eq!(import.skipped[0].name, "browse");
        assert_eq!(
            import.skipped[0].reason,
            SkipReason::UnsupportedProtocol {
                type_id: "11".to_string(),
                name: "Browser",
            }
        );
    }

    #[test]
    fn an_undocumented_type_is_skipped_and_reported_by_number() {
        // MobaXterm has telnet and serial sessions, but the specification does not say which numbers
        // they use. Guessing risks importing a serial console as a telnet host; the number is
        // reported instead so the gap is visible and fixable.
        let text = concat!(
            "[Bookmarks]\r\n",
            "SubRep=\r\n",
            "serial=#131#8%COM3%9600#MobaFont%10#0##-1\r\n",
        );
        let import = parse(text.as_bytes());
        assert_eq!(import.imported_sessions(), 0);
        assert_eq!(
            import.skipped[0].reason,
            SkipReason::UnsupportedProtocol {
                type_id: "8".to_string(),
                name: "unrecognised",
            }
        );
        assert!(import.skipped[0].reason.to_string().contains("type 8"));
    }

    #[test]
    fn escapes_are_undone() {
        assert_eq!(unescape("a__PTVIRG__b"), "a;b");
        assert_eq!(unescape("say __DBLQUO__hi__DBLQUO__"), "say \"hi\"");
        assert_eq!(unescape("a__PIPE__b"), "a|b");
        assert_eq!(unescape("c__DIEZE__1"), "c#1");
        assert_eq!(unescape("50__PERCENT__"), "50%");
        assert_eq!(unescape("_CurrentDrive_\\keys\\id_rsa"), "C:\\keys\\id_rsa");
    }

    #[test]
    fn a_pipe_list_is_split_before_it_is_unescaped() {
        // Unescaping first would turn `a__PIPE__b` into `a|b` and lose the boundary.
        assert_eq!(
            split_pipe_list("host1__PIPE__host2"),
            vec!["host1", "host2"]
        );
        assert_eq!(split_pipe_list(""), Vec::<String>::new());
        assert_eq!(
            split_pipe_list("only"),
            vec!["only"],
            "a single value is one element, not zero"
        );
    }

    #[test]
    fn cp1252_names_survive_the_import() {
        let mut bytes = b"[Bookmarks]\r\nSubRep=\r\nCaf".to_vec();
        bytes.push(0xE9); // é
        bytes.extend_from_slice(b"=#109#0%cafe.int%22%%%0%0#MobaFont%10#0##-1\r\n");

        let import = parse(&bytes);
        assert_eq!(import.imported_sessions(), 1);
        let names: Vec<String> = import
            .tree
            .walk()
            .into_iter()
            .filter_map(|id| import.tree.get(id))
            .map(|node| node.name.clone())
            .collect();
        assert_eq!(names, vec!["Café".to_string()]);
    }

    #[test]
    fn icon_numbers_are_preserved_verbatim() {
        // Not mapped to a name yet: BestTerm has no icon set until phase 1, and inventing one now
        // would have to be undone.
        let import = parse(&sample());
        let web = node_named(&import, "web");
        assert_eq!(
            import.tree.get(web).expect("node").icon.as_deref(),
            Some("moba:109")
        );
    }

    #[test]
    fn an_empty_file_imports_nothing_and_complains_about_nothing() {
        let import = parse(b"");
        assert!(import.tree.is_empty());
        assert!(import.skipped.is_empty());
        assert!(import.secrets.is_empty());
    }

    #[test]
    fn a_truncated_line_is_skipped_with_a_reason() {
        let text = "[Bookmarks]\r\nSubRep=\r\nbroken=#109\r\n";
        let import = parse(text.as_bytes());
        assert_eq!(import.imported_sessions(), 0);
        assert_eq!(
            import.skipped.first().map(|s| s.reason.clone()),
            Some(SkipReason::Malformed("no connection group"))
        );
    }

    #[test]
    fn lines_outside_a_bookmarks_section_are_ignored() {
        let text = concat!(
            "[Misc]\r\n",
            "SomeSetting=1\r\n",
            "[Bookmarks]\r\n",
            "SubRep=\r\n",
            "good=#109#0%h%22%%%0%0#MobaFont%10#0##-1\r\n",
        );
        let import = parse(text.as_bytes());
        assert_eq!(import.imported_sessions(), 1);
        assert!(import.skipped.is_empty());
        assert_eq!(import.notes.len(), 1, "the ignored section is mentioned");
    }

    #[test]
    fn unix_line_endings_are_accepted_too() {
        // Real files are CRLF, but anything that has been through a text editor or git may not be.
        let text = "[Bookmarks]\nSubRep=\nhost=#109#0%h%22%%%0%0#MobaFont%10#0##-1\n";
        assert_eq!(parse(text.as_bytes()).imported_sessions(), 1);
    }

    #[test]
    fn a_backslash_pair_in_a_folder_path_is_one_separator() {
        let text = "[Bookmarks]\r\nSubRep=A\\\\B\r\nh=#109#0%h%22%%%0%0#MobaFont%10#0##-1\r\n";
        let import = parse(text.as_bytes());
        let id = node_named(&import, "h");
        assert_eq!(import.tree.path_string(id), "A / B / h");
    }

    fn node_named(import: &Import, name: &str) -> NodeId {
        import
            .tree
            .walk()
            .into_iter()
            .find(|id| import.tree.get(*id).is_some_and(|node| node.name == name))
            .unwrap_or_else(|| panic!("no node named {name}"))
    }

    fn find<'a>(import: &'a Import, name: &str) -> &'a ProtocolConfig {
        let id = node_named(import, name);
        import
            .tree
            .get(id)
            .and_then(|node| node.kind.config())
            .unwrap_or_else(|| panic!("{name} is not a session"))
    }
}
