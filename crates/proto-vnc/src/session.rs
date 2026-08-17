//! The connection: handshake, then a loop of framebuffer updates.
//!
//! # The shape of an RFB connection
//!
//! 1. Both ends announce a version as twelve ASCII bytes. The client picks the lower of the two, which
//!    is how a 3.8 client talks to a 3.3 server.
//! 2. The server lists security types; the client picks one and authenticates.
//! 3. The client says whether it minds sharing the desktop; the server describes it.
//! 4. The client asks for a pixel format and a list of encodings.
//! 5. Then it is updates, forever, each one asked for.
//!
//! # Updates are pulled, not pushed
//!
//! This is the piece that surprises people coming from RDP. A VNC server sends nothing until asked,
//! and it answers one request with one update — so the client has to ask again immediately after every
//! update, forever. Forgetting to means a session that draws its first frame and then freezes, which
//! looks exactly like a hung decoder.
//!
//! The requests are *incremental* after the first: "tell me what changed". The first one is not, and
//! that is what paints the initial desktop.

use std::io::Cursor;

use bestterm_core_vault::Secret;
use bestterm_surface::Rect;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::auth::{self, Security};
use crate::decode::{self, DecodeError, Framebuffer};
use crate::pixels::{PIXEL_FORMAT_LEN, PixelFormat};

/// The version this client speaks.
const OUR_VERSION: (u8, u8) = (3, 8);

/// Largest desktop this will allocate for.
///
/// A server states the size and the client believes it, so this is the one place a bad number becomes
/// a bad allocation. 16384 on a side is beyond any real desktop and still only a gigabyte.
const MAX_DIMENSION: u16 = 16_384;

/// Largest rectangle payload accepted.
///
/// A rectangle's length is the server's to choose. Uncompressed, the largest legitimate one is a whole
/// screen; this is comfortably above that and well below anything that would exhaust memory.
const MAX_RECTANGLE: usize = 64 * 1024 * 1024;

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum VncError {
    /// The socket failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The server is not speaking RFB.
    #[error("this does not look like a VNC server (it said {0:?})")]
    NotRfb(String),

    /// The server offered nothing this build can do.
    ///
    /// Names what it offered, because the answer is usually "turn off the encryption plugin" or "this
    /// is a Tight-only server", and neither is guessable from "authentication failed".
    #[error("the server offered no security type this build supports (it offered {offered:?})")]
    NoSharedSecurity {
        /// The codes it listed.
        offered: Vec<u8>,
    },

    /// The server refused the connection, with its own words.
    #[error("the server refused the connection: {0}")]
    Refused(String),

    /// The password was wrong, or none was given and one was needed.
    #[error("the server rejected the password")]
    PasswordRejected,

    /// The server described a desktop this cannot allocate for.
    #[error("the server says its desktop is {width}x{height}, which is not a size")]
    ImpossibleDesktop {
        /// What it said.
        width: u16,
        /// As above.
        height: u16,
    },

    /// A rectangle could not be decoded.
    #[error(transparent)]
    Decode(#[from] DecodeError),

    /// The server said something the protocol does not define.
    #[error("{0}")]
    Protocol(String),
}

/// What a connected session knows about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Desktop {
    /// What the server calls itself.
    pub name: String,
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// How the handshake was authenticated.
    pub security: Security,
}

/// Something that happened during an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    /// Pixels changed in these regions.
    Damage(Vec<Rect>),
    /// The desktop changed size, and the framebuffer was rebuilt.
    Resized {
        /// New width.
        width: u32,
        /// New height.
        height: u32,
    },
}

/// Read the twelve-byte version banner and answer it.
///
/// Returns the version agreed. A server older than 3.3 is not something this speaks, and a banner
/// that is not a version at all is almost always a different protocol on the port.
pub async fn handshake_version<S>(stream: &mut S) -> Result<(u8, u8), VncError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut banner = [0u8; 12];
    stream.read_exact(&mut banner).await?;

    let text = String::from_utf8_lossy(&banner).to_string();
    let version =
        parse_version(&banner).ok_or_else(|| VncError::NotRfb(text.trim_end().to_string()))?;

    // The lower of the two, which is what lets a 3.8 client talk to a 3.3 server.
    let agreed = if version < OUR_VERSION {
        version
    } else {
        OUR_VERSION
    };
    if agreed < (3, 3) {
        return Err(VncError::Protocol(format!(
            "RFB {}.{} is older than anything this speaks",
            agreed.0, agreed.1
        )));
    }

    let reply = format!("RFB {:03}.{:03}\n", agreed.0, agreed.1);
    stream.write_all(reply.as_bytes()).await?;
    stream.flush().await?;
    Ok(agreed)
}

/// Read `RFB 003.008\n` and nothing else.
fn parse_version(banner: &[u8; 12]) -> Option<(u8, u8)> {
    let text = std::str::from_utf8(banner).ok()?;
    let rest = text.strip_prefix("RFB ")?;
    let (major, rest) = rest.split_at_checked(3)?;
    let rest = rest.strip_prefix('.')?;
    let (minor, tail) = rest.split_at_checked(3)?;
    if tail != "\n" {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Choose a security type from what the server offered, and authenticate.
pub async fn handshake_security<S>(
    stream: &mut S,
    version: (u8, u8),
    password: Option<&Secret>,
) -> Result<Security, VncError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let count = stream.read_u8().await?;
    if count == 0 {
        // Zero means refusal, and the reason follows. Reading it is the difference between "the
        // server refused" and "too many authentication failures, try again in 10 seconds".
        return Err(VncError::Refused(read_reason(stream, version).await?));
    }

    let mut offered = vec![0u8; usize::from(count)];
    stream.read_exact(&mut offered).await?;

    // The first one this build can do, in the server's order. Preferring VNC authentication over none
    // would be pointless -- a server offering `None` has decided nobody needs a password.
    let chosen = offered
        .iter()
        .find_map(|code| Security::from_code(*code))
        .ok_or_else(|| VncError::NoSharedSecurity {
            offered: offered.clone(),
        })?;

    stream.write_all(&[chosen.code()]).await?;
    stream.flush().await?;

    if chosen == Security::VncAuth {
        let mut challenge = [0u8; 16];
        stream.read_exact(&mut challenge).await?;
        let password = password.ok_or(VncError::PasswordRejected)?;
        let response = auth::respond(&challenge, password)
            .ok_or_else(|| VncError::Protocol("the challenge was not 16 bytes".to_string()))?;
        stream.write_all(&response).await?;
        stream.flush().await?;
    }

    // 3.8 always sends a result; 3.3 and 3.7 send one only after an authenticated exchange.
    let expects_result = version >= (3, 8) || chosen != Security::None;
    if expects_result {
        let result = stream.read_u32().await?;
        if result != 0 {
            // The reason is only present from 3.8 onwards. Older servers just close.
            if version >= (3, 8) {
                let reason = read_reason(stream, version).await.unwrap_or_default();
                if !reason.is_empty() {
                    return Err(VncError::Refused(reason));
                }
            }
            return Err(VncError::PasswordRejected);
        }
    }

    Ok(chosen)
}

/// Read a length-prefixed reason string.
async fn read_reason<S>(stream: &mut S, _version: (u8, u8)) -> Result<String, VncError>
where
    S: AsyncRead + Unpin,
{
    let length = stream.read_u32().await?;
    // The length is the server's to choose, and this arrives before anything is authenticated.
    let length = usize::try_from(length).unwrap_or(0).min(4096);
    let mut text = vec![0u8; length];
    stream.read_exact(&mut text).await?;
    Ok(String::from_utf8_lossy(&text).trim().to_string())
}

/// Say whether the desktop may be shared, and read its description.
pub async fn handshake_init<S>(
    stream: &mut S,
    shared: bool,
    security: Security,
) -> Result<Desktop, VncError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream.write_all(&[u8::from(shared)]).await?;
    stream.flush().await?;

    let width = stream.read_u16().await?;
    let height = stream.read_u16().await?;
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(VncError::ImpossibleDesktop { width, height });
    }

    let mut format = [0u8; PIXEL_FORMAT_LEN];
    stream.read_exact(&mut format).await?;
    // Read and discarded: a format is asked for immediately afterwards, and the server's own is only
    // interesting if it refuses. `PixelFormat::parse` exists for a caller that wants to check.
    let _ = PixelFormat::parse(&format);

    let name_length = stream.read_u32().await?;
    let name_length = usize::try_from(name_length).unwrap_or(0).min(4096);
    let mut name = vec![0u8; name_length];
    stream.read_exact(&mut name).await?;

    Ok(Desktop {
        name: String::from_utf8_lossy(&name).trim().to_string(),
        width,
        height,
        security,
    })
}

/// Ask for the pixel format and encodings this client wants.
pub async fn set_up<S>(stream: &mut S) -> Result<(), VncError>
where
    S: AsyncWrite + Unpin,
{
    // SetPixelFormat: message 0, three bytes of padding, then the format.
    let mut message = vec![0u8, 0, 0, 0];
    message.extend_from_slice(&PixelFormat::BGRA.encode());
    stream.write_all(&message).await?;

    // SetEncodings: message 2, one byte of padding, a count, then the list.
    let mut message = vec![2u8, 0];
    let count = u16::try_from(decode::ENCODINGS.len()).unwrap_or(u16::MAX);
    message.extend_from_slice(&count.to_be_bytes());
    for encoding in decode::ENCODINGS {
        message.extend_from_slice(&encoding.to_be_bytes());
    }
    stream.write_all(&message).await?;
    stream.flush().await?;
    Ok(())
}

/// Ask for an update.
///
/// Every update has to be asked for, and a session that stops asking freezes. See the module
/// documentation.
pub async fn request_update<S>(
    stream: &mut S,
    incremental: bool,
    width: u16,
    height: u16,
) -> Result<(), VncError>
where
    S: AsyncWrite + Unpin,
{
    let mut message = vec![3u8, u8::from(incremental)];
    message.extend_from_slice(&0u16.to_be_bytes());
    message.extend_from_slice(&0u16.to_be_bytes());
    message.extend_from_slice(&width.to_be_bytes());
    message.extend_from_slice(&height.to_be_bytes());
    stream.write_all(&message).await?;
    stream.flush().await?;
    Ok(())
}

/// Read one server message and act on it.
///
/// Returns what changed, or nothing for the messages that change no pixels — a bell, or the server
/// putting something on its clipboard, both of which are read and skipped so the stream stays in step.
pub async fn read_message<S>(
    stream: &mut S,
    framebuffer: &mut Framebuffer,
) -> Result<Vec<Update>, VncError>
where
    S: AsyncRead + Unpin,
{
    match stream.read_u8().await? {
        0 => read_framebuffer_update(stream, framebuffer).await,
        // SetColourMapEntries: only meaningful for an indexed format, which is not what was asked
        // for. Read past it rather than ignoring it, or everything after is misaligned.
        1 => {
            let mut header = [0u8; 5];
            stream.read_exact(&mut header).await?;
            let count = u16::from_be_bytes([header[3], header[4]]);
            let mut colours = vec![0u8; usize::from(count) * 6];
            stream.read_exact(&mut colours).await?;
            Ok(Vec::new())
        }
        // Bell.
        2 => Ok(Vec::new()),
        // ServerCutText: the server's clipboard.
        3 => {
            let mut header = [0u8; 7];
            stream.read_exact(&mut header).await?;
            let length = u32::from_be_bytes([header[3], header[4], header[5], header[6]]) as usize;
            let mut text = vec![0u8; length.min(1024 * 1024)];
            stream.read_exact(&mut text).await?;
            Ok(Vec::new())
        }
        other => Err(VncError::Protocol(format!(
            "the server sent message type {other}, which the protocol does not define"
        ))),
    }
}

/// Read a framebuffer update and apply every rectangle in it.
async fn read_framebuffer_update<S>(
    stream: &mut S,
    framebuffer: &mut Framebuffer,
) -> Result<Vec<Update>, VncError>
where
    S: AsyncRead + Unpin,
{
    let mut header = [0u8; 3];
    stream.read_exact(&mut header).await?;
    let count = u16::from_be_bytes([header[1], header[2]]);

    let mut updates = Vec::new();
    let mut damage = Vec::new();

    for _ in 0..count {
        let mut fields = [0u8; 12];
        stream.read_exact(&mut fields).await?;
        let rect = Rect {
            x: u32::from(u16::from_be_bytes([fields[0], fields[1]])),
            y: u32::from(u16::from_be_bytes([fields[2], fields[3]])),
            width: u32::from(u16::from_be_bytes([fields[4], fields[5]])),
            height: u32::from(u16::from_be_bytes([fields[6], fields[7]])),
        };
        let encoding = i32::from_be_bytes([fields[8], fields[9], fields[10], fields[11]]);

        match encoding {
            decode::RAW => {
                let needed = rect.width as usize * rect.height as usize * 4;
                if needed > MAX_RECTANGLE {
                    return Err(VncError::Protocol(format!(
                        "a raw rectangle of {needed} bytes is larger than anything this accepts"
                    )));
                }
                let mut data = vec![0u8; needed];
                stream.read_exact(&mut data).await?;
                framebuffer.apply_raw(rect, &data)?;
                damage.push(rect);
            }

            decode::COPY_RECT => {
                let mut from = [0u8; 4];
                stream.read_exact(&mut from).await?;
                framebuffer.apply_copy(
                    rect,
                    u32::from(u16::from_be_bytes([from[0], from[1]])),
                    u32::from(u16::from_be_bytes([from[2], from[3]])),
                )?;
                damage.push(rect);
            }

            decode::ZRLE => {
                let length = stream.read_u32().await? as usize;
                if length > MAX_RECTANGLE {
                    return Err(VncError::Protocol(format!(
                        "a ZRLE rectangle of {length} bytes is larger than anything this accepts"
                    )));
                }
                let mut data = vec![0u8; length];
                stream.read_exact(&mut data).await?;
                framebuffer.apply_zrle(rect, &data)?;
                damage.push(rect);
            }

            // Not a rectangle of pixels: the server saying the desktop is now this size. The
            // framebuffer is rebuilt and everything on it is gone, so the caller has to redraw --
            // which is why this ends the damage list rather than joining it.
            decode::DESKTOP_SIZE => {
                framebuffer.resize(rect.width, rect.height);
                if !damage.is_empty() {
                    updates.push(Update::Damage(std::mem::take(&mut damage)));
                }
                updates.push(Update::Resized {
                    width: rect.width,
                    height: rect.height,
                });
            }

            other => return Err(DecodeError::UnknownEncoding(other).into()),
        }
    }

    if !damage.is_empty() {
        updates.push(Update::Damage(damage));
    }
    Ok(updates)
}

/// Send a key transition, as an X11 keysym.
pub async fn send_key<S>(stream: &mut S, keysym: u32, pressed: bool) -> Result<(), VncError>
where
    S: AsyncWrite + Unpin,
{
    let mut message = vec![4u8, u8::from(pressed), 0, 0];
    message.extend_from_slice(&keysym.to_be_bytes());
    stream.write_all(&message).await?;
    stream.flush().await?;
    Ok(())
}

/// Send the pointer's position and which buttons are down.
///
/// RFB has no press or release: every pointer message is the *current* state of all the buttons, so
/// the caller has to track them. A client that sends a press and forgets to send the release leaves a
/// button held down on the remote desktop, which is how a VNC session ends up selecting everything.
pub async fn send_pointer<S>(stream: &mut S, buttons: u8, x: u16, y: u16) -> Result<(), VncError>
where
    S: AsyncWrite + Unpin,
{
    let mut message = vec![5u8, buttons];
    message.extend_from_slice(&x.to_be_bytes());
    message.extend_from_slice(&y.to_be_bytes());
    stream.write_all(&message).await?;
    stream.flush().await?;
    Ok(())
}

/// Read a message out of a slice, for tests and for callers that already have the bytes.
pub fn message_from_bytes(bytes: &[u8]) -> Cursor<&[u8]> {
    Cursor::new(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that reads from one buffer and collects writes into another.
    struct Pipe {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Pipe {
        fn new(input: Vec<u8>) -> Self {
            Self {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }
    }

    impl AsyncRead for Pipe {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.input).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for Pipe {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.output.extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn the_lower_version_wins_so_an_old_server_still_works() {
        let mut pipe = Pipe::new(b"RFB 003.003\n".to_vec());
        let agreed = handshake_version(&mut pipe).await.expect("a version");
        assert_eq!(agreed, (3, 3));
        assert_eq!(pipe.output, b"RFB 003.003\n");
    }

    #[tokio::test]
    async fn a_newer_server_is_answered_with_our_version() {
        let mut pipe = Pipe::new(b"RFB 004.001\n".to_vec());
        let agreed = handshake_version(&mut pipe).await.expect("a version");
        assert_eq!(agreed, OUR_VERSION);
        assert_eq!(pipe.output, b"RFB 003.008\n");
    }

    #[tokio::test]
    async fn something_that_is_not_a_vnc_server_says_so_with_what_it_said() {
        // Almost always a different service on the port, and quoting it is what makes that obvious.
        let mut pipe = Pipe::new(b"SSH-2.0-Open".to_vec());
        let error = handshake_version(&mut pipe).await.expect_err("not RFB");
        let message = error.to_string();
        assert!(message.contains("SSH-2.0-Open"), "{message}");
    }

    #[test]
    fn the_version_banner_is_parsed_strictly() {
        assert_eq!(parse_version(b"RFB 003.008\n"), Some((3, 8)));
        // A number that does not fit a byte is not a version this understands, and wrapping it
        // into one would agree a version neither end speaks.
        assert_eq!(
            parse_version(
                b"RFB 003.889
"
            ),
            None
        );
        // Anything not exactly this shape is not a version.
        assert_eq!(parse_version(b"RFB 3.8\n\0\0\0\0"), None);
        assert_eq!(parse_version(b"rfb 003.008\n"), None);
        assert_eq!(parse_version(b"RFB 003.008 "), None);
        assert_eq!(parse_version(b"RFB xxx.yyy\n"), None);
    }

    #[tokio::test]
    async fn a_server_offering_nothing_we_speak_names_what_it_offered() {
        // The answer is usually "turn off the encryption plugin" or "this is Tight-only", and neither
        // is guessable from "authentication failed".
        let mut pipe = Pipe::new(vec![2, 16, 18]);
        let error = handshake_security(&mut pipe, (3, 8), None)
            .await
            .expect_err("nothing shared");
        match error {
            VncError::NoSharedSecurity { offered } => assert_eq!(offered, vec![16, 18]),
            other => panic!("expected no shared security, got {other}"),
        }
    }

    #[tokio::test]
    async fn a_refusal_carries_the_servers_own_words() {
        // "Too many authentication failures" is worth reading; "the server refused" is not.
        let reason = b"too many security failures";
        let mut input = vec![0u8];
        input.extend_from_slice(&(reason.len() as u32).to_be_bytes());
        input.extend_from_slice(reason);

        let mut pipe = Pipe::new(input);
        let error = handshake_security(&mut pipe, (3, 8), None)
            .await
            .expect_err("refused");
        assert!(error.to_string().contains("too many"), "{error}");
    }

    #[tokio::test]
    async fn no_authentication_is_chosen_and_confirmed() {
        // 3.8 sends a result even for `None`; forgetting to read it leaves four bytes in the stream
        // and everything after is misaligned.
        let mut input = vec![1u8, 1];
        input.extend_from_slice(&0u32.to_be_bytes());
        let mut pipe = Pipe::new(input);

        let chosen = handshake_security(&mut pipe, (3, 8), None)
            .await
            .expect("no authentication");
        assert_eq!(chosen, Security::None);
        assert_eq!(pipe.output, vec![1]);
    }

    #[tokio::test]
    async fn a_password_challenge_is_answered_and_the_result_read() {
        let mut input = vec![1u8, 2];
        input.extend_from_slice(&[0x22u8; 16]);
        input.extend_from_slice(&0u32.to_be_bytes());
        let mut pipe = Pipe::new(input);

        let password = Secret::new("hunter2".to_string());
        let chosen = handshake_security(&mut pipe, (3, 8), Some(&password))
            .await
            .expect("authenticated");
        assert_eq!(chosen, Security::VncAuth);
        // The chosen type, then sixteen bytes of response.
        assert_eq!(pipe.output.len(), 1 + 16);
        assert_eq!(pipe.output[0], 2);
    }

    #[tokio::test]
    async fn a_password_that_is_needed_and_missing_is_reported_before_the_socket_is_used() {
        let mut input = vec![1u8, 2];
        input.extend_from_slice(&[0x22u8; 16]);
        let mut pipe = Pipe::new(input);
        assert!(matches!(
            handshake_security(&mut pipe, (3, 8), None).await,
            Err(VncError::PasswordRejected)
        ));
    }

    #[tokio::test]
    async fn a_desktop_of_no_size_is_refused_rather_than_allocated_for() {
        // The server states the size and the client believes it, so this is the one place a bad
        // number becomes a bad allocation.
        for (width, height) in [(0u16, 100u16), (100, 0), (u16::MAX, u16::MAX)] {
            let mut input = Vec::new();
            input.extend_from_slice(&width.to_be_bytes());
            input.extend_from_slice(&height.to_be_bytes());
            input.extend_from_slice(&[0u8; PIXEL_FORMAT_LEN]);
            input.extend_from_slice(&0u32.to_be_bytes());

            let mut pipe = Pipe::new(input);
            assert!(
                matches!(
                    handshake_init(&mut pipe, true, Security::None).await,
                    Err(VncError::ImpossibleDesktop { .. })
                ),
                "{width}x{height} should be refused"
            );
        }
    }

    #[tokio::test]
    async fn the_desktop_description_is_read_whole() {
        let name = b"jenkins-01 (tigervnc)";
        let mut input = Vec::new();
        input.extend_from_slice(&1920u16.to_be_bytes());
        input.extend_from_slice(&1080u16.to_be_bytes());
        input.extend_from_slice(&PixelFormat::BGRA.encode());
        input.extend_from_slice(&(name.len() as u32).to_be_bytes());
        input.extend_from_slice(name);

        let mut pipe = Pipe::new(input);
        let desktop = handshake_init(&mut pipe, true, Security::VncAuth)
            .await
            .expect("a desktop");

        assert_eq!(desktop.width, 1920);
        assert_eq!(desktop.height, 1080);
        assert_eq!(desktop.name, "jenkins-01 (tigervnc)");
        assert_eq!(desktop.security, Security::VncAuth);
        assert_eq!(pipe.output, vec![1], "shared, as asked");
    }

    #[tokio::test]
    async fn the_setup_asks_for_the_format_the_framebuffer_wants() {
        let mut pipe = Pipe::new(Vec::new());
        set_up(&mut pipe).await.expect("writes");

        assert_eq!(pipe.output[0], 0, "SetPixelFormat");
        assert_eq!(
            &pipe.output[4..4 + PIXEL_FORMAT_LEN],
            &PixelFormat::BGRA.encode()
        );

        let encodings_at = 4 + PIXEL_FORMAT_LEN;
        assert_eq!(pipe.output[encodings_at], 2, "SetEncodings");
        let count =
            u16::from_be_bytes([pipe.output[encodings_at + 2], pipe.output[encodings_at + 3]]);
        assert_eq!(usize::from(count), decode::ENCODINGS.len());
    }

    #[tokio::test]
    async fn an_update_request_says_whether_it_wants_everything() {
        let mut pipe = Pipe::new(Vec::new());
        request_update(&mut pipe, false, 800, 600)
            .await
            .expect("writes");
        assert_eq!(pipe.output[0], 3);
        assert_eq!(pipe.output[1], 0, "the first request is not incremental");

        let mut pipe = Pipe::new(Vec::new());
        request_update(&mut pipe, true, 800, 600)
            .await
            .expect("writes");
        assert_eq!(pipe.output[1], 1, "and every one after it is");
    }

    #[tokio::test]
    async fn a_raw_rectangle_arrives_as_damage() {
        let mut input = vec![0u8, 0];
        input.extend_from_slice(&1u16.to_be_bytes());
        // One 2x2 rectangle at 0,0, raw.
        input.extend_from_slice(&0u16.to_be_bytes());
        input.extend_from_slice(&0u16.to_be_bytes());
        input.extend_from_slice(&2u16.to_be_bytes());
        input.extend_from_slice(&2u16.to_be_bytes());
        input.extend_from_slice(&decode::RAW.to_be_bytes());
        input.extend_from_slice(&[0x40u8; 2 * 2 * 4]);

        let mut pipe = Pipe::new(input);
        let mut fb = Framebuffer::new(4, 4);
        let updates = read_message(&mut pipe, &mut fb).await.expect("an update");

        assert_eq!(
            updates,
            vec![Update::Damage(vec![Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 2
            }])]
        );
        assert_eq!(&fb.pixels()[..4], &[0x40; 4]);
    }

    #[tokio::test]
    async fn a_desktop_resize_is_reported_and_ends_the_damage_before_it() {
        // Everything drawn before the resize refers to a framebuffer that no longer exists, so the
        // two must not be reported as one batch.
        let mut input = vec![0u8, 0];
        input.extend_from_slice(&2u16.to_be_bytes());
        // A raw rectangle...
        input.extend_from_slice(&0u16.to_be_bytes());
        input.extend_from_slice(&0u16.to_be_bytes());
        input.extend_from_slice(&1u16.to_be_bytes());
        input.extend_from_slice(&1u16.to_be_bytes());
        input.extend_from_slice(&decode::RAW.to_be_bytes());
        input.extend_from_slice(&[0x11u8; 4]);
        // ...then a resize.
        input.extend_from_slice(&0u16.to_be_bytes());
        input.extend_from_slice(&0u16.to_be_bytes());
        input.extend_from_slice(&800u16.to_be_bytes());
        input.extend_from_slice(&600u16.to_be_bytes());
        input.extend_from_slice(&decode::DESKTOP_SIZE.to_be_bytes());

        let mut pipe = Pipe::new(input);
        let mut fb = Framebuffer::new(4, 4);
        let updates = read_message(&mut pipe, &mut fb).await.expect("updates");

        assert_eq!(updates.len(), 2);
        assert!(matches!(updates[0], Update::Damage(_)));
        assert_eq!(
            updates[1],
            Update::Resized {
                width: 800,
                height: 600
            }
        );
        assert_eq!((fb.width(), fb.height()), (800, 600));
    }

    #[tokio::test]
    async fn an_encoding_this_build_cannot_decode_is_named() {
        let mut input = vec![0u8, 0];
        input.extend_from_slice(&1u16.to_be_bytes());
        input.extend_from_slice(&[0u8; 8]);
        // 7 is Tight, which is real and not implemented.
        input.extend_from_slice(&7i32.to_be_bytes());

        let mut pipe = Pipe::new(input);
        let mut fb = Framebuffer::new(4, 4);
        let error = read_message(&mut pipe, &mut fb)
            .await
            .expect_err("an unknown encoding");
        assert!(error.to_string().contains('7'), "{error}");
    }

    #[tokio::test]
    async fn messages_that_change_no_pixels_are_read_past_rather_than_ignored() {
        // Skipping the bytes rather than the message: leaving them in the stream misaligns everything
        // after, which surfaces as a nonsense encoding several frames later.
        let mut input = vec![2u8]; // Bell.
        input.push(3); // ServerCutText.
        input.extend_from_slice(&[0u8; 3]);
        input.extend_from_slice(&5u32.to_be_bytes());
        input.extend_from_slice(b"hello");
        input.push(2); // And another bell, which proves the stream stayed in step.

        let mut pipe = Pipe::new(input);
        let mut fb = Framebuffer::new(4, 4);
        for _ in 0..3 {
            assert!(
                read_message(&mut pipe, &mut fb)
                    .await
                    .expect("read")
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn a_pointer_message_carries_the_whole_button_state() {
        // RFB has no press or release. A client that sends a press and forgets the release leaves a
        // button held down on the remote desktop.
        let mut pipe = Pipe::new(Vec::new());
        send_pointer(&mut pipe, 0b0000_0001, 640, 480)
            .await
            .expect("writes");
        assert_eq!(pipe.output, vec![5, 1, 2, 128, 1, 224]);
    }

    #[tokio::test]
    async fn a_key_message_carries_a_keysym_and_a_direction() {
        let mut pipe = Pipe::new(Vec::new());
        // 0xFF0D is Return.
        send_key(&mut pipe, 0xFF0D, true).await.expect("writes");
        assert_eq!(pipe.output, vec![4, 1, 0, 0, 0, 0, 0xFF, 0x0D]);
    }
}
