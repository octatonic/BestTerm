//! The VNC helper process.
//!
//! The same shape as `bestterm-rdp`, and deliberately so: `crates/helper-surface` takes a helper's
//! name as a parameter rather than knowing about either protocol, so the host side of this was
//! already written before this binary existed.
//!
//! stdin carries [`HostMessage`], stdout carries [`HelperMessage`], and the pixels go through a
//! shared mapping. stdout is the protocol, so logging goes to stderr.
//!
//! # Updates are pulled
//!
//! The loop is not "read whatever arrives". A VNC server sends nothing until asked and answers one
//! request with one update, so every update ends with asking for the next. That is why this reads and
//! writes on the same task rather than splitting the socket: the two are a conversation, not two
//! streams.
//!
//! # The button state is ours to remember
//!
//! RFB has no press or release. Every pointer message carries the current state of every button, so
//! this keeps that state — a client that sends a press and forgets the release leaves a button held
//! down on the remote desktop, which is how a VNC session ends up selecting the whole screen.
//!
//! # Nothing here is encrypted
//!
//! VNC's own security types protect the handshake and nothing after it. Said once, at connection
//! time, on the same footing telnet says it.

use std::io::Write;
use std::sync::{Arc, Mutex};

use bestterm_ipc_frame::{
    ConnectRequest, FrameReady, HelperMessage, HostMessage, PROTOCOL_VERSION, SharedFrames,
    read_message, write_message,
};
use bestterm_proto_vnc::decode::Framebuffer;
use bestterm_proto_vnc::keysym;
use bestterm_proto_vnc::session::{self, Update, VncError};
use bestterm_surface::{FrameSize, InputEvent, PixelFormat, PointerButton, Rect};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// How many commands may be queued before the reader thread waits.
const COMMAND_QUEUE: usize = 64;

/// The largest framebuffer a mapping is created for, in pixels.
const MAX_PIXELS: u64 = 16_384 * 16_384;

fn main() -> std::process::ExitCode {
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
            tracing::error!(%err, "the helper stopped");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Everything the helper does.
async fn run() -> Result<(), Failure> {
    let out = Arc::new(Mutex::new(Reporter::new()));
    let mut commands = spawn_command_reader();

    let request = match commands.recv().await {
        Some(HostMessage::Connect(request)) => request,
        Some(HostMessage::Shutdown) | None => return Ok(()),
        Some(other) => {
            let message = format!("expected a connect request first, got {other:?}");
            report(&out, &HelperMessage::Error(message.clone()));
            return Err(Failure(message));
        }
    };

    let label = format!("{}:{}", request.host, request.port);
    tracing::warn!(
        host = %request.host,
        port = request.port,
        "vnc: this connection is not encrypted; the desktop and everything typed into it travel in \
         clear text"
    );

    let session = match connect(&request).await {
        Ok(session) => session,
        Err(error) => {
            let message = error.to_string();
            report(
                &out,
                &HelperMessage::Closed {
                    reason: Some(message.clone()),
                },
            );
            return Err(Failure(message));
        }
    };

    tracing::info!(%label, desktop = %session.desktop.name, "vnc: session open");
    serve(session, commands, out).await
}

/// A connected session.
struct Connected {
    stream: TcpStream,
    framebuffer: Framebuffer,
    desktop: session::Desktop,
}

/// Connect, authenticate and describe the desktop.
async fn connect(request: &ConnectRequest) -> Result<Connected, VncError> {
    let mut stream = TcpStream::connect((request.host.as_str(), request.port)).await?;
    // Interactive input in small writes, exactly as for telnet.
    if let Err(error) = stream.set_nodelay(true) {
        tracing::debug!(%error, "vnc: could not disable Nagle's algorithm");
    }

    let version = session::handshake_version(&mut stream).await?;
    // An empty password is no password: a server offering `None` never asks for one, and passing an
    // empty secret to a server that does ask would answer its challenge with eight zero bytes.
    let password = (!request.password.expose().is_empty()).then(|| request.password.clone());
    let security = session::handshake_security(&mut stream, version, password.as_ref()).await?;

    // Shared, always. Refusing to share disconnects whoever is already looking at the desktop, and
    // that is not a decision a remote-access tool should make on somebody's behalf.
    let desktop = session::handshake_init(&mut stream, true, security).await?;
    session::set_up(&mut stream).await?;

    tracing::info!(
        desktop = %desktop.name,
        width = desktop.width,
        height = desktop.height,
        security = security.label(),
        "vnc: connected"
    );

    let framebuffer = Framebuffer::new(u32::from(desktop.width), u32::from(desktop.height));
    Ok(Connected {
        stream,
        framebuffer,
        desktop,
    })
}

/// Run the session until it ends.
///
/// The socket is split, and the reading half lives in a task of its own. That is not tidiness: a VNC
/// message is a sequence of reads -- a header, then a rectangle, then its payload -- so
/// [`session::read_message`] is not cancel-safe, and a `select!` that also waited on the command
/// channel would abandon it half-way every time somebody moved the mouse. The stream would then be
/// one rectangle out of step and every frame after it nonsense. The first version of this had exactly
/// that, with a comment claiming the command branch "only ever completes between messages", which is
/// not true of a channel.
///
/// So nothing selects over the reading. One task reads, one task writes, and the commands become
/// messages for the writer -- the same shape `proto-telnet` uses, for the same reason.
async fn serve(
    session_state: Connected,
    mut commands: mpsc::Receiver<HostMessage>,
    out: Arc<Mutex<Reporter>>,
) -> Result<(), Failure> {
    let Connected {
        stream,
        framebuffer,
        desktop,
    } = session_state;

    let frames = Frames::create(
        FrameSize::new(framebuffer.width(), framebuffer.height()),
        &out,
    )?;

    // VNC has no server key to pin, so the host is told there is nothing to store rather than left
    // waiting for a message that never comes.
    report(
        &out,
        &HelperMessage::ServerKey {
            fingerprint: String::new(),
            store: false,
        },
    );
    flush(&out);

    let (read_half, write_half) = stream.into_split();
    let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();

    spawn_writer(write_half, outgoing_rx);
    // The first request asks for everything; every one after it asks for what changed, and the reader
    // is what sends those -- see its own documentation.
    let _ = outgoing_tx.send(Outgoing::Update {
        incremental: false,
        width: desktop.width,
        height: desktop.height,
    });

    let reader = spawn_reader(
        read_half,
        framebuffer,
        frames,
        Arc::clone(&out),
        outgoing_tx.clone(),
    );

    // RFB has no press or release; see the module documentation.
    let mut buttons = 0u8;
    let mut pointer = (0u16, 0u16);
    let mut unsendable = std::collections::HashSet::new();

    loop {
        tokio::select! {
            // The reading task ended, which means the session did. It has already reported why,
            // so there is nothing to add. `Notify::notified` is cancel-safe, which is what lets it
            // sit in a select at all.
            () = reader.notified() => return Ok(()),
            command = commands.recv() => {
                match command {
                    None | Some(HostMessage::Shutdown) => {
                        report(&out, &HelperMessage::Closed { reason: None });
                        return Ok(());
                    }
                    Some(HostMessage::Input(event)) => {
                        send_input(
                            &outgoing_tx,
                            &event,
                            &mut buttons,
                            &mut pointer,
                            &mut unsendable,
                            &out,
                        );
                    }
                    // RFB has no client-initiated resize outside the SetDesktopSize extension, which
                    // this build does not implement. Said once rather than silently ignored.
                    Some(HostMessage::Resize(_)) => {
                        if unsendable.insert("resize") {
                            report(&out, &HelperMessage::Error(
                                "this server's desktop size is not something the client can change"
                                    .to_string(),
                            ));
                        }
                    }
                    Some(HostMessage::Connect(_)) => {
                        report(&out, &HelperMessage::Error(
                            "this helper already has a session".to_string(),
                        ));
                    }
                    Some(HostMessage::ServerKeyAnswer { .. }) => {
                        // VNC has no server key, so nothing here ever asks about one.
                        tracing::debug!("an answer about a key arrived with no question open");
                    }
                }
            }
        }
    }
}

/// Something to put on the socket.
///
/// One writer owns the write half, so a keystroke cannot interleave with an update request in the
/// middle of a message.
enum Outgoing {
    /// Ask for a framebuffer update.
    Update {
        /// Whether to ask only for what changed.
        incremental: bool,
        /// The desktop's width.
        width: u16,
        /// And height.
        height: u16,
    },
    /// A key transition.
    Key {
        /// The X11 keysym.
        keysym: u32,
        /// True on press.
        pressed: bool,
    },
    /// The pointer's position and the state of every button.
    Pointer {
        /// Which buttons are down.
        buttons: u8,
        /// Horizontal position.
        x: u16,
        /// Vertical position.
        y: u16,
    },
}

/// The task that owns the write half.
fn spawn_writer(
    mut write_half: tokio::net::tcp::OwnedWriteHalf,
    mut outgoing: mpsc::UnboundedReceiver<Outgoing>,
) {
    tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            let sent = match message {
                Outgoing::Update {
                    incremental,
                    width,
                    height,
                } => session::request_update(&mut write_half, incremental, width, height).await,
                Outgoing::Key { keysym, pressed } => {
                    session::send_key(&mut write_half, keysym, pressed).await
                }
                Outgoing::Pointer { buttons, x, y } => {
                    session::send_pointer(&mut write_half, buttons, x, y).await
                }
            };
            if let Err(error) = sent {
                tracing::debug!(%error, "vnc: write failed");
                return;
            }
        }
    });
}

/// The task that reads framebuffer updates.
///
/// It owns the framebuffer and the shared mapping, and it is what asks for the next update: a VNC
/// server answers one request with one update and then says nothing, so the asking has to happen
/// where the answering is noticed.
fn spawn_reader(
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    mut framebuffer: Framebuffer,
    mut frames: Frames,
    out: Arc<Mutex<Reporter>>,
    outgoing: mpsc::UnboundedSender<Outgoing>,
) -> Arc<tokio::sync::Notify> {
    let done = Arc::new(tokio::sync::Notify::new());
    let finished = Arc::clone(&done);

    tokio::spawn(async move {
        loop {
            let updates = match session::read_message(&mut read_half, &mut framebuffer).await {
                Ok(updates) => updates,
                Err(error) => {
                    report(
                        &out,
                        &HelperMessage::Closed {
                            reason: Some(error.to_string()),
                        },
                    );
                    flush(&out);
                    finished.notify_waiters();
                    return;
                }
            };

            let mut resized = None;
            let mut damage: Vec<Rect> = Vec::new();
            for update in updates {
                match update {
                    Update::Damage(rects) => damage.extend(rects),
                    Update::Resized { width, height } => {
                        // Everything drawn before this refers to a framebuffer that no longer exists,
                        // so the damage before it is discarded rather than reported against the new
                        // one.
                        damage.clear();
                        resized = Some(FrameSize::new(width, height));
                    }
                }
            }

            if let Some(size) = resized {
                if frames.fit(size, &out).is_err() {
                    finished.notify_waiters();
                    return;
                }
                report(&out, &HelperMessage::Resized(size));
            }
            if (!damage.is_empty() || resized.is_some())
                && frames.publish(&framebuffer, damage, &out).is_err()
            {
                finished.notify_waiters();
                return;
            }
            flush(&out);

            // And ask for the next one, which is the whole reason this loop keeps going.
            let sent = outgoing.send(Outgoing::Update {
                incremental: true,
                width: u16::try_from(framebuffer.width()).unwrap_or(u16::MAX),
                height: u16::try_from(framebuffer.height()).unwrap_or(u16::MAX),
            });
            if sent.is_err() {
                finished.notify_waiters();
                return;
            }
        }
    });

    done
}

/// Turn one input event into RFB messages.
///
/// Nothing here writes to a socket: the messages go to the writer task, which owns the write half, so
/// a keystroke cannot land in the middle of an update request.
fn send_input(
    outgoing: &mpsc::UnboundedSender<Outgoing>,
    event: &InputEvent,
    buttons: &mut u8,
    pointer: &mut (u16, u16),
    unsendable: &mut std::collections::HashSet<&'static str>,
    out: &Mutex<Reporter>,
) {
    match event {
        InputEvent::Key {
            scancode, pressed, ..
        } => match keysym(*scancode) {
            Some(keysym) => {
                let _ = outgoing.send(Outgoing::Key {
                    keysym,
                    pressed: *pressed,
                });
            }
            None => {
                if unsendable.insert("key") {
                    tracing::debug!(scancode, "vnc: no keysym for this key");
                    report(
                        out,
                        &HelperMessage::Error(
                            "some keys on this keyboard have no VNC equivalent and are not sent"
                                .to_string(),
                        ),
                    );
                }
            }
        },

        InputEvent::PointerMove { x, y } => {
            *pointer = (clamp(*x), clamp(*y));
            let _ = outgoing.send(Outgoing::Pointer {
                buttons: *buttons,
                x: pointer.0,
                y: pointer.1,
            });
        }

        InputEvent::PointerButton {
            button,
            pressed,
            x,
            y,
        } => {
            let mask = button_mask(*button);
            if *pressed {
                *buttons |= mask;
            } else {
                *buttons &= !mask;
            }
            *pointer = (clamp(*x), clamp(*y));
            let _ = outgoing.send(Outgoing::Pointer {
                buttons: *buttons,
                x: pointer.0,
                y: pointer.1,
            });
        }

        // The wheel is four more buttons, pressed and released. RFB has no scroll message and no way
        // to say how far: one notch is one press and one release.
        InputEvent::Scroll { dx, dy } => {
            let (mask, notches) = if dy.abs() >= dx.abs() {
                (if *dy > 0.0 { 0b0000_1000 } else { 0b0001_0000 }, dy.abs())
            } else {
                (if *dx > 0.0 { 0b0010_0000 } else { 0b0100_0000 }, dx.abs())
            };
            // Bounded: a trackpad produces fractional deltas by the hundred, and a gesture that became
            // four hundred clicks would scroll a window to its end.
            let notches = (notches.ceil() as u32).clamp(1, 8);
            for _ in 0..notches {
                let _ = outgoing.send(Outgoing::Pointer {
                    buttons: *buttons | mask,
                    x: pointer.0,
                    y: pointer.1,
                });
                // The release matters as much as the press: a wheel button left down scrolls forever.
                let _ = outgoing.send(Outgoing::Pointer {
                    buttons: *buttons,
                    x: pointer.0,
                    y: pointer.1,
                });
            }
        }

        InputEvent::Text(_) | InputEvent::ClipboardProvide(_) => {
            if unsendable.insert("text") {
                report(
                    out,
                    &HelperMessage::Error(
                        "composed text and the clipboard are not shared with this server yet"
                            .to_string(),
                    ),
                );
            }
        }
    }
}

/// The bit RFB uses for a button.
fn button_mask(button: PointerButton) -> u8 {
    match button {
        PointerButton::Left => 0b0000_0001,
        PointerButton::Middle => 0b0000_0010,
        PointerButton::Right => 0b0000_0100,
        // Buttons six and seven, past the four the wheel uses.
        PointerButton::X1 => 0b1000_0000,
        PointerButton::X2 => 0b0100_0000,
    }
}

/// A coordinate as RFB carries it.
fn clamp(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// The shared mapping, and the generation counter that goes with it.
struct Frames {
    shared: SharedFrames,
    generation: u64,
}

impl Frames {
    fn create(size: FrameSize, out: &Mutex<Reporter>) -> Result<Self, Failure> {
        let shared = SharedFrames::create("vnc", slot_bytes(size)?)
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
        })
    }

    /// Make sure a frame of `size` fits, replacing the mapping if it does not.
    fn fit(&mut self, size: FrameSize, out: &Mutex<Reporter>) -> Result<(), Failure> {
        if slot_bytes(size)? <= self.shared.slot_bytes() {
            return Ok(());
        }
        *self = Self::create(size, out)?;
        Ok(())
    }

    /// Copy the framebuffer into the mapping and tell the host.
    fn publish(
        &mut self,
        framebuffer: &Framebuffer,
        damage: Vec<Rect>,
        out: &Mutex<Reporter>,
    ) -> Result<(), Failure> {
        self.generation += 1;
        let generation = self.generation;
        let pixels = framebuffer.pixels();
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
                size: FrameSize::new(framebuffer.width(), framebuffer.height()),
                stride: framebuffer.stride(),
                format: PixelFormat::Bgra8,
                damage,
            }),
        );
        Ok(())
    }
}

/// Bytes one frame occupies.
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
struct Reporter {
    out: std::io::Stdout,
}

impl Reporter {
    fn new() -> Self {
        Self {
            out: std::io::stdout(),
        }
    }

    fn send(&mut self, message: &HelperMessage) {
        let mut handle = self.out.lock();
        if let Err(err) = write_message(&mut handle, &message.encode()) {
            tracing::debug!(%err, "could not reach the host");
        }
    }

    fn flush(&mut self) {
        let _ = self.out.lock().flush();
    }
}

/// Send one message to the host.
fn report(out: &Mutex<Reporter>, message: &HelperMessage) {
    match out.lock() {
        Ok(mut out) => out.send(message),
        Err(_) => tracing::error!("the report channel is poisoned"),
    }
}

/// Push whatever is buffered.
fn flush(out: &Mutex<Reporter>) {
    if let Ok(mut out) = out.lock() {
        out.flush();
    }
}

/// Read commands from stdin on a thread of its own.
fn spawn_command_reader() -> mpsc::Receiver<HostMessage> {
    let (tx, rx) = mpsc::channel(COMMAND_QUEUE);

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
                        tracing::error!(%err, "could not read a command from the host");
                        break;
                    }
                };
                if tx.blocking_send(message).is_err() {
                    break;
                }
            }
        })
        .expect("the command reader thread must start");

    rx
}

/// Something that ended the helper.
#[derive(Debug)]
struct Failure(String);

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
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
    fn a_desktop_that_is_not_a_size_is_refused_before_it_is_allocated_for() {
        assert!(slot_bytes(FrameSize::new(0, 1080)).is_err());
        assert!(slot_bytes(FrameSize::new(1920, 0)).is_err());
        assert!(slot_bytes(FrameSize::new(u32::MAX, u32::MAX)).is_err());
    }

    #[test]
    fn every_button_has_its_own_bit() {
        // A collision is a click on the wrong button, which is worse than a click that does nothing.
        let masks = [
            PointerButton::Left,
            PointerButton::Middle,
            PointerButton::Right,
            PointerButton::X1,
            PointerButton::X2,
        ]
        .map(button_mask);

        let mut seen = std::collections::HashSet::new();
        for mask in masks {
            assert!(mask.count_ones() == 1, "{mask:#b} is not one button");
            assert!(seen.insert(mask), "{mask:#b} is used twice");
        }

        // And none of them collides with the four the wheel uses.
        let wheel = 0b0000_1000 | 0b0001_0000 | 0b0010_0000;
        for mask in [
            button_mask(PointerButton::Left),
            button_mask(PointerButton::Middle),
            button_mask(PointerButton::Right),
        ] {
            assert_eq!(mask & wheel, 0, "{mask:#b} overlaps the wheel");
        }
    }

    #[test]
    fn a_coordinate_off_the_desktop_lands_at_the_edge() {
        assert_eq!(clamp(0), 0);
        assert_eq!(clamp(1920), 1920);
        assert_eq!(clamp(999_999), u16::MAX);
    }
}
