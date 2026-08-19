//! SFTP on an SSH session that is already open.
//!
//! The point of holding the SSH connection in this process rather than shelling out to `ssh` is that
//! a file browser costs one more channel, not a second login: no second password prompt, no second
//! entry in the server's auth log, and the browser cannot be looking at a different host from the
//! terminal beside it. That is the one thing an architecture built on external processes cannot do,
//! and it is why this crate exists rather than a wrapper around `sftp`.
//!
//! The framing is [`russh_sftp`]. What is here is the part a file browser needs and the protocol
//! crate does not have: entries in a shape the interface can draw, path arithmetic that behaves the
//! way a remote POSIX path does rather than the way the local platform's paths do, and transfers that
//! can be resumed.
//!
//! # Paths are remote, and remote paths are POSIX
//!
//! Nothing here uses [`std::path`] for the remote side. On Windows it would join with a backslash and
//! call `/home/user` relative, and both are wrong for a path the server will interpret. The helpers
//! ([`join`], [`parent`], [`normalise`]) are POSIX-only and are tested as such -- on both platforms,
//! because the bug they exist to prevent only appears on one of them.

pub mod session;

pub use session::{FileCommand, FileEvent, FileSession};

use std::path::Path;

use bestterm_proto_ssh::SshConnection;
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum SftpError {
    /// The channel could not be opened, or the server has no `sftp` subsystem.
    #[error("could not start SFTP on this connection: {0}")]
    Channel(#[from] bestterm_proto_ssh::SshError),
    /// The server refused something, or the protocol went wrong.
    #[error("sftp: {0}")]
    Protocol(#[from] russh_sftp::client::error::Error),
    /// A local file could not be read or written.
    #[error("{path}: {source}")]
    Local {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

/// A result from this crate.
pub type Result<T> = std::result::Result<T, SftpError>;

/// What kind of thing an entry is.
///
/// A symlink is its own kind rather than resolved to what it points at: a browser has to draw the
/// difference, and following one to decide costs a round trip per entry on a directory that may hold
/// thousands. Whoever wants the target asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A directory.
    Directory,
    /// A regular file.
    File,
    /// A symbolic link, unresolved.
    Symlink,
    /// A socket, a device, a fifo -- something a file browser can list and not open.
    Other,
}

/// One line in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name within its directory, with no path attached.
    pub name: String,
    /// What it is.
    pub kind: EntryKind,
    /// Size in bytes. Zero for the kinds that do not have one.
    pub size: u64,
    /// Last modification, in seconds since the Unix epoch, when the server said.
    pub modified: Option<u32>,
    /// The POSIX mode, when the server said.
    pub permissions: Option<u32>,
    /// Owner name, when the server sent one rather than only a number.
    pub owner: Option<String>,
    /// Group name, when the server sent one.
    pub group: Option<String>,
}

impl Entry {
    /// Whether this is something a browser can descend into.
    ///
    /// A symlink is not: it may point at a directory, and it may point at nothing. Finding out is a
    /// round trip, and the caller is the one who knows whether it is worth making.
    #[must_use]
    pub fn is_directory(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    /// Whether the name is one Unix hides.
    #[must_use]
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// An SFTP session on a connection somebody else owns.
///
/// Borrowing rather than owning the SSH connection is deliberate: the terminal beside this browser is
/// the reason the connection exists, and a file browser that could outlive it -- or close it -- would
/// be a browser that can hang up on somebody mid-command.
pub struct Sftp {
    session: SftpSession,
    /// What the connection is called, for messages.
    label: String,
}

impl std::fmt::Debug for Sftp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sftp").field("label", &self.label).finish()
    }
}

/// How much is read or written at once.
///
/// 32 KiB because that is the largest payload every SFTP server accepts in one packet: OpenSSH will
/// take more, and enough others will not that asking is not worth the round trips saved.
const CHUNK: usize = 32 * 1024;

impl Sftp {
    /// Start SFTP on an existing SSH connection.
    ///
    /// # Errors
    ///
    /// If the channel cannot be opened, or the server has no `sftp` subsystem -- which is worth
    /// telling people apart from a refused login, because an OpenSSH server with `Subsystem sftp`
    /// commented out keeps serving shells while every file browser fails.
    pub async fn open(connection: &SshConnection, label: impl Into<String>) -> Result<Self> {
        let stream = connection.open_sftp().await?;
        let session = SftpSession::new(stream).await?;
        Ok(Self {
            session,
            label: label.into(),
        })
    }

    /// Where a browser should open: the account's own directory.
    ///
    /// Asked of the server rather than guessed from the user name. `/home/<user>` is wrong on macOS,
    /// wrong for a chrooted account, wrong for `root` on most systems, and wrong for anybody whose
    /// home was moved -- and it is the sort of wrong that looks like an empty server.
    ///
    /// # Errors
    ///
    /// If the server cannot resolve `.`, which in practice means the session is already gone.
    pub async fn home(&self) -> Result<String> {
        Ok(self.session.canonicalize(".").await?)
    }

    /// Resolve a path the way the server would, following symlinks and `..`.
    ///
    /// # Errors
    ///
    /// If the path does not exist, or is not readable.
    pub async fn resolve(&self, path: &str) -> Result<String> {
        Ok(self.session.canonicalize(path).await?)
    }

    /// List a directory, ordered the way a browser shows it.
    ///
    /// `.` and `..` are dropped: every browser draws its own way back up, and a list that contains
    /// itself is a list that can be descended into forever.
    ///
    /// # Errors
    ///
    /// If the directory cannot be read -- which for a file browser is usually a permission the
    /// account does not have, not a broken session.
    pub async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for entry in self.session.read_dir(path).await? {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let attributes = entry.metadata();
            let kind = match entry.file_type() {
                russh_sftp::protocol::FileType::Dir => EntryKind::Directory,
                russh_sftp::protocol::FileType::File => EntryKind::File,
                russh_sftp::protocol::FileType::Symlink => EntryKind::Symlink,
                russh_sftp::protocol::FileType::Other => EntryKind::Other,
            };
            entries.push(Entry {
                name,
                kind,
                size: attributes.size.unwrap_or(0),
                modified: attributes.mtime,
                permissions: attributes.permissions,
                owner: attributes.user.clone(),
                group: attributes.group.clone(),
            });
        }
        order(&mut entries);
        Ok(entries)
    }

    /// What a single path is.
    ///
    /// # Errors
    ///
    /// If it does not exist or cannot be reached.
    pub async fn about(&self, path: &str) -> Result<Entry> {
        let attributes = self.session.metadata(path).await?;
        let kind = if attributes.is_dir() {
            EntryKind::Directory
        } else if attributes.is_symlink() {
            EntryKind::Symlink
        } else if attributes.is_regular() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Ok(Entry {
            name: base_name(path).to_owned(),
            kind,
            size: attributes.size.unwrap_or(0),
            modified: attributes.mtime,
            permissions: attributes.permissions,
            owner: attributes.user.clone(),
            group: attributes.group.clone(),
        })
    }

    /// Where a symlink points, unresolved -- one hop, as the server stored it.
    ///
    /// # Errors
    ///
    /// If the path is not a symlink, or cannot be read.
    pub async fn link_target(&self, path: &str) -> Result<String> {
        Ok(self.session.read_link(path).await?)
    }

    /// Make a directory.
    ///
    /// # Errors
    ///
    /// If it exists already, or the parent is not writable.
    pub async fn make_directory(&self, path: &str) -> Result<()> {
        self.session.create_dir(path).await?;
        Ok(())
    }

    /// Rename, which is also how a file is moved within one server.
    ///
    /// # Errors
    ///
    /// If the source does not exist, the destination does, or either parent is not writable. Servers
    /// differ on whether renaming across filesystems works at all; the ones that refuse say so here
    /// rather than copying behind the caller's back.
    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.session.rename(from, to).await?;
        Ok(())
    }

    /// Delete a file.
    ///
    /// # Errors
    ///
    /// If it does not exist, is a directory, or the parent is not writable.
    pub async fn remove_file(&self, path: &str) -> Result<()> {
        self.session.remove_file(path).await?;
        Ok(())
    }

    /// Delete an empty directory.
    ///
    /// Only an empty one: SFTP has no recursive delete, and a helpful one built in here would delete
    /// a tree from a single click with no way to see what was in it first. Whoever wants that walks
    /// the tree itself, where it can be shown and confirmed.
    ///
    /// # Errors
    ///
    /// If it is not empty, does not exist, or the parent is not writable.
    pub async fn remove_directory(&self, path: &str) -> Result<()> {
        self.session.remove_dir(path).await?;
        Ok(())
    }

    /// Copy a remote file here, continuing an interrupted one where it stopped.
    ///
    /// `progress` is called with the number of bytes transferred so far and the total when the server
    /// gave one. It runs on the transfer's own task, so it must not block -- a channel send or a
    /// counter, not a repaint.
    ///
    /// Resuming is by size, which is what every file manager does and is worth being honest about: a
    /// file that was *changed* rather than truncated resumes into a mixture of both versions. The
    /// alternative is hashing what is already there, which costs a full read of both sides -- the
    /// thing resuming exists to avoid.
    ///
    /// # Errors
    ///
    /// If the remote file cannot be read, or the local one cannot be written.
    pub async fn download(
        &self,
        remote: &str,
        local: &Path,
        resume: bool,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<u64> {
        let total = self
            .session
            .metadata(remote)
            .await
            .ok()
            .and_then(|a| a.size);

        let already = if resume {
            tokio::fs::metadata(local)
                .await
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };

        let mut source = self.session.open(remote).await?;
        if already > 0 {
            source
                .seek(std::io::SeekFrom::Start(already))
                .await
                .map_err(|source| SftpError::Local {
                    path: remote.to_owned(),
                    source,
                })?;
        }

        let mut sink = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(already > 0)
            .truncate(already == 0)
            .open(local)
            .await
            .map_err(|source| SftpError::Local {
                path: local.display().to_string(),
                source,
            })?;

        let mut done = already;
        let mut buffer = vec![0_u8; CHUNK];
        progress(done, total);
        loop {
            let read = source
                .read(&mut buffer)
                .await
                .map_err(|source| SftpError::Local {
                    path: remote.to_owned(),
                    source,
                })?;
            if read == 0 {
                break;
            }
            sink.write_all(&buffer[..read])
                .await
                .map_err(|source| SftpError::Local {
                    path: local.display().to_string(),
                    source,
                })?;
            done += read as u64;
            progress(done, total);
        }
        sink.flush().await.map_err(|source| SftpError::Local {
            path: local.display().to_string(),
            source,
        })?;
        Ok(done)
    }

    /// Copy a local file to the server, continuing an interrupted one where it stopped.
    ///
    /// The same resume-by-size caveat as [`Sftp::download`] applies, in the same direction.
    ///
    /// # Errors
    ///
    /// If the local file cannot be read, or the remote one cannot be written.
    pub async fn upload(
        &self,
        local: &Path,
        remote: &str,
        resume: bool,
        progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
    ) -> Result<u64> {
        let total = tokio::fs::metadata(local).await.ok().map(|m| m.len());

        let already = if resume {
            self.session
                .metadata(remote)
                .await
                .ok()
                .and_then(|a| a.size)
                .unwrap_or(0)
        } else {
            0
        };

        let mut source = tokio::fs::File::open(local)
            .await
            .map_err(|source| SftpError::Local {
                path: local.display().to_string(),
                source,
            })?;
        if already > 0 {
            source
                .seek(std::io::SeekFrom::Start(already))
                .await
                .map_err(|source| SftpError::Local {
                    path: local.display().to_string(),
                    source,
                })?;
        }

        // Opened for writing and positioned, rather than opened in append mode: a server that ignores
        // the append flag would otherwise silently write the tail twice, and a seek that the server
        // refuses fails loudly instead.
        let mut sink = self.session.create(remote).await?;
        if already > 0 {
            sink.seek(std::io::SeekFrom::Start(already))
                .await
                .map_err(|source| SftpError::Local {
                    path: remote.to_owned(),
                    source,
                })?;
        }

        let mut done = already;
        let mut buffer = vec![0_u8; CHUNK];
        progress(done, total);
        loop {
            let read = source
                .read(&mut buffer)
                .await
                .map_err(|source| SftpError::Local {
                    path: local.display().to_string(),
                    source,
                })?;
            if read == 0 {
                break;
            }
            sink.write_all(&buffer[..read])
                .await
                .map_err(|source| SftpError::Local {
                    path: remote.to_owned(),
                    source,
                })?;
            done += read as u64;
            progress(done, total);
        }
        sink.flush().await.map_err(|source| SftpError::Local {
            path: remote.to_owned(),
            source,
        })?;
        Ok(done)
    }

    /// Close the SFTP channel, leaving the SSH connection alone.
    ///
    /// # Errors
    ///
    /// If the server objects. Usually ignorable: the channel is going away regardless.
    pub async fn close(self) -> Result<()> {
        self.session.close().await?;
        Ok(())
    }
}

/// Order a listing the way a file browser shows one: directories first, then by name.
///
/// Case-insensitively, and then by the bytes for names that differ only in case, so the order is
/// total -- `Makefile` and `makefile` are both real and both have to sit somewhere fixed, or they
/// swap places between listings and look like the directory changed.
pub fn order(entries: &mut [Entry]) {
    entries.sort_by(|left, right| {
        right
            .is_directory()
            .cmp(&left.is_directory())
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
}

/// Join a remote directory and a name, POSIX-style.
///
/// Not [`std::path::Path::join`]: on Windows that joins with a backslash, which a server reads as
/// part of the file name. An absolute `name` replaces the base, which is what a browser's address
/// field does when somebody types a path into it.
#[must_use]
pub fn join(base: &str, name: &str) -> String {
    if name.starts_with('/') {
        return normalise(name);
    }
    if base.is_empty() || base == "/" {
        return normalise(&format!("/{name}"));
    }
    normalise(&format!("{}/{name}", base.trim_end_matches('/')))
}

/// The directory containing `path`, or `None` for the root, which has no parent.
#[must_use]
pub fn parent(path: &str) -> Option<String> {
    let path = normalise(path);
    if path == "/" {
        return None;
    }
    let cut = path.rfind('/')?;
    Some(if cut == 0 {
        "/".to_owned()
    } else {
        path[..cut].to_owned()
    })
}

/// The last component of a path, with no trailing slash.
#[must_use]
pub fn base_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(cut) => &trimmed[cut + 1..],
        None => trimmed,
    }
}

/// Tidy a remote path: one slash between components, `.` dropped, `..` applied.
///
/// `..` is resolved here rather than left for the server because a browser has to show where it is
/// about to go before it goes there. Above the root it stops at the root, which is what a POSIX
/// server does with `/..` too.
#[must_use]
pub fn normalise(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                // A leading `..` on a relative path has nowhere to go and has to be kept, or
                // `../sibling` would quietly become `sibling`.
                if matches!(parts.last(), Some(&last) if last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_owned()
    } else {
        joined
    }
}

/// A POSIX mode as `ls` writes it: `drwxr-xr-x`.
///
/// The kind comes from the entry rather than the mode's own type bits, because not every server
/// sends them -- and an entry whose kind says directory while its mode says file is the sort of
/// disagreement that should show as the kind the listing sorted by.
#[must_use]
pub fn mode_string(kind: EntryKind, mode: Option<u32>) -> String {
    let leading = match kind {
        EntryKind::Directory => 'd',
        EntryKind::Symlink => 'l',
        EntryKind::File => '-',
        EntryKind::Other => '?',
    };
    let Some(mode) = mode else {
        // Nine question marks, not nine dashes: "not told" and "no permissions at all" are different
        // facts, and a listing that showed them the same way would be inventing one.
        return format!("{leading}?????????");
    };

    let mut out = String::with_capacity(10);
    out.push(leading);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        out.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        out.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        out.push(if bits & 0o1 != 0 { 'x' } else { '-' });
    }
    out
}

/// A size a person can read at a glance.
///
/// Powers of 1024 with the units spelled the way `ls -h` spells them, because that is what people
/// comparing this window with a terminal beside it will be reading.
#[must_use]
pub fn human_size(bytes: u64) -> String {
    // Up to exbibytes, which is as far as a `u64` of bytes reaches. Stopping at P would print a
    // number like `16384 P` for a size this function was handed and could not express.
    const UNITS: [&str; 7] = ["B", "K", "M", "G", "T", "P", "E"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    // One decimal below 10, none above: `9.4 M` and `327 M` are both three characters of information,
    // and `327.4 M` is two of them plus noise.
    if size < 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.to_owned(),
            kind,
            size: 0,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
        }
    }

    #[test]
    fn a_remote_path_is_joined_with_a_forward_slash_on_every_platform() {
        // The bug this exists for only appears on Windows, where `Path::join` produces a backslash
        // that a server reads as part of the file name -- so the test runs on both.
        assert_eq!(
            join("/home/someone", "notes.txt"),
            "/home/someone/notes.txt"
        );
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("", "etc"), "/etc");
        assert_eq!(
            join("/home/someone/", "notes.txt"),
            "/home/someone/notes.txt"
        );
        // Typed into an address field: an absolute name replaces the base rather than hanging off it.
        assert_eq!(join("/home/someone", "/var/log"), "/var/log");
        assert_eq!(join("/home/someone", ".."), "/home");
    }

    #[test]
    fn dot_dot_is_applied_before_anybody_is_shown_where_they_are_going() {
        assert_eq!(normalise("/home/someone/../else"), "/home/else");
        assert_eq!(normalise("/home//someone/./notes"), "/home/someone/notes");
        assert_eq!(normalise("/home/someone/"), "/home/someone");
        // Above the root there is only the root, which is what a POSIX server does with `/..`.
        assert_eq!(normalise("/.."), "/");
        assert_eq!(normalise("/../.."), "/");
        assert_eq!(normalise("/"), "/");
        // A relative `..` has nowhere to go and has to survive, or `../sibling` becomes `sibling`
        // and points somewhere else entirely.
        assert_eq!(normalise("../sibling"), "../sibling");
        assert_eq!(normalise("../../x"), "../../x");
        assert_eq!(normalise("."), ".");
    }

    #[test]
    fn the_root_has_no_parent_and_everything_else_does() {
        assert_eq!(
            parent("/home/someone/notes.txt").as_deref(),
            Some("/home/someone")
        );
        assert_eq!(parent("/home").as_deref(), Some("/"));
        assert_eq!(parent("/"), None);
        // A trailing slash does not make a directory its own parent.
        assert_eq!(parent("/home/someone/").as_deref(), Some("/home"));
    }

    #[test]
    fn a_name_is_the_last_component_whether_or_not_it_ends_in_a_slash() {
        assert_eq!(base_name("/home/someone/notes.txt"), "notes.txt");
        assert_eq!(base_name("/home/someone/"), "someone");
        assert_eq!(base_name("notes.txt"), "notes.txt");
        assert_eq!(base_name("/"), "");
    }

    #[test]
    fn directories_come_first_and_the_order_is_total() {
        let mut entries = vec![
            entry("zebra.txt", EntryKind::File),
            entry("Applications", EntryKind::Directory),
            entry("makefile", EntryKind::File),
            entry("Makefile", EntryKind::File),
            entry("apples.txt", EntryKind::File),
            entry("bin", EntryKind::Directory),
            entry("link", EntryKind::Symlink),
        ];
        order(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Applications",
                "bin",
                "apples.txt",
                "link",
                "Makefile",
                "makefile",
                "zebra.txt"
            ]
        );

        // Sorted again, the result has to be identical. Two names that differ only in case compare
        // equal case-insensitively, and without the tie-break they would swap places between
        // listings -- which looks like the directory changed under somebody.
        let mut again = entries.clone();
        order(&mut again);
        assert_eq!(again, entries);

        // A symlink is not a directory even when it points at one: deciding otherwise costs a round
        // trip per entry, and this listing may hold thousands.
        assert!(!entry("link", EntryKind::Symlink).is_directory());
    }

    #[test]
    fn a_mode_reads_the_way_ls_writes_it() {
        assert_eq!(mode_string(EntryKind::Directory, Some(0o755)), "drwxr-xr-x");
        assert_eq!(mode_string(EntryKind::File, Some(0o644)), "-rw-r--r--");
        assert_eq!(mode_string(EntryKind::File, Some(0o600)), "-rw-------");
        assert_eq!(mode_string(EntryKind::Symlink, Some(0o777)), "lrwxrwxrwx");
        // The type bits servers do send are ignored: the kind the listing sorted by is the kind it
        // shows, or the two disagree on screen.
        assert_eq!(
            mode_string(EntryKind::Directory, Some(0o040_755)),
            "drwxr-xr-x"
        );
        // Not told is not the same fact as no permissions, and must not be drawn as if it were.
        assert_eq!(mode_string(EntryKind::File, None), "-?????????");
    }

    #[test]
    fn sizes_are_readable_at_a_glance() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 K");
        assert_eq!(human_size(1536), "1.5 K");
        assert_eq!(human_size(10 * 1024), "10 K");
        assert_eq!(human_size(343_244_800), "327 M");
        assert_eq!(human_size(u64::MAX), "16 E");
    }

    #[test]
    fn a_dotfile_is_hidden_and_a_file_with_a_dot_in_it_is_not() {
        assert!(entry(".bashrc", EntryKind::File).is_hidden());
        assert!(!entry("notes.txt", EntryKind::File).is_hidden());
        assert!(!entry("", EntryKind::File).is_hidden());
    }
}
