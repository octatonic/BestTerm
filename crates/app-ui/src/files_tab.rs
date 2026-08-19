//! A two-panel file browser on an SSH session.
//!
//! The third kind of pane, and the one the plan named as the release condition for 1.0. Local on the
//! left, remote on the right, transfers along the bottom -- the arrangement every file manager of this
//! kind has used for thirty years, because it is the one where "copy that there" is a single gesture
//! with both ends visible.
//!
//! # What is on which thread
//!
//! The remote side is a [`bestterm_proto_sftp::FileSession`]: commands out, events in, nothing awaited
//! while drawing. The local side is read with [`std::fs`] on the drawing thread, which is a deliberate
//! asymmetry -- a local directory is a syscall away, and putting it behind a channel would add a frame
//! of latency to every keystroke in the path box to guard against a case (a stalled network mount)
//! that a spinner cannot rescue anyway.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bestterm_proto_sftp::{Entry, EntryKind, FileCommand, FileEvent, FileSession, human_size};
use bestterm_proto_ssh::SshConnection;
use bestterm_ui_chrome::ChromeTheme;

use crate::tunnels::ConnectionId;

/// One transfer, running or finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Transfer {
    /// Matches the id in the events.
    pub(crate) id: u64,
    /// What to call it: the file's name, not its path.
    pub(crate) name: String,
    /// Which way it is going.
    pub(crate) upload: bool,
    /// Bytes moved.
    pub(crate) done: u64,
    /// Bytes in total, when known.
    pub(crate) total: Option<u64>,
    /// How it ended, once it has.
    pub(crate) outcome: Option<Outcome>,
}

/// How a transfer ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// It finished, with this many bytes.
    Finished(u64),
    /// It failed, and this is what to say.
    Failed(String),
}

impl Transfer {
    /// How far along, when that is knowable.
    ///
    /// `None` for a transfer whose size the server did not give: a bar that guessed would be a bar
    /// that lies, and an indeterminate one at least says "still going".
    fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            // A zero-byte file is finished the moment it starts, and dividing by its size is not the
            // way to say so.
            return Some(1.0);
        }
        #[allow(clippy::cast_precision_loss)]
        Some((self.done as f32 / total as f32).clamp(0.0, 1.0))
    }
}

/// A local directory entry, in the same shape as a remote one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalEntry {
    /// The name within its directory.
    pub(crate) name: String,
    /// Whether it can be descended into.
    pub(crate) directory: bool,
    /// Size in bytes, zero for directories.
    pub(crate) size: u64,
}

/// A two-panel file browser.
pub(crate) struct FilesTab {
    session: FileSession,
    events: crossbeam_channel::Receiver<FileEvent>,
    title: String,
    /// The connection this browser runs on, when the browser is what opened it.
    ///
    /// Held so it cannot be dropped while the browser is open: dropping the last owner closes the
    /// TCP connection, and a browser drawing a listing it can no longer act on is worse than one that
    /// says the session ended.
    ///
    /// `None` when somebody else owns it -- a browser docked into an existing session tab, which is
    /// how the reference arranges it, does not own the connection its terminal is using.
    _owner: Option<Arc<SshConnection>>,
    /// Which SSH connection this belongs to, for the tunnels view and the status bar.
    pub(crate) connection: Option<ConnectionId>,

    /// Where the remote panel is, once the server has said where the account starts.
    remote_path: Option<String>,
    remote: Vec<Entry>,
    remote_selected: Option<usize>,
    /// A listing has been asked for and has not arrived.
    waiting: bool,

    local_path: PathBuf,
    local: Vec<LocalEntry>,
    local_selected: Option<usize>,

    transfers: Vec<Transfer>,
    next_id: u64,
    /// Whether names beginning with a dot are shown, on both sides.
    show_hidden: bool,
    /// Problems, newest last.
    pub(crate) notices: Vec<String>,
    /// Set once the session has ended.
    closed: bool,
}

impl std::fmt::Debug for FilesTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesTab")
            .field("title", &self.title)
            .field("remote_path", &self.remote_path)
            .field("entries", &self.remote.len())
            .field("transfers", &self.transfers.len())
            .finish_non_exhaustive()
    }
}

impl FilesTab {
    /// Take over a file session somebody else started.
    pub(crate) fn adopt(
        session: FileSession,
        events: crossbeam_channel::Receiver<FileEvent>,
        title: String,
        owner: Option<Arc<SshConnection>>,
        connection: Option<ConnectionId>,
    ) -> Self {
        let local_path = starting_local_directory();
        let mut tab = Self {
            session,
            events,
            title,
            _owner: owner,
            connection,
            remote_path: None,
            remote: Vec::new(),
            remote_selected: None,
            waiting: true,
            local_path,
            local: Vec::new(),
            local_selected: None,
            transfers: Vec::new(),
            next_id: 1,
            show_hidden: false,
            notices: Vec::new(),
            closed: false,
        };
        tab.read_local();
        tab
    }

    /// What goes on the tab.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// One line for the status bar.
    pub(crate) fn status_line(&self) -> String {
        if self.closed {
            return format!("{} — closed", self.title);
        }
        let running = self
            .transfers
            .iter()
            .filter(|transfer| transfer.outcome.is_none())
            .count();
        let where_it_is = self.remote_path.as_deref().unwrap_or("connecting…");
        if running > 0 {
            format!(
                "sftp {} — {} — {running} transferring",
                self.title, where_it_is
            )
        } else {
            format!("sftp {} — {}", self.title, where_it_is)
        }
    }

    /// Whether the session has ended.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    /// Take whatever the session has reported. True if anything changed.
    pub(crate) fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            self.apply(event);
            changed = true;
        }
        changed
    }

    /// Fold one event into the state.
    ///
    /// Separated from the drawing so it can be tested without a server: every interesting thing this
    /// pane does to itself happens here.
    pub(crate) fn apply(&mut self, event: FileEvent) {
        match event {
            FileEvent::Ready { home } => {
                self.remote_path = Some(home.clone());
                self.ask_for(&home);
            }
            FileEvent::Listing { path, entries } => {
                // Only if it is the directory we are actually looking at. Two listings can be in
                // flight after a fast double-click, and the slower one must not overwrite the newer.
                if self.remote_path.as_deref() == Some(path.as_str()) {
                    self.remote = entries;
                    self.remote_selected = None;
                    self.waiting = false;
                }
            }
            FileEvent::Progress { id, done, total } => {
                if let Some(transfer) = self.transfers.iter_mut().find(|t| t.id == id) {
                    transfer.done = done;
                    transfer.total = total.or(transfer.total);
                }
            }
            FileEvent::Finished { id, bytes } => {
                if let Some(transfer) = self.transfers.iter_mut().find(|t| t.id == id) {
                    transfer.done = bytes;
                    transfer.total = Some(bytes);
                    transfer.outcome = Some(Outcome::Finished(bytes));
                }
                // Whichever side it landed on now has a file it did not have.
                self.refresh();
            }
            FileEvent::Done(said) => {
                self.notices.push(said);
                self.refresh();
            }
            FileEvent::Failed { what, why } => {
                // Attached to the transfer when it is one, so a failed copy shows as a failed copy
                // rather than only as a line in a list of notices.
                let attached = self.transfers.iter_mut().find(|transfer| {
                    transfer.outcome.is_none() && what.contains(transfer.name.as_str())
                });
                if let Some(transfer) = attached {
                    transfer.outcome = Some(Outcome::Failed(why.clone()));
                }
                self.notices.push(format!("{what}: {why}"));
                self.waiting = false;
            }
            FileEvent::Closed => {
                self.closed = true;
                // Anything still running has stopped, whatever it thought it was doing.
                for transfer in &mut self.transfers {
                    if transfer.outcome.is_none() {
                        transfer.outcome = Some(Outcome::Failed("the session ended".to_owned()));
                    }
                }
            }
        }
    }

    /// Ask the server for a directory and remember that we are waiting.
    fn ask_for(&mut self, path: &str) {
        self.waiting = true;
        if self
            .session
            .send(FileCommand::List(path.to_owned()))
            .is_err()
        {
            self.closed = true;
            self.waiting = false;
        }
    }

    /// Re-read both sides.
    fn refresh(&mut self) {
        if let Some(path) = self.remote_path.clone() {
            self.ask_for(&path);
        }
        self.read_local();
    }

    /// Go to a remote directory.
    fn go_remote(&mut self, path: String) {
        self.remote_path = Some(path.clone());
        self.remote.clear();
        self.remote_selected = None;
        self.ask_for(&path);
    }

    /// Read the local directory into `local`.
    ///
    /// A directory that cannot be read leaves the previous listing in place and says so: emptying the
    /// panel would look like an empty directory, which is a different fact.
    fn read_local(&mut self) {
        let reading = std::fs::read_dir(&self.local_path);
        let entries = match reading {
            Ok(entries) => entries,
            Err(error) => {
                self.notices
                    .push(format!("{}: {error}", self.local_path.display()));
                return;
            }
        };

        let mut listing = Vec::new();
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            listing.push(LocalEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                directory: kind.is_dir(),
                size: if kind.is_dir() { 0 } else { size },
            });
        }
        order_local(&mut listing);
        self.local = listing;
        self.local_selected = None;
    }

    /// Go to a local directory.
    fn go_local(&mut self, path: PathBuf) {
        self.local_path = path;
        self.read_local();
    }

    /// Start a transfer and record it.
    fn begin(&mut self, name: String, upload: bool, command: FileCommand) {
        let id = self.next_id;
        self.next_id += 1;
        if self.session.send(command).is_err() {
            self.notices.push(format!("{name}: the session has ended"));
            self.closed = true;
            return;
        }
        self.transfers.push(Transfer {
            id,
            name,
            upload,
            done: 0,
            total: None,
            outcome: None,
        });
    }

    /// Copy the selected remote file here.
    fn download_selected(&mut self) {
        let Some(entry) = self
            .remote_selected
            .and_then(|i| self.remote.get(i))
            .cloned()
        else {
            return;
        };
        if entry.kind == EntryKind::Directory {
            self.notices.push(format!(
                "{}: copying a whole directory is not implemented yet",
                entry.name
            ));
            return;
        }
        let Some(remote_dir) = self.remote_path.clone() else {
            return;
        };
        let remote = bestterm_proto_sftp::join(&remote_dir, &entry.name);
        let local = self.local_path.join(&entry.name);
        // Resuming when something of that name is already here and is shorter. Overwriting silently
        // would be worse, and asking is the interface's job once there is somewhere to ask.
        let resume = std::fs::metadata(&local).is_ok_and(|m| m.len() < entry.size);
        let id = self.next_id;
        self.begin(
            entry.name.clone(),
            false,
            FileCommand::Download {
                id,
                remote,
                local,
                resume,
            },
        );
    }

    /// Copy the selected local file to the server.
    fn upload_selected(&mut self) {
        let Some(entry) = self.local_selected.and_then(|i| self.local.get(i)).cloned() else {
            return;
        };
        if entry.directory {
            self.notices.push(format!(
                "{}: copying a whole directory is not implemented yet",
                entry.name
            ));
            return;
        }
        let Some(remote_dir) = self.remote_path.clone() else {
            return;
        };
        let remote = bestterm_proto_sftp::join(&remote_dir, &entry.name);
        let local = self.local_path.join(&entry.name);
        let resume = self
            .remote
            .iter()
            .find(|there| there.name == entry.name)
            .is_some_and(|there| there.size < entry.size);
        let id = self.next_id;
        self.begin(
            entry.name.clone(),
            true,
            FileCommand::Upload {
                id,
                local,
                remote,
                resume,
            },
        );
    }

    /// Delete whatever is selected on the remote side.
    fn delete_selected(&mut self) {
        let Some(entry) = self
            .remote_selected
            .and_then(|i| self.remote.get(i))
            .cloned()
        else {
            return;
        };
        let Some(remote_dir) = self.remote_path.clone() else {
            return;
        };
        let path = bestterm_proto_sftp::join(&remote_dir, &entry.name);
        let _ = self.session.send(FileCommand::Remove {
            path,
            directory: entry.kind == EntryKind::Directory,
        });
    }

    /// End the session.
    pub(crate) fn shutdown(&mut self) {
        let _ = self.session.send(FileCommand::Shutdown);
        self.closed = true;
    }

    /// Draw the whole thing.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, theme: &ChromeTheme) {
        if self.is_closed() {
            // Said, and the controls that need a server turned off. The panels stay: the local half is
            // still real, and the transfer list is the record of what did and did not get across --
            // which is exactly what somebody wants to see after a connection drops.
            ui.label(
                egui::RichText::new("The connection has ended. Nothing here can be changed.")
                    .color(theme.warning),
            );
            ui.add_space(4.0);
        }

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_hidden, "Show hidden");
            ui.add_space(12.0);
            let mut refresh = false;
            let mut upload = false;
            let mut download = false;
            let mut delete = false;
            ui.add_enabled_ui(!self.is_closed(), |ui| {
                refresh = ui.button("Refresh").clicked();
                ui.add_space(12.0);
                upload = ui.button("Upload →").clicked();
                download = ui.button("← Download").clicked();
                ui.add_space(12.0);
                delete = ui.button("Delete remote").clicked();
            });
            if refresh {
                self.refresh();
            }
            if upload {
                self.upload_selected();
            }
            if download {
                self.download_selected();
            }
            if delete {
                self.delete_selected();
            }
        });
        ui.separator();

        let available = ui.available_height();
        let transfers_height = if self.transfers.is_empty() {
            0.0
        } else {
            (available * 0.25).min(140.0)
        };
        let panels_height = (available - transfers_height - 12.0).max(80.0);

        ui.horizontal_top(|ui| {
            let half = (ui.available_width() - 8.0) / 2.0;
            ui.allocate_ui(egui::vec2(half, panels_height), |ui| {
                self.local_panel(ui, theme);
            });
            ui.separator();
            ui.allocate_ui(egui::vec2(half, panels_height), |ui| {
                self.remote_panel(ui, theme);
            });
        });

        if !self.transfers.is_empty() {
            ui.separator();
            self.transfer_list(ui, theme, transfers_height);
        }
    }

    /// The local half.
    fn local_panel(&mut self, ui: &mut egui::Ui, theme: &ChromeTheme) {
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(format!("Local — {}", self.local_path.display()))
                    .small()
                    .color(theme.text_dim),
            );

            let mut go: Option<PathBuf> = None;
            egui::ScrollArea::vertical()
                .id_salt("bestterm_files_local")
                .show(ui, |ui| {
                    if let Some(parent) = self.local_path.parent().map(Path::to_path_buf)
                        && ui.selectable_label(false, "..").double_clicked()
                    {
                        go = Some(parent);
                    }
                    for (index, entry) in self.local.iter().enumerate() {
                        if !self.show_hidden && entry.name.starts_with('.') {
                            continue;
                        }
                        let label = if entry.directory {
                            format!("[{}]", entry.name)
                        } else {
                            format!("{}   {}", entry.name, human_size(entry.size))
                        };
                        let response =
                            ui.selectable_label(self.local_selected == Some(index), label);
                        if response.clicked() {
                            self.local_selected = Some(index);
                        }
                        if response.double_clicked() {
                            if entry.directory {
                                go = Some(self.local_path.join(&entry.name));
                            } else {
                                self.local_selected = Some(index);
                            }
                        }
                    }
                });
            if let Some(path) = go {
                self.go_local(path);
            }
        });
    }

    /// The remote half.
    fn remote_panel(&mut self, ui: &mut egui::Ui, theme: &ChromeTheme) {
        ui.vertical(|ui| {
            let where_it_is = self.remote_path.as_deref().unwrap_or("connecting…");
            ui.label(
                egui::RichText::new(format!("Remote — {where_it_is}"))
                    .small()
                    .color(theme.text_dim),
            );

            if self.waiting && self.remote.is_empty() {
                ui.label(
                    egui::RichText::new("reading…")
                        .small()
                        .color(theme.text_dim),
                );
                return;
            }

            let mut go: Option<String> = None;
            let mut download: Option<usize> = None;
            egui::ScrollArea::vertical()
                .id_salt("bestterm_files_remote")
                .show(ui, |ui| {
                    if let Some(here) = self.remote_path.as_deref()
                        && let Some(up) = bestterm_proto_sftp::parent(here)
                        && ui.selectable_label(false, "..").double_clicked()
                    {
                        go = Some(up);
                    }
                    for (index, entry) in self.remote.iter().enumerate() {
                        if !self.show_hidden && entry.is_hidden() {
                            continue;
                        }
                        let label = match entry.kind {
                            EntryKind::Directory => format!("[{}]", entry.name),
                            EntryKind::Symlink => format!("{} →", entry.name),
                            _ => format!("{}   {}", entry.name, human_size(entry.size)),
                        };
                        let response =
                            ui.selectable_label(self.remote_selected == Some(index), label);
                        if response.clicked() {
                            self.remote_selected = Some(index);
                        }
                        if response.double_clicked() {
                            if entry.is_directory() {
                                if let Some(here) = self.remote_path.as_deref() {
                                    go = Some(bestterm_proto_sftp::join(here, &entry.name));
                                }
                            } else {
                                download = Some(index);
                            }
                        }
                    }
                });
            if let Some(path) = go {
                self.go_remote(path);
            }
            if let Some(index) = download {
                self.remote_selected = Some(index);
                self.download_selected();
            }
        });
    }

    /// The transfers along the bottom.
    fn transfer_list(&mut self, ui: &mut egui::Ui, theme: &ChromeTheme, height: f32) {
        egui::ScrollArea::vertical()
            .id_salt("bestterm_files_transfers")
            .max_height(height)
            .show(ui, |ui| {
                for transfer in &self.transfers {
                    ui.horizontal(|ui| {
                        ui.label(if transfer.upload { "up" } else { "down" });
                        ui.label(&transfer.name);
                        match &transfer.outcome {
                            Some(Outcome::Finished(bytes)) => {
                                ui.label(
                                    egui::RichText::new(format!("{} done", human_size(*bytes)))
                                        .small()
                                        .color(theme.text_dim),
                                );
                            }
                            Some(Outcome::Failed(why)) => {
                                ui.label(egui::RichText::new(why).small().color(theme.warning));
                            }
                            None => match transfer.fraction() {
                                Some(fraction) => {
                                    ui.add(
                                        egui::ProgressBar::new(fraction)
                                            .desired_width(180.0)
                                            .text(human_size(transfer.done)),
                                    );
                                }
                                None => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} so far",
                                            human_size(transfer.done)
                                        ))
                                        .small()
                                        .color(theme.text_dim),
                                    );
                                }
                            },
                        }
                    });
                }
            });
    }
}

/// Order a local listing the same way a remote one is ordered.
///
/// The same rule in both panels, because two panels sorted differently is two panels somebody has to
/// read differently.
fn order_local(entries: &mut [LocalEntry]) {
    entries.sort_by(|left, right| {
        right
            .directory
            .cmp(&left.directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
}

/// Where the local panel opens.
///
/// The account's own directory, falling back to whatever the process is in -- which is at least
/// somewhere that exists, unlike a hard-coded path.
fn starting_local_directory() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");

    home.map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tab with no connection behind it, for testing what events do to its state.
    ///
    /// The session handle is a real one whose receiver has been dropped, which is what a closed
    /// session looks like -- so commands this tab tries to send fail the way they would in the wild
    /// rather than being silently accepted by a stub.
    fn detached() -> FilesTab {
        let (_events, events) = crossbeam_channel::unbounded();
        FilesTab {
            session: FileSession::closed(),
            events,
            title: "someone@host".to_owned(),
            _owner: None,
            connection: None,
            remote_path: None,
            remote: Vec::new(),
            remote_selected: None,
            waiting: true,
            local_path: std::env::temp_dir(),
            local: Vec::new(),
            local_selected: None,
            transfers: Vec::new(),
            next_id: 1,
            show_hidden: false,
            notices: Vec::new(),
            closed: false,
        }
    }

    fn remote_entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.to_owned(),
            kind,
            size,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
        }
    }

    #[test]
    fn a_listing_for_a_directory_nobody_is_looking_at_is_dropped() {
        // Two listings can be in flight at once -- a double-click while the first is still coming --
        // and the slower answer arriving second would put the previous directory's contents under the
        // newer directory's name. The path is the only thing that can tell them apart.
        let mut tab = detached();
        tab.apply(FileEvent::Ready {
            home: "/home/someone".to_owned(),
        });
        assert_eq!(tab.remote_path.as_deref(), Some("/home/someone"));

        tab.apply(FileEvent::Listing {
            path: "/var/log".to_owned(),
            entries: vec![remote_entry("syslog", EntryKind::File, 10)],
        });
        assert!(
            tab.remote.is_empty(),
            "a listing of somewhere else must not be shown as this directory"
        );

        tab.apply(FileEvent::Listing {
            path: "/home/someone".to_owned(),
            entries: vec![remote_entry("notes", EntryKind::File, 3)],
        });
        assert_eq!(tab.remote.len(), 1);
        assert!(!tab.waiting, "the listing it was waiting for arrived");
    }

    #[test]
    fn progress_belongs_to_its_own_transfer() {
        // Two transfers at once is the normal case, and an event applied to the wrong one draws a bar
        // moving on a file that is not moving.
        let mut tab = detached();
        tab.transfers = vec![
            Transfer {
                id: 1,
                name: "first".to_owned(),
                upload: false,
                done: 0,
                total: None,
                outcome: None,
            },
            Transfer {
                id: 2,
                name: "second".to_owned(),
                upload: true,
                done: 0,
                total: None,
                outcome: None,
            },
        ];

        tab.apply(FileEvent::Progress {
            id: 2,
            done: 4096,
            total: Some(8192),
        });
        assert_eq!(tab.transfers[0].done, 0, "the other one has not moved");
        assert_eq!(tab.transfers[1].done, 4096);
        assert_eq!(tab.transfers[1].fraction(), Some(0.5));

        // An event for a transfer that is not here is ignored rather than panicking: a stale event
        // after a cleared list is possible, and it is not worth a crash.
        tab.apply(FileEvent::Progress {
            id: 99,
            done: 1,
            total: None,
        });

        tab.apply(FileEvent::Finished { id: 2, bytes: 8192 });
        assert_eq!(tab.transfers[1].outcome, Some(Outcome::Finished(8192)));
        assert_eq!(tab.transfers[0].outcome, None);
    }

    #[test]
    fn a_failure_lands_on_the_transfer_it_belongs_to_and_is_also_said_out_loud() {
        // A failed copy has to show as a failed copy. A line in a list of messages is not enough when
        // the thing that failed has a row of its own with a bar on it.
        let mut tab = detached();
        tab.transfers = vec![Transfer {
            id: 1,
            name: "payload.tar".to_owned(),
            upload: true,
            done: 512,
            total: Some(4096),
            outcome: None,
        }];

        tab.apply(FileEvent::Failed {
            what: "uploading /home/someone/payload.tar".to_owned(),
            why: "permission denied".to_owned(),
        });
        assert_eq!(
            tab.transfers[0].outcome,
            Some(Outcome::Failed("permission denied".to_owned()))
        );
        assert_eq!(tab.notices.len(), 1, "and it is in the messages too");
    }

    #[test]
    fn the_session_ending_stops_everything_that_was_running() {
        // Otherwise a bar sits at 40% for the rest of the session, which reads as a transfer that is
        // still going when the connection it was going over has gone.
        let mut tab = detached();
        tab.transfers = vec![
            Transfer {
                id: 1,
                name: "running".to_owned(),
                upload: false,
                done: 40,
                total: Some(100),
                outcome: None,
            },
            Transfer {
                id: 2,
                name: "already-done".to_owned(),
                upload: false,
                done: 100,
                total: Some(100),
                outcome: Some(Outcome::Finished(100)),
            },
        ];

        tab.apply(FileEvent::Closed);
        assert!(tab.is_closed());
        assert!(matches!(tab.transfers[0].outcome, Some(Outcome::Failed(_))));
        assert_eq!(
            tab.transfers[1].outcome,
            Some(Outcome::Finished(100)),
            "one that had already finished keeps its result"
        );
        assert!(tab.status_line().contains("closed"));
    }

    #[test]
    fn the_status_line_says_how_many_transfers_are_running() {
        let mut tab = detached();
        // Set rather than applied. `detached()` holds a session that refuses commands, and going
        // through `Ready` would try to ask for a listing, fail, and mark the tab closed -- which is
        // the right behaviour for a connection that has gone, and not what this test is about.
        tab.remote_path = Some("/home/someone".to_owned());
        assert!(
            tab.status_line().contains("/home/someone"),
            "{}",
            tab.status_line()
        );
        assert!(!tab.status_line().contains("transferring"));

        tab.transfers = vec![Transfer {
            id: 1,
            name: "a-file".to_owned(),
            upload: false,
            done: 0,
            total: None,
            outcome: None,
        }];
        assert!(tab.status_line().contains("1 transferring"));
    }

    #[test]
    fn a_local_listing_is_ordered_the_way_the_remote_one_is() {
        // Two panels sorted differently is two panels that have to be read differently, so the rule
        // is the same one: directories first, case-insensitive, ties broken by the bytes so the order
        // is total.
        let mut entries = vec![
            LocalEntry {
                name: "zebra.txt".to_owned(),
                directory: false,
                size: 1,
            },
            LocalEntry {
                name: "Documents".to_owned(),
                directory: true,
                size: 0,
            },
            LocalEntry {
                name: "makefile".to_owned(),
                directory: false,
                size: 2,
            },
            LocalEntry {
                name: "Makefile".to_owned(),
                directory: false,
                size: 3,
            },
            LocalEntry {
                name: "apples".to_owned(),
                directory: true,
                size: 0,
            },
        ];
        order_local(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["apples", "Documents", "Makefile", "makefile", "zebra.txt"]
        );

        let mut again = entries.clone();
        order_local(&mut again);
        assert_eq!(again, entries, "the order has to be stable across sorts");
    }

    #[test]
    fn the_local_panel_opens_somewhere_that_exists() {
        // The fallback chain matters more than which link answers: a panel that opens on a path that
        // is not there shows an error instead of a directory, on the first frame, every time.
        let start = starting_local_directory();
        assert!(
            start.is_dir(),
            "the starting directory has to exist: {}",
            start.display()
        );
    }

    #[test]
    fn a_bar_without_a_total_is_indeterminate_rather_than_wrong() {
        // Some servers do not report a size. A bar that guessed would be a bar that lies.
        let mut transfer = Transfer {
            id: 1,
            name: "a-file".to_owned(),
            upload: false,
            done: 4096,
            total: None,
            outcome: None,
        };
        assert_eq!(transfer.fraction(), None);

        transfer.total = Some(8192);
        assert_eq!(transfer.fraction(), Some(0.5));

        // A zero-byte file is finished the moment it starts, and dividing by its size is not the way
        // to say so.
        transfer.done = 0;
        transfer.total = Some(0);
        assert_eq!(transfer.fraction(), Some(1.0));

        // A server that reports fewer bytes than arrive must not produce a bar past its end.
        transfer.done = 9000;
        transfer.total = Some(8192);
        assert_eq!(transfer.fraction(), Some(1.0));
    }
}
