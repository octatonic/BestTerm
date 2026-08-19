//! A file session an interface can drive without blocking.
//!
//! [`Sftp`] is async and every method on it is a round trip. An immediate-mode interface cannot await
//! anything: it has one frame's worth of time and has to draw whatever it already knows. So the work
//! happens on a task, commands go in, events come out, and the interface draws the last thing it was
//! told.
//!
//! The same shape as the SSH transport and the graphical surfaces, for the same reason: it is the only
//! arrangement in which a slow server makes the file list stale rather than making the window stop
//! responding.
//!
//! # Progress is throttled, deliberately
//!
//! A transfer reads in 32 KiB chunks, so a one-gigabyte file is thirty-two thousand chunks. Reporting
//! each one would put thirty-two thousand events through a channel and ask for thirty-two thousand
//! repaints to draw a bar that has moved a thousandth of a pixel. [`PROGRESS_INTERVAL`] is how often
//! the interface hears about it instead. The first is always sent, so a bar appears at zero the
//! moment a transfer starts; the last is not, because [`FileEvent::Finished`] carries the final size
//! and a bar drawn from progress alone still ends full.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bestterm_proto_ssh::SshConnection;
use tokio::sync::mpsc;

use crate::{Entry, Sftp};

/// How often a running transfer reports in.
///
/// A tenth of a second: fast enough that a bar looks continuous, slow enough that a fast local
/// transfer does not spend its time waking the interface instead of copying.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Something to do on the server.
#[derive(Debug)]
pub enum FileCommand {
    /// List a directory and report it.
    List(String),
    /// Create a directory.
    MakeDirectory(String),
    /// Rename, which within one server is also a move.
    Rename {
        /// The existing path.
        from: String,
        /// The new one.
        to: String,
    },
    /// Delete one thing.
    Remove {
        /// What to delete.
        path: String,
        /// Whether it is a directory. Deleting one is a different operation, and getting it wrong is
        /// an error rather than a surprise, which is why the caller says which it meant.
        directory: bool,
    },
    /// Copy a remote file here.
    Download {
        /// Which transfer this is, so its progress can be told from another's.
        id: u64,
        /// The remote path.
        remote: String,
        /// Where to put it.
        local: PathBuf,
        /// Continue an interrupted one rather than starting again.
        resume: bool,
    },
    /// Copy a local file to the server.
    Upload {
        /// Which transfer this is.
        id: u64,
        /// The local path.
        local: PathBuf,
        /// Where to put it.
        remote: String,
        /// Continue an interrupted one.
        resume: bool,
    },
    /// Close the channel and stop.
    Shutdown,
}

/// Something that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEvent {
    /// The session is up, and this is where the account starts.
    Ready {
        /// The home directory, as the server resolved it.
        home: String,
    },
    /// A directory was read.
    Listing {
        /// Which directory, canonical as the request named it.
        path: String,
        /// What is in it, ordered the way a browser shows it.
        entries: Vec<Entry>,
    },
    /// A transfer moved.
    Progress {
        /// Which transfer.
        id: u64,
        /// Bytes done.
        done: u64,
        /// Bytes in total, when the size was known.
        total: Option<u64>,
    },
    /// A transfer finished.
    Finished {
        /// Which transfer.
        id: u64,
        /// How long the file turned out to be.
        bytes: u64,
    },
    /// An operation with nothing to return succeeded. The string is what to say about it.
    Done(String),
    /// An operation failed. Both halves are for people to read.
    Failed {
        /// What was being attempted.
        what: String,
        /// What the server or the filesystem said.
        why: String,
    },
    /// The session ended and nothing more will arrive.
    Closed,
}

/// Anything that can wake the interface after an event is sent.
///
/// The same contract as the graphical surfaces: sending into a channel nobody is polling changes
/// nothing on screen until something else happens to cause a repaint, and "something else" is usually
/// the user moving the mouse to find out why nothing is happening.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A handle for sending work to a file session.
///
/// Dropping it ends the session: the task sees its command channel close and shuts down, which is the
/// behaviour a tab being closed should have.
#[derive(Debug, Clone)]
pub struct FileSession {
    commands: mpsc::UnboundedSender<FileCommand>,
}

impl FileSession {
    /// Ask for something.
    ///
    /// # Errors
    ///
    /// Returns the command back if the session has already ended, so a caller that cares can say so
    /// rather than having the request disappear.
    pub fn send(&self, command: FileCommand) -> std::result::Result<(), FileCommand> {
        self.commands.send(command).map_err(|error| error.0)
    }

    /// Whether the session is still there.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.commands.is_closed()
    }

    /// A handle with nothing behind it.
    ///
    /// Every command sent to it fails, which is what a session whose connection has gone does. Useful
    /// as the starting state for something that will be given a real session later, and in tests that
    /// are about what an interface does with events rather than about talking to a server -- where the
    /// alternative is a stub that accepts commands nothing will ever act on, and so cannot show a
    /// caller mishandling a refusal.
    #[must_use]
    pub fn closed() -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        Self { commands }
    }
}

/// Start a file session on an SSH connection somebody else owns.
///
/// The connection is held for as long as the session runs -- an `Arc` rather than a reference, because
/// the task outlives the call and a file browser must not be able to close the terminal's connection
/// out from under it.
///
/// Events arrive on the returned receiver. The task sends [`FileEvent::Ready`] once it has resolved
/// the account's home directory, or [`FileEvent::Failed`] followed by [`FileEvent::Closed`] if the
/// server refuses the subsystem.
#[must_use]
pub fn start(
    connection: Arc<SshConnection>,
    label: impl Into<String>,
    waker: Waker,
) -> (FileSession, crossbeam_channel::Receiver<FileEvent>) {
    let label = label.into();
    let (commands_tx, mut commands_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = crossbeam_channel::unbounded();

    tokio::spawn(async move {
        // Every send goes through this, so waking is not something a new branch of the loop can
        // forget to do -- the mistake that leaves a tab blank until the mouse moves over it.
        let report = |event: FileEvent| -> bool {
            let sent = events_tx.send(event).is_ok();
            if sent {
                waker();
            }
            sent
        };

        let sftp = match Sftp::open(&connection, label.clone()).await {
            Ok(sftp) => sftp,
            Err(error) => {
                report(FileEvent::Failed {
                    what: "starting SFTP".to_owned(),
                    why: error.to_string(),
                });
                report(FileEvent::Closed);
                return;
            }
        };

        match sftp.home().await {
            Ok(home) => {
                if !report(FileEvent::Ready { home }) {
                    return;
                }
            }
            Err(error) => {
                report(FileEvent::Failed {
                    what: "asking where the account starts".to_owned(),
                    why: error.to_string(),
                });
                report(FileEvent::Closed);
                return;
            }
        }

        while let Some(command) = commands_rx.recv().await {
            let carry_on = match command {
                FileCommand::List(path) => match sftp.list(&path).await {
                    Ok(entries) => report(FileEvent::Listing { path, entries }),
                    Err(error) => report(FileEvent::Failed {
                        what: format!("listing {path}"),
                        why: error.to_string(),
                    }),
                },
                FileCommand::MakeDirectory(path) => match sftp.make_directory(&path).await {
                    Ok(()) => report(FileEvent::Done(format!("created {path}"))),
                    Err(error) => report(FileEvent::Failed {
                        what: format!("creating {path}"),
                        why: error.to_string(),
                    }),
                },
                FileCommand::Rename { from, to } => match sftp.rename(&from, &to).await {
                    Ok(()) => report(FileEvent::Done(format!("renamed {from} to {to}"))),
                    Err(error) => report(FileEvent::Failed {
                        what: format!("renaming {from}"),
                        why: error.to_string(),
                    }),
                },
                FileCommand::Remove { path, directory } => {
                    let outcome = if directory {
                        sftp.remove_directory(&path).await
                    } else {
                        sftp.remove_file(&path).await
                    };
                    match outcome {
                        Ok(()) => report(FileEvent::Done(format!("deleted {path}"))),
                        Err(error) => report(FileEvent::Failed {
                            what: format!("deleting {path}"),
                            why: error.to_string(),
                        }),
                    }
                }
                FileCommand::Download {
                    id,
                    remote,
                    local,
                    resume,
                } => {
                    let mut throttle = Throttle::new(id, &report);
                    let outcome = sftp
                        .download(&remote, &local, resume, &mut |done, total| {
                            throttle.tick(done, total);
                        })
                        .await;
                    finish(&report, id, &format!("downloading {remote}"), outcome)
                }
                FileCommand::Upload {
                    id,
                    local,
                    remote,
                    resume,
                } => {
                    let mut throttle = Throttle::new(id, &report);
                    let outcome = sftp
                        .upload(&local, &remote, resume, &mut |done, total| {
                            throttle.tick(done, total);
                        })
                        .await;
                    finish(&report, id, &format!("uploading {remote}"), outcome)
                }
                FileCommand::Shutdown => false,
            };
            if !carry_on {
                break;
            }
        }

        // Best effort: the channel is going away whether or not the server agrees, and a failure to
        // close it politely is not something anybody can act on.
        if let Err(error) = sftp.close().await {
            tracing::debug!(%error, %label, "closing the sftp channel failed");
        }
        report(FileEvent::Closed);
    });

    (
        FileSession {
            commands: commands_tx,
        },
        events_rx,
    )
}

/// Report a finished transfer, or why it did not finish.
fn finish(
    report: &impl Fn(FileEvent) -> bool,
    id: u64,
    what: &str,
    outcome: crate::Result<u64>,
) -> bool {
    match outcome {
        Ok(bytes) => report(FileEvent::Finished { id, bytes }),
        Err(error) => report(FileEvent::Failed {
            what: what.to_owned(),
            why: error.to_string(),
        }),
    }
}

/// Thins out progress reports so a big transfer does not drown the interface.
///
/// The first is always sent, so a bar appears at zero the moment a transfer starts rather than a
/// tenth of a second later -- which on a small file is after it has already finished.
struct Throttle<'a, F: Fn(FileEvent) -> bool> {
    id: u64,
    report: &'a F,
    last: Option<Instant>,
}

impl<'a, F: Fn(FileEvent) -> bool> Throttle<'a, F> {
    fn new(id: u64, report: &'a F) -> Self {
        Self {
            id,
            report,
            last: None,
        }
    }

    fn tick(&mut self, done: u64, total: Option<u64>) {
        let now = Instant::now();
        let due = match self.last {
            None => true,
            Some(last) => now.duration_since(last) >= PROGRESS_INTERVAL,
        };
        // The last chunk is not special-cased here: `Finished` carries the final size, so a bar that
        // is drawn from `Progress` alone still ends full.
        if due {
            self.last = Some(now);
            (self.report)(FileEvent::Progress {
                id: self.id,
                done,
                total,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// What a throttle decided to report: bytes done, and the total it was told.
    type Reported = Arc<Mutex<Vec<(u64, Option<u64>)>>>;

    /// Collects what a throttle decided to report.
    fn collector() -> (impl Fn(FileEvent) -> bool, Reported) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let report = move |event: FileEvent| {
            if let FileEvent::Progress { done, total, .. } = event {
                sink.lock().expect("not poisoned").push((done, total));
            }
            true
        };
        (report, seen)
    }

    #[test]
    fn the_first_report_is_never_thinned_out() {
        // A file that finishes inside one interval would otherwise report nothing at all, and a
        // transfer with no progress at all looks like one that never started.
        let (report, seen) = collector();
        let mut throttle = Throttle::new(7, &report);
        throttle.tick(0, Some(100));
        throttle.tick(50, Some(100));
        throttle.tick(100, Some(100));
        let seen = seen.lock().expect("not poisoned");
        assert_eq!(
            seen.as_slice(),
            &[(0, Some(100))],
            "the first goes out and the rest are inside the interval"
        );
    }

    #[test]
    fn a_report_goes_out_once_the_interval_has_passed() {
        let (report, seen) = collector();
        let mut throttle = Throttle::new(1, &report);
        throttle.tick(0, None);
        // Reaching in rather than sleeping: a test that waited for real time would take a tenth of
        // a second to prove something about arithmetic.
        throttle.last = Some(Instant::now() - PROGRESS_INTERVAL * 2);
        throttle.tick(4096, None);
        throttle.tick(8192, None);
        let seen = seen.lock().expect("not poisoned");
        assert_eq!(seen.as_slice(), &[(0, None), (4096, None)]);
    }

    #[test]
    fn thirty_two_thousand_chunks_do_not_become_thirty_two_thousand_events() {
        // The number this exists for: a gigabyte in 32 KiB chunks. Without throttling this is
        // 32,768 channel sends and 32,768 repaint requests to move a bar by a thousandth of a pixel.
        let (report, seen) = collector();
        let mut throttle = Throttle::new(0, &report);
        for chunk in 0..32_768_u64 {
            throttle.tick(chunk * 32 * 1024, Some(1024 * 1024 * 1024));
        }
        let count = seen.lock().expect("not poisoned").len();
        assert!(
            count < 100,
            "a gigabyte reported {count} times, which is the flood this prevents"
        );
    }

    #[test]
    fn a_closed_session_says_so_rather_than_swallowing_work() {
        // Dropping the receiving end is a tab being closed. A send after that has to fail, so a
        // caller can say "that did not happen" instead of leaving somebody waiting for a copy that
        // was never started.
        let (commands, rx) = mpsc::unbounded_channel::<FileCommand>();
        let session = FileSession { commands };
        assert!(session.is_open());
        drop(rx);
        assert!(!session.is_open());
        assert!(
            session.send(FileCommand::List("/tmp".to_owned())).is_err(),
            "a command into a closed session has to come back"
        );
    }
}
