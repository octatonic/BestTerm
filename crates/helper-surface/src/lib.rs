//! A [`GraphicalSurface`] that is really another process.
//!
//! RDP and VNC decode in helper processes, so that a decoder fault costs a tab rather than the
//! application and so a GPL C library can be kept out of the main binary. This is the half of that
//! boundary the application holds: it launches the helper, speaks [`bestterm_ipc_frame`]'s protocol
//! to it, opens the shared mapping it announces, and presents all of it as an ordinary surface.
//!
//! Nothing here knows what RDP is. The helper's name is a parameter.
//!
//! # Threads
//!
//! One, per surface, reading the helper's stdout. It turns each message into a [`SurfaceEvent`],
//! sends it down a channel the interface drains, and then asks the interface to draw. Writing happens
//! on whichever thread calls, under a lock, because the messages are small and a lock held for the
//! length of a `write_all` is cheaper than another thread to own the pipe.
//!
//! The wake happens here, on the thread that already has the event, and *after* the send — so a frame
//! that starts in response to it always finds the event queued. It cannot be done on the reading side
//! instead: a channel has no way to look at what is in it without taking it, so a thread that waited
//! on the channel purely to wake somebody would consume the very events it was announcing.
//!
//! # What happens when the helper dies
//!
//! Its stdout closes, the reader thread ends, and it sends [`SurfaceEvent::Closed`] on the way out.
//! That is the only close notification the application is promised, so it is sent whether the helper
//! exited cleanly, crashed, or was killed — a surface that goes quiet without saying so is a tab that
//! looks alive forever.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use bestterm_ipc_frame::{
    ConnectRequest, HelperMessage, HostMessage, SharedFrames, read_message, write_message,
};
use bestterm_surface::{
    EventReceiver, FrameMeta, FrameSize, GraphicalSurface, InputEvent, Result, SurfaceError,
    SurfaceEvent, SurfaceKind,
};

/// Where a helper binary is looked for.
///
/// Beside the running executable, and nowhere else. Not on `PATH`: a helper found on `PATH` is a
/// helper somebody else can put there, and this one is handed a password.
pub fn helper_path(name: &str) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the running executable has no directory",
        )
    })?;
    Ok(dir.join(with_exe_suffix(name)))
}

/// Add `.exe` where the platform expects one.
fn with_exe_suffix(name: &str) -> String {
    if std::env::consts::EXE_SUFFIX.is_empty() {
        name.to_string()
    } else {
        format!("{name}{}", std::env::consts::EXE_SUFFIX)
    }
}

/// The pixels of the most recent frame, and how to read them.
///
/// Behind one lock because a frame is copied out of shared memory and then handed to a renderer, and
/// the two must not see different halves of a resize.
#[derive(Default)]
struct Frame {
    /// How to interpret `pixels`, or `None` before the first frame.
    meta: Option<FrameMeta>,
    /// The copy.
    pixels: Vec<u8>,
}

/// A surface backed by a helper process.
pub struct HelperSurface {
    /// Which protocol the helper speaks, for the UI.
    kind: SurfaceKind,
    /// What to call this session.
    label: String,
    /// The helper. Killed on drop, because a helper whose parent is gone has nobody to send frames
    /// to and a live connection to a server that nobody is watching.
    child: Child,
    /// Its stdin, behind a lock so any thread may send.
    to_helper: Arc<Mutex<Option<std::process::ChildStdin>>>,
    /// The latest frame, copied out of the mapping by the reader thread.
    frame: Arc<Mutex<Frame>>,
    /// The size last asked for, so a repeated request is not sent twice.
    requested_size: Option<FrameSize>,
}

impl std::fmt::Debug for HelperSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperSurface")
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

/// Launch `helper` and ask it to open `request`.
///
/// Returns as soon as the process is running and the request has been written — not when the session
/// is up. Everything after that arrives as events, including the failure to connect at all, because
/// a connection that takes eight seconds to be refused must not be eight seconds of frozen window.
pub fn connect(
    helper: &Path,
    kind: SurfaceKind,
    label: String,
    request: ConnectRequest,
    waker: Waker,
) -> Result<(HelperSurface, EventReceiver<SurfaceEvent>)> {
    let mut child = Command::new(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Inherited: the helper logs to stderr on purpose, so whatever is capturing this process's
        // diagnostics captures the helper's too. Piping it and not reading it would eventually block
        // the helper on a full pipe.
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin = child.stdin.take().ok_or_else(|| {
        SurfaceError::Protocol("the helper was started without a command channel".to_string())
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        SurfaceError::Protocol("the helper was started without a message channel".to_string())
    })?;

    // Written before the reader starts, so the helper has something to do the moment it is up.
    write_message(
        &mut stdin,
        &HostMessage::Connect(Box::new(request)).encode(),
    )?;
    stdin.flush()?;

    let (events_tx, events_rx) = crossbeam_channel::unbounded();
    let frame = Arc::new(Mutex::new(Frame::default()));

    {
        let frame = Arc::clone(&frame);
        let label = label.clone();
        std::thread::Builder::new()
            .name(format!("helper-{}", kind.id()))
            .spawn(move || read_from_helper(BufReader::new(stdout), frame, events_tx, waker, label))
            // A surface whose reader could not start would produce no frames and no errors, which
            // looks exactly like a server that never answers.
            .expect("the helper reader thread must start");
    }

    Ok((
        HelperSurface {
            kind,
            label,
            child,
            to_helper: Arc::new(Mutex::new(Some(stdin))),
            frame,
            requested_size: None,
        },
        events_rx,
    ))
}

/// Something that asks the interface to draw a frame.
///
/// A closure rather than the windowing type, so this crate says what it needs — "wake up" — without
/// naming who provides it. The same shape the terminal side uses, for the same reason.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Turn the helper's messages into surface events until it stops.
fn read_from_helper(
    mut stdout: BufReader<std::process::ChildStdout>,
    frame: Arc<Mutex<Frame>>,
    events: crossbeam_channel::Sender<SurfaceEvent>,
    waker: Waker,
    label: String,
) {
    let mut buf = Vec::new();
    let mut shared: Option<SharedFrames> = None;
    // Sent when the loop ends, unless the helper already said why.
    let mut closed_with: Option<Option<String>> = None;

    loop {
        match read_message(&mut stdout, &mut buf) {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => {
                tracing::debug!(%label, %error, "the helper's message stream ended");
                break;
            }
        }

        let message = match HelperMessage::decode(&buf) {
            Ok(message) => message,
            Err(error) => {
                // Refused rather than skipped: a message this build cannot read means the two sides
                // disagree about the protocol, and the next message would be acted on with no idea
                // what this one asked for.
                tracing::error!(%label, %error, "could not read a message from the helper");
                closed_with = Some(Some(format!(
                    "the helper said something unreadable: {error}"
                )));
                break;
            }
        };

        match message {
            HelperMessage::Ready {
                version,
                mapping,
                slot_count,
                slot_bytes,
            } => {
                if version != bestterm_ipc_frame::PROTOCOL_VERSION {
                    closed_with = Some(Some(format!(
                        "the helper speaks protocol {version} and this build speaks {}",
                        bestterm_ipc_frame::PROTOCOL_VERSION
                    )));
                    break;
                }
                tracing::debug!(%label, slot_count, slot_bytes, "opening the helper's framebuffer");
                match SharedFrames::open(&mapping) {
                    Ok(opened) => shared = Some(opened),
                    Err(error) => {
                        closed_with = Some(Some(format!(
                            "could not open the shared framebuffer: {error}"
                        )));
                        break;
                    }
                }
            }

            HelperMessage::Frame(ready) => {
                let Some(shared) = shared.as_ref() else {
                    // A frame before a mapping is the helper's bug, not a reason to stop: the next
                    // `Ready` would fix it, and dropping the connection here would lose a session
                    // over one lost picture.
                    tracing::warn!(%label, "a frame arrived before the framebuffer");
                    continue;
                };

                let wanted = ready.size.pixel_count() * u64::from(ready.format.bytes_per_pixel());
                let Ok(wanted) = usize::try_from(wanted) else {
                    tracing::warn!(%label, "the helper announced a frame larger than memory");
                    continue;
                };

                let meta = FrameMeta {
                    size: ready.size,
                    format: ready.format,
                    stride: ready.stride,
                    damage: ready.damage,
                    generation: ready.generation,
                };

                let copied = {
                    let Ok(mut frame) = frame.lock() else { break };
                    match shared.read_latest(wanted, &mut frame.pixels) {
                        Some(generation) => {
                            frame.meta = Some(meta.clone());
                            Some(generation)
                        }
                        // Not an error, and deliberately not reported as one: the writer lapped the
                        // reader mid-copy, so this frame is torn. The next one is already on its way,
                        // and showing a torn frame is worse than skipping one.
                        None => None,
                    }
                };

                if let Some(generation) = copied {
                    if events.send(SurfaceEvent::Frame(meta)).is_err() {
                        tracing::debug!(%label, generation, "nobody is reading this surface");
                        return;
                    }
                    waker();
                }
            }

            HelperMessage::Resized(size) => {
                if events.send(SurfaceEvent::Resized(size)).is_err() {
                    return;
                }
                waker();
            }
            HelperMessage::Cursor(shape) => {
                if events.send(SurfaceEvent::Cursor(shape)).is_err() {
                    return;
                }
                waker();
            }
            HelperMessage::ClipboardOffer(text) => {
                if events.send(SurfaceEvent::ClipboardOffer(text)).is_err() {
                    return;
                }
                waker();
            }
            HelperMessage::AskAboutServerKey {
                host,
                port,
                fingerprint,
                expected,
            } => {
                if events
                    .send(SurfaceEvent::AskAboutServerKey {
                        host,
                        port,
                        fingerprint,
                        expected,
                    })
                    .is_err()
                {
                    return;
                }
                waker();
            }
            HelperMessage::ServerKey { fingerprint, store } => {
                if events
                    .send(SurfaceEvent::ServerKeySettled { fingerprint, store })
                    .is_err()
                {
                    return;
                }
                waker();
            }
            HelperMessage::Error(detail) => {
                if events.send(SurfaceEvent::Error(detail)).is_err() {
                    return;
                }
                waker();
            }
            HelperMessage::Closed { reason } => {
                closed_with = Some(reason);
                break;
            }
        }
    }

    // Always, whatever ended the loop. A surface that goes quiet without saying so is a tab that
    // looks alive forever.
    let _ = events.send(SurfaceEvent::Closed {
        reason: closed_with.flatten(),
    });
    waker();
}

impl HelperSurface {
    /// Send one message to the helper.
    fn tell(&self, message: &HostMessage) -> Result<()> {
        let mut slot = self
            .to_helper
            .lock()
            .map_err(|_| SurfaceError::Protocol("the command channel is poisoned".to_string()))?;
        let stdin = slot.as_mut().ok_or(SurfaceError::Closed)?;
        write_message(stdin, &message.encode())?;
        stdin.flush()?;
        Ok(())
    }
}

impl GraphicalSurface for HelperSurface {
    fn kind(&self) -> SurfaceKind {
        self.kind
    }

    fn send_input(&mut self, input: InputEvent) -> Result<()> {
        self.tell(&HostMessage::Input(input))
    }

    fn request_resize(&mut self, size: FrameSize) -> Result<()> {
        // Skipped when it would ask for what was already asked for. A window drag produces a resize
        // per frame, and every one of them makes the server re-run capability exchange.
        if self.requested_size == Some(size) {
            return Ok(());
        }
        self.requested_size = Some(size);
        self.tell(&HostMessage::Resize(size))
    }

    fn with_frame(&self, f: &mut dyn FnMut(&FrameMeta, &[u8])) {
        let Ok(frame) = self.frame.lock() else { return };
        if let Some(meta) = &frame.meta {
            f(meta, &frame.pixels);
        }
    }

    fn answer_server_key(&mut self, accept: bool) -> Result<()> {
        self.tell(&HostMessage::ServerKeyAnswer { accept })
    }

    fn shutdown(&mut self) -> Result<()> {
        // Asked first, so the helper can close the session politely; the pipe is dropped afterwards,
        // which is what tells it to stop even if the message never arrives.
        let _ = self.tell(&HostMessage::Shutdown);
        if let Ok(mut slot) = self.to_helper.lock() {
            slot.take();
        }
        Ok(())
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

impl Drop for HelperSurface {
    /// Kill the helper.
    ///
    /// Not merely dropped: a helper whose parent has gone still holds an authenticated connection to
    /// somebody's server, and it would keep holding it until the server timed out. Closing the pipe
    /// first gives it a chance to exit on its own; the kill is for the case where it is wedged
    /// somewhere that does not notice.
    fn drop(&mut self) {
        if let Ok(mut slot) = self.to_helper.lock() {
            slot.take();
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            _ => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_helper_is_looked_for_beside_this_executable_and_not_on_the_path() {
        // The reason it matters: the helper is handed a password. Something found on PATH is
        // something another program can arrange to be found.
        let found = helper_path("bestterm-rdp").expect("this executable has a directory");
        let here = std::env::current_exe().expect("this executable exists");
        assert_eq!(found.parent(), here.parent());
        assert!(
            found
                .file_name()
                .is_some_and(|name| { name.to_string_lossy().starts_with("bestterm-rdp") }),
            "{found:?}"
        );
    }

    #[test]
    fn the_name_carries_the_platform_suffix() {
        let name = with_exe_suffix("bestterm-rdp");
        if cfg!(windows) {
            assert_eq!(name, "bestterm-rdp.exe");
        } else {
            assert_eq!(name, "bestterm-rdp");
        }
    }
}
