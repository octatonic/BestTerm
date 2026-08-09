//! What the host and a helper process say to each other.
//!
//! Two enums, one per direction, because the directions are not symmetric: the host asks for things
//! and sends input, the helper reports what happened. Splitting them means a decoder can never be
//! handed a message meant for the other side.
//!
//! Pixels are not in here. They travel through the shared mapping described in [`crate::shared`];
//! what crosses this channel is only the note saying a frame is ready and where in it to look.

use bestterm_core_vault::Secret;
use bestterm_surface::{
    CursorShape, FrameSize, InputEvent, Modifiers, PixelFormat, PointerButton, Rect,
};

use crate::codec::{CodecError, CodecResult, Decoder, Encode as _};

/// The protocol version this build speaks.
///
/// Sent in [`HelperMessage::Ready`] so a host talking to a helper from a different build finds out
/// immediately, rather than by misreading a message whose shape changed.
pub const PROTOCOL_VERSION: u32 = 1;

/// The largest message either side will send or accept.
///
/// Only clipboard text approaches this. The limit exists so a corrupt length prefix fails the read
/// instead of becoming an allocation.
pub const MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;

// Message tags. Written out rather than derived from declaration order, because reordering the
// enum must not silently change the wire format.
const HOST_CONNECT: u8 = 1;
const HOST_INPUT: u8 = 2;
const HOST_RESIZE: u8 = 3;
const HOST_SHUTDOWN: u8 = 4;

const HELPER_READY: u8 = 1;
const HELPER_FRAME: u8 = 2;
const HELPER_RESIZED: u8 = 3;
const HELPER_CURSOR: u8 = 4;
const HELPER_CLIPBOARD: u8 = 5;
const HELPER_CLOSED: u8 = 6;
const HELPER_ERROR: u8 = 7;

const INPUT_KEY: u8 = 1;
const INPUT_TEXT: u8 = 2;
const INPUT_POINTER_MOVE: u8 = 3;
const INPUT_POINTER_BUTTON: u8 = 4;
const INPUT_SCROLL: u8 = 5;
const INPUT_CLIPBOARD: u8 = 6;

/// Everything needed to open a session.
///
/// Carried in one message rather than set field by field, so a helper is never half-configured and
/// there is exactly one moment at which connecting can be attempted.
#[derive(Clone)]
pub struct ConnectRequest {
    /// Host name or address of the RDP server.
    pub host: String,
    /// TCP port, conventionally 3389.
    pub port: u16,
    /// Login name.
    pub username: String,
    /// Windows domain, when the account is a domain account.
    pub domain: Option<String>,
    /// The password.
    pub password: Secret,
    /// Desktop size to ask for.
    pub desktop_size: FrameSize,
    /// Whether to authenticate before the desktop is shown.
    ///
    /// This is what Windows calls Network Level Authentication. Off only for servers old enough not
    /// to offer it, because with it off the credentials are typed into the remote login screen
    /// instead of being proven up front.
    pub enable_credssp: bool,
    /// Windows keyboard layout identifier, or 0 to let the server decide.
    pub keyboard_layout: u32,
    /// Name the session appears under in the server's logs.
    pub client_name: String,
}

impl std::fmt::Debug for ConnectRequest {
    /// Written by hand so the password cannot reach a log through this type.
    ///
    /// `Secret` redacts itself, but a derived impl would still print the field name next to it and
    /// invite someone to add a plain `String` beside it later.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectRequest")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("domain", &self.domain)
            .field("desktop_size", &self.desktop_size)
            .field("enable_credssp", &self.enable_credssp)
            .finish_non_exhaustive()
    }
}

/// A frame the helper has finished writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameReady {
    /// Which generation of the shared buffer holds it.
    pub generation: u64,
    /// Dimensions of the framebuffer.
    pub size: FrameSize,
    /// Bytes per row.
    pub stride: u32,
    /// Byte layout of a pixel.
    pub format: PixelFormat,
    /// What changed since the previous frame; empty means all of it.
    pub damage: Vec<Rect>,
}

/// Host to helper.
#[derive(Clone, Debug)]
pub enum HostMessage {
    /// Open a session.
    Connect(Box<ConnectRequest>),
    /// Forward input to the server.
    Input(InputEvent),
    /// Ask the server for a different desktop size.
    Resize(FrameSize),
    /// Close the session and exit.
    Shutdown,
}

/// Helper to host.
#[derive(Clone, Debug)]
pub enum HelperMessage {
    /// The shared mapping exists and the session is up.
    Ready {
        /// Protocol version the helper speaks.
        version: u32,
        /// Where the shared mapping can be opened.
        mapping: String,
        /// How many frames the mapping holds.
        slot_count: u32,
        /// Bytes reserved for one frame.
        slot_bytes: u64,
    },
    /// A frame is ready to read.
    Frame(FrameReady),
    /// The server changed the desktop size.
    Resized(FrameSize),
    /// The server asked for a different pointer shape.
    Cursor(CursorShape),
    /// The server put text on its clipboard.
    ClipboardOffer(String),
    /// The session ended.
    Closed {
        /// Why, when the protocol said.
        reason: Option<String>,
    },
    /// Something went wrong that the session survived.
    Error(String),
}

impl HostMessage {
    /// The bytes of this message, without a length prefix.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Connect(request) => {
                out.put_u8(HOST_CONNECT);
                out.put_str(&request.host);
                out.put_u16(request.port);
                out.put_str(&request.username);
                put_option_str(&mut out, request.domain.as_deref());
                out.put_str(request.password.expose());
                put_size(&mut out, request.desktop_size);
                out.put_bool(request.enable_credssp);
                out.put_u32(request.keyboard_layout);
                out.put_str(&request.client_name);
            }
            Self::Input(input) => {
                out.put_u8(HOST_INPUT);
                put_input(&mut out, input);
            }
            Self::Resize(size) => {
                out.put_u8(HOST_RESIZE);
                put_size(&mut out, *size);
            }
            Self::Shutdown => out.put_u8(HOST_SHUTDOWN),
        }
        out
    }

    /// Read a message from the bytes [`HostMessage::encode`] produced.
    ///
    /// Every byte has to be accounted for; see [`CodecError::TrailingBytes`].
    pub fn decode(bytes: &[u8]) -> CodecResult<Self> {
        let mut d = Decoder::new(bytes);
        let message = Self::read(&mut d)?;
        d.finish()?;
        Ok(message)
    }

    fn read(d: &mut Decoder<'_>) -> CodecResult<Self> {
        let tag = d.u8()?;
        match tag {
            HOST_CONNECT => Ok(Self::Connect(Box::new(ConnectRequest {
                host: d.string()?,
                port: d.u16()?,
                username: d.string()?,
                domain: take_option_str(d)?,
                password: Secret::new(d.string()?),
                desktop_size: take_size(d)?,
                enable_credssp: d.bool()?,
                keyboard_layout: d.u32()?,
                client_name: d.string()?,
            }))),
            HOST_INPUT => Ok(Self::Input(take_input(d)?)),
            HOST_RESIZE => Ok(Self::Resize(take_size(d)?)),
            HOST_SHUTDOWN => Ok(Self::Shutdown),
            tag => Err(CodecError::UnknownTag {
                what: "host message",
                tag,
            }),
        }
    }
}

impl HelperMessage {
    /// The bytes of this message, without a length prefix.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Ready {
                version,
                mapping,
                slot_count,
                slot_bytes,
            } => {
                out.put_u8(HELPER_READY);
                out.put_u32(*version);
                out.put_str(mapping);
                out.put_u32(*slot_count);
                out.put_u64(*slot_bytes);
            }
            Self::Frame(frame) => {
                out.put_u8(HELPER_FRAME);
                out.put_u64(frame.generation);
                put_size(&mut out, frame.size);
                out.put_u32(frame.stride);
                out.put_u8(format_tag(frame.format));
                out.put_len(frame.damage.len());
                for rect in &frame.damage {
                    out.put_u32(rect.x);
                    out.put_u32(rect.y);
                    out.put_u32(rect.width);
                    out.put_u32(rect.height);
                }
            }
            Self::Resized(size) => {
                out.put_u8(HELPER_RESIZED);
                put_size(&mut out, *size);
            }
            Self::Cursor(shape) => {
                out.put_u8(HELPER_CURSOR);
                out.put_u8(cursor_tag(*shape));
            }
            Self::ClipboardOffer(text) => {
                out.put_u8(HELPER_CLIPBOARD);
                out.put_str(text);
            }
            Self::Closed { reason } => {
                out.put_u8(HELPER_CLOSED);
                put_option_str(&mut out, reason.as_deref());
            }
            Self::Error(detail) => {
                out.put_u8(HELPER_ERROR);
                out.put_str(detail);
            }
        }
        out
    }

    /// Read a message from the bytes [`HelperMessage::encode`] produced.
    ///
    /// Every byte has to be accounted for; see [`CodecError::TrailingBytes`].
    pub fn decode(bytes: &[u8]) -> CodecResult<Self> {
        let mut d = Decoder::new(bytes);
        let message = Self::read(&mut d)?;
        d.finish()?;
        Ok(message)
    }

    fn read(d: &mut Decoder<'_>) -> CodecResult<Self> {
        let tag = d.u8()?;
        match tag {
            HELPER_READY => Ok(Self::Ready {
                version: d.u32()?,
                mapping: d.string()?,
                slot_count: d.u32()?,
                slot_bytes: d.u64()?,
            }),
            HELPER_FRAME => {
                let generation = d.u64()?;
                let size = take_size(d)?;
                let stride = d.u32()?;
                let format = take_format(d)?;
                let count = d.len()?;
                let mut damage = Vec::new();
                // Reserved only after each rectangle is read: `count` came from another process, and
                // reserving up front would let a corrupt count allocate before it is disproved.
                for _ in 0..count {
                    damage.push(Rect {
                        x: d.u32()?,
                        y: d.u32()?,
                        width: d.u32()?,
                        height: d.u32()?,
                    });
                }
                Ok(Self::Frame(FrameReady {
                    generation,
                    size,
                    stride,
                    format,
                    damage,
                }))
            }
            HELPER_RESIZED => Ok(Self::Resized(take_size(d)?)),
            HELPER_CURSOR => Ok(Self::Cursor(take_cursor(d)?)),
            HELPER_CLIPBOARD => Ok(Self::ClipboardOffer(d.string()?)),
            HELPER_CLOSED => Ok(Self::Closed {
                reason: take_option_str(d)?,
            }),
            HELPER_ERROR => Ok(Self::Error(d.string()?)),
            tag => Err(CodecError::UnknownTag {
                what: "helper message",
                tag,
            }),
        }
    }
}

fn put_size(out: &mut Vec<u8>, size: FrameSize) {
    out.put_u32(size.width);
    out.put_u32(size.height);
}

fn take_size(d: &mut Decoder<'_>) -> CodecResult<FrameSize> {
    Ok(FrameSize::new(d.u32()?, d.u32()?))
}

fn put_option_str(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(text) => {
            out.put_bool(true);
            out.put_str(text);
        }
        None => out.put_bool(false),
    }
}

fn take_option_str(d: &mut Decoder<'_>) -> CodecResult<Option<String>> {
    if d.bool()? {
        Ok(Some(d.string()?))
    } else {
        Ok(None)
    }
}

fn format_tag(format: PixelFormat) -> u8 {
    match format {
        PixelFormat::Bgra8 => 1,
        PixelFormat::Rgba8 => 2,
    }
}

fn take_format(d: &mut Decoder<'_>) -> CodecResult<PixelFormat> {
    match d.u8()? {
        1 => Ok(PixelFormat::Bgra8),
        2 => Ok(PixelFormat::Rgba8),
        tag => Err(CodecError::UnknownTag {
            what: "pixel format",
            tag,
        }),
    }
}

fn cursor_tag(shape: CursorShape) -> u8 {
    match shape {
        CursorShape::Default => 1,
        CursorShape::Text => 2,
        CursorShape::Wait => 3,
        CursorShape::Crosshair => 4,
        CursorShape::Hidden => 5,
    }
}

fn take_cursor(d: &mut Decoder<'_>) -> CodecResult<CursorShape> {
    match d.u8()? {
        1 => Ok(CursorShape::Default),
        2 => Ok(CursorShape::Text),
        3 => Ok(CursorShape::Wait),
        4 => Ok(CursorShape::Crosshair),
        5 => Ok(CursorShape::Hidden),
        tag => Err(CodecError::UnknownTag {
            what: "cursor shape",
            tag,
        }),
    }
}

fn button_tag(button: PointerButton) -> u8 {
    match button {
        PointerButton::Left => 1,
        PointerButton::Middle => 2,
        PointerButton::Right => 3,
        PointerButton::X1 => 4,
        PointerButton::X2 => 5,
    }
}

fn take_button(d: &mut Decoder<'_>) -> CodecResult<PointerButton> {
    match d.u8()? {
        1 => Ok(PointerButton::Left),
        2 => Ok(PointerButton::Middle),
        3 => Ok(PointerButton::Right),
        4 => Ok(PointerButton::X1),
        5 => Ok(PointerButton::X2),
        tag => Err(CodecError::UnknownTag {
            what: "pointer button",
            tag,
        }),
    }
}

/// Modifiers as one byte, so a key event stays small enough not to matter at typing speed.
fn modifier_bits(mods: Modifiers) -> u8 {
    u8::from(mods.shift)
        | (u8::from(mods.ctrl) << 1)
        | (u8::from(mods.alt) << 2)
        | (u8::from(mods.meta) << 3)
}

fn modifiers_from_bits(bits: u8) -> Modifiers {
    Modifiers {
        shift: bits & 1 != 0,
        ctrl: bits & 2 != 0,
        alt: bits & 4 != 0,
        meta: bits & 8 != 0,
    }
}

fn put_input(out: &mut Vec<u8>, input: &InputEvent) {
    match input {
        InputEvent::Key {
            scancode,
            pressed,
            mods,
        } => {
            out.put_u8(INPUT_KEY);
            out.put_u32(*scancode);
            out.put_bool(*pressed);
            out.put_u8(modifier_bits(*mods));
        }
        InputEvent::Text(text) => {
            out.put_u8(INPUT_TEXT);
            out.put_str(text);
        }
        InputEvent::PointerMove { x, y } => {
            out.put_u8(INPUT_POINTER_MOVE);
            out.put_u32(*x);
            out.put_u32(*y);
        }
        InputEvent::PointerButton {
            button,
            pressed,
            x,
            y,
        } => {
            out.put_u8(INPUT_POINTER_BUTTON);
            out.put_u8(button_tag(*button));
            out.put_bool(*pressed);
            out.put_u32(*x);
            out.put_u32(*y);
        }
        InputEvent::Scroll { dx, dy } => {
            out.put_u8(INPUT_SCROLL);
            out.put_f32(*dx);
            out.put_f32(*dy);
        }
        InputEvent::ClipboardProvide(text) => {
            out.put_u8(INPUT_CLIPBOARD);
            out.put_str(text);
        }
    }
}

fn take_input(d: &mut Decoder<'_>) -> CodecResult<InputEvent> {
    match d.u8()? {
        INPUT_KEY => Ok(InputEvent::Key {
            scancode: d.u32()?,
            pressed: d.bool()?,
            mods: modifiers_from_bits(d.u8()?),
        }),
        INPUT_TEXT => Ok(InputEvent::Text(d.string()?)),
        INPUT_POINTER_MOVE => Ok(InputEvent::PointerMove {
            x: d.u32()?,
            y: d.u32()?,
        }),
        INPUT_POINTER_BUTTON => Ok(InputEvent::PointerButton {
            button: take_button(d)?,
            pressed: d.bool()?,
            x: d.u32()?,
            y: d.u32()?,
        }),
        INPUT_SCROLL => Ok(InputEvent::Scroll {
            dx: d.f32()?,
            dy: d.f32()?,
        }),
        INPUT_CLIPBOARD => Ok(InputEvent::ClipboardProvide(d.string()?)),
        tag => Err(CodecError::UnknownTag {
            what: "input event",
            tag,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ConnectRequest {
        ConnectRequest {
            host: "rdp.int".to_string(),
            port: 3389,
            username: "administrator".to_string(),
            domain: Some("CORP".to_string()),
            password: Secret::new("hunter2".to_string()),
            desktop_size: FrameSize::new(1920, 1080),
            enable_credssp: true,
            keyboard_layout: 0x0409,
            client_name: "bestterm".to_string(),
        }
    }

    #[test]
    fn a_connect_request_survives_the_round_trip_intact() {
        let encoded = HostMessage::Connect(Box::new(request())).encode();
        let decoded = HostMessage::decode(&encoded).expect("decodes");

        let HostMessage::Connect(back) = decoded else {
            panic!("expected a connect message");
        };
        assert_eq!(back.host, "rdp.int");
        assert_eq!(back.port, 3389);
        assert_eq!(back.username, "administrator");
        assert_eq!(back.domain.as_deref(), Some("CORP"));
        assert_eq!(back.password.expose(), "hunter2");
        assert_eq!(back.desktop_size, FrameSize::new(1920, 1080));
        assert!(back.enable_credssp);
        assert_eq!(back.keyboard_layout, 0x0409);
        assert_eq!(back.client_name, "bestterm");
    }

    #[test]
    fn a_connect_request_does_not_print_its_password() {
        // The one field that must never reach a log. Checked on the type that carries it across a
        // process boundary, which is exactly where something would think to log the whole message.
        let printed = format!("{:?}", request());
        assert!(!printed.contains("hunter2"), "leaked: {printed}");
        assert!(printed.contains("rdp.int"), "still useful: {printed}");
    }

    #[test]
    fn an_absent_domain_is_told_apart_from_an_empty_one() {
        // A local account and an account in a domain named "" are different things to a server.
        let mut none = request();
        none.domain = None;
        let mut empty = request();
        empty.domain = Some(String::new());

        let decode = |request: ConnectRequest| {
            let encoded = HostMessage::Connect(Box::new(request)).encode();
            match HostMessage::decode(&encoded).expect("decodes") {
                HostMessage::Connect(back) => back.domain,
                other => panic!("expected connect, got {other:?}"),
            }
        };

        assert_eq!(decode(none), None);
        assert_eq!(decode(empty), Some(String::new()));
    }

    #[test]
    fn every_input_event_survives_the_round_trip() {
        let events = [
            InputEvent::Key {
                scancode: 0x1C,
                pressed: true,
                mods: Modifiers {
                    shift: true,
                    ctrl: false,
                    alt: true,
                    meta: false,
                },
            },
            InputEvent::Text("日本語".to_string()),
            InputEvent::PointerMove { x: 640, y: 480 },
            InputEvent::PointerButton {
                button: PointerButton::X2,
                pressed: false,
                x: 1,
                y: 2,
            },
            InputEvent::Scroll { dx: -0.5, dy: 3.25 },
            InputEvent::ClipboardProvide("copied".to_string()),
        ];

        for event in events {
            let encoded = HostMessage::Input(event.clone()).encode();
            let decoded = HostMessage::decode(&encoded).expect("decodes");
            let HostMessage::Input(back) = decoded else {
                panic!("expected an input message");
            };
            assert_eq!(format!("{back:?}"), format!("{event:?}"));
        }
    }

    #[test]
    fn every_modifier_combination_maps_to_itself() {
        // Packed into one byte by hand, so each bit is worth checking rather than assuming.
        for bits in 0u8..16 {
            let mods = modifiers_from_bits(bits);
            assert_eq!(modifier_bits(mods), bits, "bits {bits} did not survive");
        }
    }

    #[test]
    fn a_frame_notice_carries_its_damage_list() {
        let frame = FrameReady {
            generation: 42,
            size: FrameSize::new(800, 600),
            stride: 3200,
            format: PixelFormat::Bgra8,
            damage: vec![
                Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                Rect {
                    x: 100,
                    y: 200,
                    width: 5,
                    height: 5,
                },
            ],
        };

        let encoded = HelperMessage::Frame(frame.clone()).encode();
        match HelperMessage::decode(&encoded).expect("decodes") {
            HelperMessage::Frame(back) => assert_eq!(back, frame),
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_damage_list_stays_empty() {
        // Empty means "all of it", so turning it into a one-rectangle list would be a real change in
        // meaning rather than a harmless normalisation.
        let frame = FrameReady {
            generation: 1,
            size: FrameSize::new(64, 64),
            stride: 256,
            format: PixelFormat::Rgba8,
            damage: Vec::new(),
        };
        let encoded = HelperMessage::Frame(frame.clone()).encode();
        match HelperMessage::decode(&encoded).expect("decodes") {
            HelperMessage::Frame(back) => assert!(back.damage.is_empty()),
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    #[test]
    fn every_helper_message_round_trips() {
        let messages = [
            HelperMessage::Ready {
                version: PROTOCOL_VERSION,
                mapping: "bestterm-rdp-1234".to_string(),
                slot_count: 3,
                slot_bytes: 8_294_400,
            },
            HelperMessage::Resized(FrameSize::new(1280, 720)),
            HelperMessage::Cursor(CursorShape::Hidden),
            HelperMessage::ClipboardOffer("text".to_string()),
            HelperMessage::Closed {
                reason: Some("the server logged us off".to_string()),
            },
            HelperMessage::Closed { reason: None },
            HelperMessage::Error("decoder fell behind".to_string()),
        ];

        for message in messages {
            let encoded = message.encode();
            let back = HelperMessage::decode(&encoded).expect("decodes");
            assert_eq!(format!("{back:?}"), format!("{message:?}"));
        }
    }

    #[test]
    fn every_cursor_shape_has_its_own_tag() {
        let shapes = [
            CursorShape::Default,
            CursorShape::Text,
            CursorShape::Wait,
            CursorShape::Crosshair,
            CursorShape::Hidden,
        ];
        let mut tags: Vec<u8> = shapes.iter().map(|s| cursor_tag(*s)).collect();
        let count = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), count, "two shapes share a tag");
    }

    #[test]
    fn every_pointer_button_has_its_own_tag() {
        let buttons = [
            PointerButton::Left,
            PointerButton::Middle,
            PointerButton::Right,
            PointerButton::X1,
            PointerButton::X2,
        ];
        let mut tags: Vec<u8> = buttons.iter().map(|b| button_tag(*b)).collect();
        let count = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), count, "two buttons share a tag");
    }

    #[test]
    fn a_message_from_a_newer_build_is_refused_by_name() {
        // What a version mismatch actually looks like on the wire. Saying which tag was not
        // understood is the difference between a bug report and a shrug.
        let error = HostMessage::decode(&[200]).expect_err("tag 200 is not defined");
        assert_eq!(
            error,
            CodecError::UnknownTag {
                what: "host message",
                tag: 200
            }
        );

        let error = HelperMessage::decode(&[201]).expect_err("tag 201 is not defined");
        assert!(matches!(error, CodecError::UnknownTag { .. }), "{error:?}");
    }

    #[test]
    fn a_message_with_bytes_nobody_read_is_refused() {
        // A field this build does not know about. Skipping past it would let the two sides carry on
        // while quietly disagreeing about what was said, which is worse than stopping.
        let mut encoded = HostMessage::Shutdown.encode();
        encoded.push(0);

        let error = HostMessage::decode(&encoded).expect_err("one byte too many");
        assert_eq!(error, CodecError::TrailingBytes { count: 1 });

        let mut encoded = HelperMessage::Cursor(CursorShape::Wait).encode();
        encoded.extend_from_slice(&[1, 2, 3]);
        let error = HelperMessage::decode(&encoded).expect_err("three too many");
        assert_eq!(error, CodecError::TrailingBytes { count: 3 });
    }

    #[test]
    fn an_empty_message_is_refused_rather_than_defaulted() {
        assert!(HostMessage::decode(&[]).is_err());
        assert!(HelperMessage::decode(&[]).is_err());
    }

    #[test]
    fn a_truncated_message_never_decodes_to_something_plausible() {
        // Every prefix of a real message must fail. Reading a short one as a valid message with
        // zeroed tail is the failure mode that turns a dropped connection into a phantom click.
        let full = HostMessage::Connect(Box::new(request())).encode();
        for cut in 1..full.len() {
            assert!(
                HostMessage::decode(&full[..cut]).is_err(),
                "a {cut}-byte prefix decoded as a whole message"
            );
        }
    }
}
