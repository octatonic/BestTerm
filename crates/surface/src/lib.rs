//! The frame-based protocol boundary.
//!
//! RDP, VNC and forwarded X11 windows all present the same shape: a stream of pixel frames flowing
//! one way and input events flowing the other. [`GraphicalSurface`] is that shape.
//!
//! This trait exists from the first commit even though nothing implements it until phase 3. That is
//! on purpose. RDP and VNC are scheduled *before* SFTP, so a pane abstraction written
//! terminal-first would have to be torn open halfway through the project. Declaring both boundaries
//! now costs a few hundred lines and removes that risk — see `docs/ARCHITECTURE.md`.
//!
//! # Where the pixels live
//!
//! Frames are produced by a helper process (`bestterm-rdp`, `bestterm-vnc`) and reach the main
//! process through shared memory. The trait therefore never hands out an owned buffer or a
//! borrowed slice with a lifetime: [`GraphicalSurface::with_frame`] lends the current frame to a
//! closure for the duration of a lock. That is the only shape that works for a mutex-guarded
//! mapping without inventing a self-referential type.

use std::fmt;

pub use crossbeam_channel::Receiver as EventReceiver;

/// Errors a surface can report.
#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    /// The underlying handle or IPC channel failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The helper process died or the peer disconnected.
    #[error("surface is closed")]
    Closed,

    /// Authentication was rejected or could not be completed.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// The server offered nothing we can decode.
    #[error("unsupported encoding: {0}")]
    UnsupportedEncoding(String),

    /// A protocol-level failure that is not an I/O error.
    #[error("{0}")]
    Protocol(String),
}

/// Result alias used throughout the surface layer.
pub type Result<T> = std::result::Result<T, SurfaceError>;

/// Which protocol is behind a surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceKind {
    /// Microsoft Remote Desktop.
    Rdp,
    /// VNC / RFB.
    Vnc,
    /// A window from a forwarded X11 connection.
    X11,
    /// SPICE.
    Spice,
}

impl SurfaceKind {
    /// Short lowercase identifier, stable enough to persist in configuration.
    pub fn id(self) -> &'static str {
        match self {
            Self::Rdp => "rdp",
            Self::Vnc => "vnc",
            Self::X11 => "x11",
            Self::Spice => "spice",
        }
    }
}

impl fmt::Display for SurfaceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// Pixel dimensions of a framebuffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameSize {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl FrameSize {
    /// A framebuffer of `width` × `height` pixels.
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Total pixel count.
    pub fn pixel_count(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// Byte layout of a framebuffer.
///
/// Deliberately short: RDP and VNC are both negotiated down to a 32-bit format in practice, and
/// converting once in the helper process is cheaper than teaching the renderer every wire format
/// either protocol can theoretically offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Blue, green, red, alpha — what RDP and Windows hand over natively.
    Bgra8,
    /// Red, green, blue, alpha — what GPU upload paths generally want.
    Rgba8,
}

impl PixelFormat {
    /// Bytes occupied by a single pixel.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Bgra8 | Self::Rgba8 => 4,
        }
    }
}

/// An axis-aligned rectangle in framebuffer coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Everything needed to interpret the bytes of one frame.
#[derive(Clone, Debug)]
pub struct FrameMeta {
    /// Dimensions of the framebuffer.
    pub size: FrameSize,
    /// Byte layout.
    pub format: PixelFormat,
    /// Bytes per row, which may exceed `size.width * bytes_per_pixel` because of alignment.
    pub stride: u32,
    /// Regions that changed since the previous frame.
    ///
    /// Empty means "assume everything changed". Both protocols send incremental updates, so
    /// honouring this is the difference between uploading a few kilobytes and a few megabytes per
    /// frame.
    pub damage: Vec<Rect>,
    /// Monotonically increasing counter, so a consumer can tell whether it has already drawn this.
    pub generation: u64,
}

/// The shape the remote end asked the local pointer to take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    /// Ordinary arrow.
    Default,
    /// Text insertion bar.
    Text,
    /// Busy.
    Wait,
    /// Crosshair.
    Crosshair,
    /// The remote end draws its own cursor into the framebuffer; hide the local one.
    Hidden,
}

/// Something that happened on a surface.
#[derive(Clone, Debug)]
pub enum SurfaceEvent {
    /// A new frame is available; read it with [`GraphicalSurface::with_frame`].
    Frame(FrameMeta),
    /// The remote end changed the framebuffer size.
    Resized(FrameSize),
    /// The remote end asked for a different pointer shape.
    Cursor(CursorShape),
    /// The remote end put text on its clipboard.
    ClipboardOffer(String),
    /// The remote end's identity is not settled, and the connection is waiting on an answer.
    ///
    /// Arrives before any credential has been sent, which is the whole point of it: after that the
    /// password is already on the wire and asking is theatre. Answer with
    /// [`GraphicalSurface::answer_server_key`]. Nothing else about the connection progresses until
    /// then, so an answer that never comes is a connection that eventually gives up.
    AskAboutServerKey {
        /// The server, as the session named it.
        host: String,
        /// And on which port.
        port: u16,
        /// The key presented, in the form meant to be read aloud and compared.
        fingerprint: String,
        /// The key on record, when there was one and it did not match.
        ///
        /// `Some` is the serious case: something is answering for this address that was not
        /// answering for it before.
        expected: Option<String>,
    },
    /// The key this connection settled on, for whoever owns the store.
    ///
    /// Sent once, after the key was accepted and before any frame.
    ServerKeySettled {
        /// The digest, in the form the store keeps.
        fingerprint: String,
        /// False when it was already on record and nothing needs writing.
        store: bool,
    },
    /// The remote end disconnected.
    Closed {
        /// Human-readable reason, when the protocol supplied one.
        reason: Option<String>,
    },
    /// A recoverable problem worth surfacing to the user.
    Error(String),
}

/// Mouse buttons, in the order both RDP and VNC number them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    /// Primary.
    Left,
    /// Tertiary.
    Middle,
    /// Secondary.
    Right,
    /// First extra button.
    X1,
    /// Second extra button.
    X2,
}

/// Keyboard modifier state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Windows / Command key.
    pub meta: bool,
}

/// Input travelling towards the remote end.
///
/// Keys are carried as physical scancodes rather than characters: both RDP and VNC want the hardware
/// key, and the remote host applies its own keyboard layout. Sending characters instead is the
/// classic source of "my keyboard layout is wrong over RDP" bugs.
///
/// # Which scancodes
///
/// PC scan code set 1 — the make codes an IBM PC/XT keyboard produced and the ones RDP has carried
/// ever since. Keys that a real keyboard prefixes with `0xE0` are carried as `0x100 | code`: the
/// right-hand Control is `0x11D` rather than `0x1D`, which is the left-hand one. This had said "USB
/// HID usage / RDP scancode", which are two different numbering schemes, and a contract that names
/// two answers has none — a right Alt would have arrived as a left Alt, or as nothing.
///
/// Set 1 rather than HID because it is what RDP puts on the wire unchanged. VNC wants X keysyms and
/// converts through a table either way, so nothing is saved by meeting it halfway, and the conversion
/// is better done in the backend that needs it than in every producer.
#[derive(Clone, Debug)]
pub enum InputEvent {
    /// A key transition, identified by its position on the keyboard.
    Key {
        /// PC scan code set 1, with extended keys as `0x100 | code`. See the type's documentation.
        scancode: u32,
        /// True on press, false on release.
        pressed: bool,
        /// Modifier state at the time of the transition.
        mods: Modifiers,
    },
    /// Composed text, for input methods that cannot be expressed as scancodes.
    Text(String),
    /// The pointer moved to a position in framebuffer coordinates.
    PointerMove {
        /// Horizontal position.
        x: u32,
        /// Vertical position.
        y: u32,
    },
    /// A pointer button transition.
    PointerButton {
        /// Which button.
        button: PointerButton,
        /// True on press, false on release.
        pressed: bool,
        /// Horizontal position.
        x: u32,
        /// Vertical position.
        y: u32,
    },
    /// A scroll gesture, in lines.
    Scroll {
        /// Horizontal delta.
        dx: f32,
        /// Vertical delta.
        dy: f32,
    },
    /// Our clipboard content, in response to a request from the remote end.
    ClipboardProvide(String),
}

/// A live frame-based connection.
pub trait GraphicalSurface: Send {
    /// Which protocol this is.
    fn kind(&self) -> SurfaceKind;

    /// Forward input to the remote end.
    fn send_input(&mut self, input: InputEvent) -> Result<()>;

    /// Ask the remote end to change the framebuffer size.
    ///
    /// Advisory: servers without dynamic resize simply ignore it, and the UI must letterbox or
    /// scale instead of assuming success.
    fn request_resize(&mut self, size: FrameSize) -> Result<()>;

    /// Lend the most recently completed frame to `f`.
    ///
    /// The closure runs while a lock is held, so it must copy or upload and return — never block,
    /// and never call back into the surface.
    fn with_frame(&self, f: &mut dyn FnMut(&FrameMeta, &[u8]));

    /// Answer a [`SurfaceEvent::AskAboutServerKey`].
    ///
    /// Refusing by default rather than accepting: a protocol backend that never asks will never have
    /// this called, and one that asks and then silently succeeded on an unimplemented answer would be
    /// accepting a server nobody looked at. There is no safe default except "no".
    fn answer_server_key(&mut self, _accept: bool) -> Result<()> {
        Err(SurfaceError::Protocol(
            "this surface did not ask about a server key".to_string(),
        ))
    }

    /// Close the connection. Idempotent.
    fn shutdown(&mut self) -> Result<()>;

    /// A label for the UI, typically `host` or `user@host`.
    fn label(&self) -> String;
}

/// A surface plus the channel its events arrive on.
pub struct OpenSurface {
    /// The input and control half.
    pub surface: Box<dyn GraphicalSurface>,
    /// The frame and status half.
    pub events: EventReceiver<SurfaceEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_size_pixel_count_does_not_overflow_u32() {
        // 65535 x 65535 overflows u32 when multiplied; the u64 return type is the point.
        let big = FrameSize::new(65_535, 65_535);
        assert_eq!(big.pixel_count(), 65_535u64 * 65_535u64);
    }

    #[test]
    fn every_format_is_four_bytes() {
        for f in [PixelFormat::Bgra8, PixelFormat::Rgba8] {
            assert_eq!(f.bytes_per_pixel(), 4);
        }
    }

    #[test]
    fn kind_ids_are_unique() {
        let kinds = [
            SurfaceKind::Rdp,
            SurfaceKind::Vnc,
            SurfaceKind::X11,
            SurfaceKind::Spice,
        ];
        let mut ids: Vec<_> = kinds.iter().map(|k| k.id()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }
}
