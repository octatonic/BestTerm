//! The RDP helper process.
//!
//! One session, one process. It speaks IronRDP to a server on one side and BestTerm's frame protocol
//! to its parent on the other, and it exists so that a decoder fault takes down a tab instead of the
//! application — see `docs/ARCHITECTURE.md`.
//!
//! # The three channels
//!
//! * **stdin** carries [`HostMessage`]: connect, input, resize, shut down.
//! * **stdout** carries [`HelperMessage`]: ready, a frame is available, the session ended.
//! * **A shared memory mapping** carries the pixels, because eight megabytes thirty times a second
//!   is not something to push down a pipe.
//!
//! stdout is the protocol, which means nothing else may write to it. Logging goes to stderr, and the
//! parent is free to read it or discard it.
//!
//! # Why reading and processing are separate calls
//!
//! The loop waits on two things at once: a PDU from the server and a command from the parent. Only
//! the *reading* half of the RDP side is cancel-safe, so the `select!` waits on
//! [`ActiveSession::read`], and [`ActiveSession::process`] runs outside it where nothing can abandon
//! it half-way. Doing it the obvious way — selecting on a combined pump — would silently drop a PDU
//! every time a command happened to arrive at the same moment, along with the acknowledgement it
//! owed the server.
//!
//! # The server's key is not this process's decision
//!
//! See [`trust`]. The store lives with the host and so does the person; what the helper owns is the
//! moment at which asking is still useful, which is inside a handshake it is running.
//!
//! # Failure is an exit
//!
//! There is no recovery here. A session that fails says so on stdout, then the process ends: the
//! parent owns the decision to retry, and a helper that reconnected on its own would be doing it
//! without anybody's permission and without a fresh look at the server's key.

mod trust;

use std::io::Write;
use std::sync::{Arc, Mutex};

use bestterm_ipc_frame::{
    ConnectRequest, FrameReady, HelperMessage, HostMessage, PROTOCOL_VERSION, SharedFrames,
    read_message, write_message,
};
use bestterm_proto_rdp::{
    ActiveSession, KeyFingerprint, KnownServers, RdpError, ServerKeyChecker, Update,
};
use bestterm_surface::{FrameSize, PixelFormat};
use tokio::sync::mpsc;

use crate::trust::AskingVerifier;

/// How many commands may be queued before the reader thread waits.
///
/// Small on purpose: input events go stale, and a backlog of mouse moves is worse than the reader
/// blocking until the session catches up.
const COMMAND_QUEUE: usize = 64;

/// The largest framebuffer a mapping is created for, in pixels.
///
/// 8192 by 8192 is what RDP's display control channel will accept, so nothing larger can be
/// negotiated. It is a bound on one allocation, not a reservation: the mapping is sized to the
/// desktop actually agreed on.
const MAX_PIXELS: u64 = 8192 * 8192;

fn main() -> std::process::ExitCode {
    // stderr, never stdout: stdout is the protocol.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BESTTERM_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(%err, "could not start a runtime");
            return std::process::ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            // Already reported on stdout where it can be; this is for whoever reads the log.
            tracing::error!(%err, "the helper stopped");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Everything the helper does, from the first command to the last frame.
async fn run() -> Result<(), Failure> {
    let out = Arc::new(Mutex::new(Reporter::new()));
    let Reading {
        mut commands,
        key_answers,
    } = spawn_command_reader();

    // Nothing happens until the parent says where to connect. Anything else at this point is a
    // protocol error on their side, not something to guess at.
    let request = match commands.recv().await {
        Some(HostMessage::Connect(request)) => request,
        Some(HostMessage::Shutdown) | None => {
            tracing::debug!("asked to stop before connecting");
            return Ok(());
        }
        Some(other) => {
            let message = format!("expected a connect request first, got {other:?}");
            report(&out, &HelperMessage::Error(message.clone()));
            return Err(Failure(message));
        }
    };

    let label = label_for(&request);
    let session = match connect(&request, Arc::clone(&out), key_answers).await {
        Ok(session) => session,
        Err(err) => {
            let message = err.to_string();
            report(
                &out,
                &HelperMessage::Closed {
                    reason: Some(message.clone()),
                },
            );
            return Err(Failure(message));
        }
    };

    tracing::info!(label = %label, "rdp: session open");
    serve(session, commands, out).await
}

/// Send one message to the host, taking the lock for the duration and no longer.
fn report(out: &Mutex<Reporter>, message: &HelperMessage) {
    match out.lock() {
        Ok(mut out) => out.send(message),
        // Only reachable if a thread panicked while holding it, which means the protocol stream is
        // in an unknown state; saying nothing is better than saying something half-written.
        Err(_) => tracing::error!("the report channel is poisoned"),
    }
}

/// Push whatever is buffered towards the host.
fn flush(out: &Mutex<Reporter>) {
    if let Ok(mut out) = out.lock() {
        out.flush();
    }
}

/// Open the session the parent asked for.
///
/// Split out so that the difference between "could not connect" and "the session ended" is visible
/// in [`run`]'s shape: the first is reported as a `Closed` the parent can show, the second is a
/// whole loop.
async fn connect(
    request: &ConnectRequest,
    out: Arc<Mutex<Reporter>>,
    key_answers: std::sync::mpsc::Receiver<bool>,
) -> Result<ActiveSession, RdpError> {
    // A store of exactly one entry: the key the host has on record for this address, if it has one.
    // That is all the store is consulted for, and shipping the rest of it across would be handing a
    // helper process an inventory of every server somebody connects to.
    let known = match request
        .known_server_key
        .as_deref()
        .and_then(KeyFingerprint::from_hex)
    {
        Some(fingerprint) => KnownServers::parse(&KnownServers::line_for(
            &request.host,
            request.port,
            fingerprint,
        )),
        None => KnownServers::default(),
    };

    let checker = ServerKeyChecker::new(known, AskingVerifier::new(out.clone(), key_answers));
    let connected = bestterm_proto_rdp::connect(request, &checker).await?;

    // Whatever was settled goes back, because the store is the host's. `should_store` is false when
    // the key was already on record, which is the common case and must not rewrite the file.
    let outcome = &connected.server_key;
    report(
        &out,
        &HelperMessage::ServerKey {
            fingerprint: outcome.presented.to_hex(),
            store: outcome.should_store(),
        },
    );
    flush(&out);

    Ok(ActiveSession::new(connected, label_for(request)))
}

/// Run the session until it ends.
async fn serve(
    mut session: ActiveSession,
    mut commands: mpsc::Receiver<HostMessage>,
    out: Arc<Mutex<Reporter>>,
) -> Result<(), Failure> {
    let mut frames = Frames::create(session.layout().size, &out)?;
    // Said once and not per event: a mouse that does nothing should produce one line in the log, not
    // one per movement.
    let mut input_reported = false;

    loop {
        let pdu = tokio::select! {
            // The cancel-safe half. See the module documentation for why the other half is not here.
            read = session.read() => match read {
                Ok(pdu) => Some(pdu),
                Err(err) => {
                    let message = err.to_string();
                    report(&out, &HelperMessage::Closed { reason: Some(message.clone()) });
                    return Err(Failure(message));
                }
            },

            command = commands.recv() => {
                match command {
                    // The parent closed the pipe. It is gone, so this session has no audience.
                    None | Some(HostMessage::Shutdown) => {
                        tracing::debug!("asked to stop");
                        report(&out, &HelperMessage::Closed { reason: None });
                        return Ok(());
                    }
                    Some(HostMessage::Resize(size)) => {
                        if let Err(err) = session.request_resize(size).await {
                            report(&out, &HelperMessage::Error(err.to_string()));
                        }
                    }
                    Some(HostMessage::Input(_)) => {
                        if !input_reported {
                            input_reported = true;
                            let message =
                                "this build cannot send input yet; the session is view-only"
                                    .to_string();
                            tracing::warn!("{message}");
                            report(&out, &HelperMessage::Error(message));
                        }
                    }
                    Some(HostMessage::Connect(_)) => {
                        // One session per process. A second request is the parent's bug, and
                        // honouring it would leave the first session running with nothing reading it.
                        report(
                            &out,
                            &HelperMessage::Error("this helper already has a session".to_string()),
                        );
                    }
                    Some(HostMessage::ServerKeyAnswer { .. }) => {
                        // The reader routes these to the verifier, so one reaching here is an answer
                        // to a question nobody asked -- most likely a second answer to the one that
                        // opened this session. Ignored rather than acted on: no key decision is
                        // outstanding, and pretending otherwise would invent one.
                        tracing::debug!("an answer about a key arrived with no question open");
                    }
                }
                None
            }
        };

        // Outside the select!, where it cannot be abandoned part-way.
        let Some(pdu) = pdu else { continue };
        let updates = match session.process(pdu).await {
            Ok(updates) => updates,
            Err(err) => {
                let message = err.to_string();
                report(
                    &out,
                    &HelperMessage::Closed {
                        reason: Some(message.clone()),
                    },
                );
                return Err(Failure(message));
            }
        };

        for update in updates {
            match update {
                Update::Frame { damage } => frames.publish(&session, damage, &out)?,
                Update::Resized(size) => {
                    frames.fit(size, &out)?;
                    report(&out, &HelperMessage::Resized(size));
                }
                Update::Cursor(shape) => report(&out, &HelperMessage::Cursor(shape)),
                Update::Closed { reason } => {
                    report(&out, &HelperMessage::Closed { reason });
                    return Ok(());
                }
            }
        }
        flush(&out);
    }
}

/// The shared mapping, and the generation counter that goes with it.
struct Frames {
    /// Where the pixels go.
    shared: SharedFrames,
    /// The last generation published. Generations start at 1, because 0 means "nothing yet".
    generation: u64,
    /// What the mapping was sized for.
    size: FrameSize,
}

impl Frames {
    /// Create a mapping for `size` and tell the parent where it is.
    fn create(size: FrameSize, out: &Mutex<Reporter>) -> Result<Self, Failure> {
        let shared = SharedFrames::create("rdp", slot_bytes(size)?)
            .map_err(|err| Failure(format!("could not create a shared framebuffer: {err}")))?;

        report(
            out,
            &HelperMessage::Ready {
                version: PROTOCOL_VERSION,
                mapping: shared.path().to_string_lossy().into_owned(),
                slot_count: shared.slot_count(),
                slot_bytes: shared.slot_bytes(),
            },
        );
        flush(out);

        Ok(Self {
            shared,
            generation: 0,
            size,
        })
    }

    /// Make sure the mapping can hold a frame of `size`, replacing it if it cannot.
    ///
    /// A replacement is announced with a second `Ready`, which is why that message describes the
    /// mapping rather than the session: it means "this is where the pixels are from now on", and the
    /// parent has to reopen. Shrinking does not replace anything — a smaller frame fits in a larger
    /// slot, and the churn would cost more than the memory.
    fn fit(&mut self, size: FrameSize, out: &Mutex<Reporter>) -> Result<(), Failure> {
        self.size = size;
        if slot_bytes(size)? <= self.shared.slot_bytes() {
            return Ok(());
        }

        tracing::debug!(
            width = size.width,
            height = size.height,
            "rdp: the desktop outgrew the shared mapping; making a new one"
        );
        // The old mapping is dropped by the assignment, which deletes its backing file. The parent
        // has not been told about the new one yet, so for that instant there is nothing to read —
        // acceptable, because a resize invalidates every frame in it anyway.
        let replacement = Self::create(size, out)?;
        *self = replacement;
        Ok(())
    }

    /// Copy the current frame into the mapping and tell the parent it is there.
    fn publish(
        &mut self,
        session: &ActiveSession,
        damage: Vec<bestterm_surface::Rect>,
        out: &Mutex<Reporter>,
    ) -> Result<(), Failure> {
        let layout = session.layout();
        let pixels = session.frame();

        self.generation += 1;
        let generation = self.generation;
        self.shared
            .write(generation, |slot| {
                let length = pixels.len().min(slot.len());
                slot[..length].copy_from_slice(&pixels[..length]);
            })
            .map_err(|err| Failure(format!("could not publish a frame: {err}")))?;

        report(
            out,
            &HelperMessage::Frame(FrameReady {
                generation,
                size: layout.size,
                stride: layout.stride,
                format: layout.format,
                damage,
            }),
        );
        Ok(())
    }
}

/// Bytes one frame of `size` occupies.
///
/// Checked rather than multiplied, because the size arrives from a server: a desktop the protocol
/// should not have been able to negotiate must fail here and not in an allocator.
fn slot_bytes(size: FrameSize) -> Result<u64, Failure> {
    let pixels = size.pixel_count();
    if pixels == 0 || pixels > MAX_PIXELS {
        return Err(Failure(format!(
            "a {}x{} desktop is not something this can allocate for",
            size.width, size.height
        )));
    }
    Ok(pixels * u64::from(PixelFormat::Bgra8.bytes_per_pixel()))
}

/// stdout, with a lock held only for the duration of a write.
///
/// A struct rather than a bare handle so that "everything the parent hears goes through one place"
/// is enforced by the shape of the code, not by remembering.
pub(crate) struct Reporter {
    out: std::io::Stdout,
}

impl Reporter {
    fn new() -> Self {
        Self {
            out: std::io::stdout(),
        }
    }

    /// Send one message, dropping it if the parent has gone.
    ///
    /// A failed write is not escalated: it means the parent closed the pipe, which the loop will
    /// discover on its own when the command channel ends. Turning it into an error here would race
    /// that and report a broken pipe as a session failure.
    pub(crate) fn send(&mut self, message: &HelperMessage) {
        let mut handle = self.out.lock();
        if let Err(err) = write_message(&mut handle, &message.encode()) {
            tracing::debug!(%err, "could not reach the host");
        }
    }

    /// Push whatever is buffered.
    pub(crate) fn flush(&mut self) {
        let _ = self.out.lock().flush();
    }
}

/// Read commands from stdin on a thread of its own.
///
/// A thread and not an async read: stdin has no useful asynchronous form on Windows, and this way
/// the blocking is visible. The thread ends when the pipe closes, which closes the channel, which is
/// how the session learns the parent is gone.
fn spawn_command_reader() -> Reading {
    let (tx, commands) = mpsc::channel(COMMAND_QUEUE);
    // A channel of its own, because the thing waiting for an answer about a key is a handshake
    // inside `connect`, and the thing draining `commands` is the loop that only starts afterwards.
    // One queue for both would leave the answer sitting behind whoever is not reading yet.
    let (key_tx, key_answers) = std::sync::mpsc::channel();

    std::thread::Builder::new()
        .name("host-commands".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let mut buf = Vec::new();

            loop {
                match read_message(&mut input, &mut buf) {
                    Ok(false) => break,
                    Ok(true) => {}
                    Err(err) => {
                        tracing::debug!(%err, "the host's command stream ended");
                        break;
                    }
                }

                let message = match HostMessage::decode(&buf) {
                    Ok(message) => message,
                    Err(err) => {
                        // Refused, not skipped: a message this build cannot read means the two
                        // sides disagree about the protocol, and carrying on would act on the next
                        // message with no idea what the last one asked for.
                        tracing::error!(%err, "could not read a command from the host");
                        break;
                    }
                };

                // Routed rather than queued: see the channel's declaration above.
                if let HostMessage::ServerKeyAnswer { accept } = message {
                    if key_tx.send(accept).is_err() {
                        tracing::debug!("an answer about a server key arrived too late");
                    }
                    continue;
                }

                if tx.blocking_send(message).is_err() {
                    break;
                }
            }
        })
        // A helper that cannot read its own commands has nothing to do, and the panic message is the
        // only way anyone would find out.
        .expect("the command reader thread must start");

    Reading {
        commands,
        key_answers,
    }
}

/// The two streams a command reader produces.
struct Reading {
    /// Everything the session loop acts on.
    commands: mpsc::Receiver<HostMessage>,
    /// Answers about the server's key, which only the handshake is waiting for.
    key_answers: std::sync::mpsc::Receiver<bool>,
}

/// Something that ended the helper.
///
/// A string and not an error enum: everything here has already been reported to the parent by the
/// time it becomes one of these, so its only remaining job is the exit code and one line in the log.
#[derive(Debug)]
struct Failure(String);

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What to call this session in a log and on a tab.
fn label_for(request: &ConnectRequest) -> String {
    if request.username.is_empty() {
        request.host.clone()
    } else {
        format!("{}@{}", request.username, request.host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_four_bytes_a_pixel() {
        assert_eq!(
            slot_bytes(FrameSize::new(1920, 1080)).expect("fits"),
            1920 * 1080 * 4
        );
    }

    #[test]
    fn a_desktop_the_protocol_cannot_negotiate_is_refused_before_it_is_allocated() {
        // The size arrives from a server. Multiplying it out and handing the result to an allocator
        // is how a bad number becomes a memory exhaustion rather than an error message.
        assert!(slot_bytes(FrameSize::new(0, 1080)).is_err());
        assert!(slot_bytes(FrameSize::new(1920, 0)).is_err());
        assert!(slot_bytes(FrameSize::new(u32::MAX, u32::MAX)).is_err());
        assert!(
            slot_bytes(FrameSize::new(8192, 8192)).is_ok(),
            "the largest RDP allows"
        );
        assert!(slot_bytes(FrameSize::new(8192, 8193)).is_err());
    }

    #[test]
    fn a_label_without_a_username_is_just_the_host() {
        let mut request = ConnectRequest {
            host: "srv.example".to_string(),
            port: 3389,
            username: String::new(),
            domain: None,
            password: bestterm_core_vault::Secret::new(String::new()),
            desktop_size: FrameSize::new(1024, 768),
            enable_credssp: true,
            keyboard_layout: 0,
            client_name: "bestterm".to_string(),
            known_server_key: None,
        };
        assert_eq!(label_for(&request), "srv.example");

        request.username = "admin".to_string();
        assert_eq!(label_for(&request), "admin@srv.example");
    }
}
