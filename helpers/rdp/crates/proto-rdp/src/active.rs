//! The active stage: everything after the handshake, which is to say the session itself.
//!
//! [`crate::session::connect`] ends with a [`ConnectionResult`]. From there on the shape is a loop:
//! read one PDU, hand it to IronRDP, and act on whatever it hands back — usually "these pixels
//! changed", occasionally "answer this", rarely "start over".
//!
//! # What must be written back
//!
//! [`ActiveStageOutput::ResponseFrame`] is not optional. On the fast path it carries the Frame
//! Acknowledge PDU, which is how RDP does flow control: a server that stops seeing acknowledgements
//! stops sending frames, and the session appears to freeze with no error anywhere. It is also how a
//! graceful shutdown completes — the disconnect ultimatum arrives as a `ResponseFrame` *ahead of*
//! the `Terminate` in the same batch, so the writing has to happen before the loop ends.
//!
//! Most batches carry an empty one: the fast-path branch pushes a `ResponseFrame` unconditionally,
//! whether or not anything was encoded into it.
//!
//! # Damage
//!
//! IronRDP reports damage as [`InclusiveRectangle`], where the pixel at `(right, bottom)` is part of
//! the rectangle. [`bestterm_surface::Rect`] is a position and a size. Converting is `right - left +
//! 1`, and getting it wrong leaves a one-pixel seam down the right and bottom of every update —
//! the kind of bug that looks like a rendering artefact and is arithmetic.
//!
//! Damage is kept as a *list* rather than a running union, for a specific reason.
//! `InclusiveRectangle::empty()` is `{0, 0, 0, 0}`, which under inclusive arithmetic is one pixel at
//! the origin rather than nothing at all, and IronRDP returns it as a "nothing happened" sentinel
//! from a dozen places. Unioned into a running rectangle it would drag the damage back to (0, 0)
//! every time, turning nearly every partial update into a full-frame upload. In a list it stays a
//! single pixel and costs nothing. The list is capped, and past the cap it does collapse to a union —
//! by then the frame is mostly dirty anyway.
//!
//! # Rectangles that do not fit
//!
//! Damage rectangles are not guaranteed to lie inside the framebuffer. Two paths in IronRDP return a
//! server-supplied rectangle without a bounds check — the RemoteFX decoder seeds its result with the
//! clipping extents before any tile is applied, and the bitmap path returns the update rectangle
//! verbatim when it cannot decode one. A stale rectangle can also arrive after a resize and before
//! the reactivation that follows it. Every rectangle is clamped here, because the alternative is
//! slicing out of bounds in whoever uploads the frame.

use bestterm_surface::{CursorShape, FrameSize, PixelFormat, Rect};
use ironrdp_connector::connection_activation::{
    ConnectionActivationFactory, ConnectionActivationSequence, ConnectionActivationState,
};
use ironrdp_connector::{ConnectionResult, Sequence};
use ironrdp_displaycontrol::pdu::MonitorLayoutEntry;
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput, fast_path};
use ironrdp_tokio::{FramedWrite, single_sequence_step};

use crate::session::{Connected, RdpError, SessionStream};

/// Byte order the framebuffer is decoded into.
///
/// `BgrA32` and not `BgrX32`, though RDP's own 32-bit bitmaps are the latter. IronRDP takes a
/// four-byte memcpy path when the source format equals the image format, copying the fourth byte
/// verbatim — so an `X` framebuffer inherits whatever the server left in that byte, which is
/// routinely zero. A fully transparent desktop is a hard bug to look at.
const PIXEL_FORMAT: ironrdp_graphics::image_processing::PixelFormat =
    ironrdp_graphics::image_processing::PixelFormat::BgrA32;

/// How many separate damage rectangles are reported before they collapse into one.
///
/// A number rather than no limit: each rectangle costs a separate upload on the far side, and past a
/// few dozen the bookkeeping outweighs the pixels saved.
const MAX_DAMAGE_RECTS: usize = 48;

/// Something the session produced.
///
/// Deliberately in [`bestterm_surface`]'s vocabulary and not IronRDP's: this is what crosses the
/// process boundary, and the host has no IronRDP in its dependency graph to describe it with.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// Pixels changed. Read them with [`ActiveSession::frame`].
    Frame {
        /// The regions that changed. Never empty when this variant is produced.
        damage: Vec<Rect>,
    },
    /// The desktop changed size and the framebuffer was rebuilt.
    ///
    /// Everything previously read is stale, including any damage not yet acted on.
    Resized(FrameSize),
    /// The pointer should look different.
    Cursor(CursorShape),
    /// The session ended, and ended cleanly.
    Closed {
        /// What the protocol said about why.
        reason: Option<String>,
    },
}

/// One protocol data unit, read but not yet acted on.
///
/// Opaque on purpose: the pair of an action and its bytes means something only to IronRDP, and the
/// point of naming it is to make the gap between reading and processing something a caller can hold
/// rather than something it has to reconstruct.
#[derive(Debug)]
pub struct Pdu {
    /// Which framing the payload uses.
    action: ironrdp_pdu::Action,
    /// The complete PDU, header included.
    payload: ironrdp_tokio::bytes::BytesMut,
}

/// A framebuffer, described.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameLayout {
    /// Dimensions.
    pub size: FrameSize,
    /// Bytes per row. IronRDP pads nothing, so this is always `width * 4`.
    pub stride: u32,
    /// Byte order.
    pub format: PixelFormat,
}

/// A connected session, pumped one PDU at a time.
pub struct ActiveSession {
    /// IronRDP's protocol state machine.
    stage: ActiveStage,
    /// The framebuffer it decodes into.
    image: DecodedImage,
    /// The TLS stream, framed.
    stream: SessionStream,
    /// Builds a fresh activation sequence when the server asks us to start over.
    activation: ConnectionActivationFactory,
    /// Needed to rebuild the fast-path processor after a reactivation.
    io_channel_id: u16,
    /// Likewise.
    user_channel_id: u16,
    /// Whether the server draws its own pointer into the framebuffer.
    pointer_software_rendering: bool,
    /// A label for logs and for the tab.
    label: String,
}

impl std::fmt::Debug for ActiveSession {
    /// By hand: none of the fields has a `Debug` worth printing, and the size is the thing anyone
    /// reading a log actually wants.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveSession")
            .field("label", &self.label)
            .field("width", &self.image.width())
            .field("height", &self.image.height())
            .finish_non_exhaustive()
    }
}

impl ActiveSession {
    /// Take over a connection that has finished its handshake.
    ///
    /// The [`ConnectionResult`] is taken apart field by field rather than kept: seven of its fields
    /// belong to the protocol state machine, one describes the framebuffer, and one is only needed
    /// if the server asks us to start over. Splitting them here means nothing holds a value it does
    /// not use.
    pub fn new(connected: Connected, label: String) -> Self {
        let Connected { result, stream, .. } = connected;
        let ConnectionResult {
            io_channel_id,
            user_channel_id,
            message_channel_id,
            share_id,
            static_channels,
            desktop_size,
            enable_server_pointer,
            pointer_software_rendering,
            activation_factory,
            compression_type,
        } = result;

        let image = DecodedImage::new(PIXEL_FORMAT, desktop_size.width, desktop_size.height);
        let stage = ActiveStageBuilder {
            static_channels,
            user_channel_id,
            io_channel_id,
            message_channel_id,
            share_id,
            compression_type,
            enable_server_pointer,
            pointer_software_rendering,
        }
        .build();

        Self {
            stage,
            image,
            stream,
            activation: activation_factory,
            io_channel_id,
            user_channel_id,
            pointer_software_rendering,
            label,
        }
    }

    /// How the framebuffer is laid out right now.
    pub fn layout(&self) -> FrameLayout {
        FrameLayout {
            size: FrameSize::new(
                u32::from(self.image.width()),
                u32::from(self.image.height()),
            ),
            stride: u32::try_from(self.image.stride()).unwrap_or(u32::MAX),
            format: PixelFormat::Bgra8,
        }
    }

    /// The decoded pixels.
    pub fn frame(&self) -> &[u8] {
        self.image.data()
    }

    /// What this session is called.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Wait for one PDU from the server.
    ///
    /// Cancel-safe: dropping this future loses nothing, because IronRDP's own reader only removes
    /// bytes from its buffer once a whole PDU is there. That is what makes it usable as a branch of
    /// a `select!` against a control channel — which is the whole reason reading and processing are
    /// two calls rather than one. See [`ActiveSession::pump`].
    pub async fn read(&mut self) -> Result<Pdu, RdpError> {
        let (action, payload) = self.stream.read_pdu().await?;
        Ok(Pdu { action, payload })
    }

    /// Read one PDU and act on it.
    ///
    /// Returns everything that one PDU produced, which is usually nothing at all: RDP sends frame
    /// markers and flow-control traffic between the PDUs that carry pixels, and those produce an
    /// acknowledgement and an empty result. A caller loops on this until it sees
    /// [`Update::Closed`].
    ///
    /// **Not cancel-safe.** Dropping this future part-way through discards a PDU that has already
    /// been taken off the stream, and with it whatever pixels it carried and whatever
    /// acknowledgement it owed the server. A caller that needs to wait on something else at the same
    /// time must use [`ActiveSession::read`] in the `select!` and call
    /// [`ActiveSession::process`] on the result afterwards, outside it.
    pub async fn pump(&mut self) -> Result<Vec<Update>, RdpError> {
        let pdu = self.read().await?;
        self.process(pdu).await
    }

    /// Act on a PDU that [`ActiveSession::read`] returned.
    ///
    /// Must not be abandoned once started, for the reason given on [`ActiveSession::pump`].
    pub async fn process(&mut self, pdu: Pdu) -> Result<Vec<Update>, RdpError> {
        let Pdu { action, payload } = pdu;
        let outputs = self
            .stage
            .process(&mut self.image, action, &payload)
            .map_err(session_error)?;

        let mut updates = Vec::new();
        let mut damage: Vec<Rect> = Vec::new();
        let mut reactivate = false;

        for output in outputs {
            match output {
                // Flow control. Empty ones are the common case and are not worth a syscall.
                ActiveStageOutput::ResponseFrame(frame) => {
                    if !frame.is_empty() {
                        self.stream.write_all(&frame).await?;
                    }
                }

                ActiveStageOutput::GraphicsUpdate(rect) => {
                    if let Some(rect) = self.clamp(&rect) {
                        push_damage(&mut damage, rect);
                    }
                }

                ActiveStageOutput::PointerDefault => {
                    updates.push(Update::Cursor(CursorShape::Default));
                }
                ActiveStageOutput::PointerHidden => {
                    updates.push(Update::Cursor(CursorShape::Hidden));
                }
                // The remote pointer's exact shape and position are not carried across the boundary
                // yet: the surface protocol describes a cursor by kind, and warping the local
                // pointer to a server-chosen position is a decision the interface should make, not
                // this loop. Logged rather than dropped silently so the gap is visible.
                ActiveStageOutput::PointerBitmap(_) | ActiveStageOutput::PointerPosition { .. } => {
                    tracing::trace!("rdp: ignoring a server pointer update");
                }

                ActiveStageOutput::Terminate(reason) => {
                    updates.push(Update::Closed {
                        reason: Some(reason.description()),
                    });
                }

                ActiveStageOutput::DeactivateAll => reactivate = true,

                // Auto-detect RTT probes are answered inside IronRDP as a `ResponseFrame`; what
                // reaches here is the informational result. Multitransport asks us to open a
                // side-channel we do not implement, and declining is simply not answering.
                ActiveStageOutput::AutoDetect(_) | ActiveStageOutput::MultitransportRequest(_) => {}
            }
        }

        if !damage.is_empty() {
            updates.push(Update::Frame { damage });
        }

        // Last, and after the damage: a reactivation replaces the framebuffer, so anything reported
        // above refers to the old one and must be consumed before this runs.
        if reactivate {
            let size = self.reactivate().await?;
            updates.push(Update::Resized(size));
        }

        Ok(updates)
    }

    /// Ask the server for a different desktop size.
    ///
    /// Advisory in the strongest sense. It needs the display-control channel, which not every server
    /// opens; it is clamped to what the protocol can express; and the size that comes back may not be
    /// the size asked for. Nothing here reports the new size, because nothing here knows it — it
    /// arrives with the [`Update::Resized`] that follows.
    pub async fn request_resize(&mut self, size: FrameSize) -> Result<(), RdpError> {
        let (width, height) = MonitorLayoutEntry::adjust_display_size(size.width, size.height);
        match self.stage.encode_resize(width, height, None, None) {
            Some(Ok(bytes)) => {
                self.stream.write_all(&bytes).await?;
                tracing::debug!(width, height, "rdp: asked for a new desktop size");
                Ok(())
            }
            Some(Err(err)) => Err(session_error(err)),
            None => {
                // Not an error: plenty of servers never open the channel, and a resize that cannot
                // be requested is a letterbox, not a failure.
                tracing::debug!("rdp: the server has no display control channel; keeping the size");
                Ok(())
            }
        }
    }

    /// Run the deactivation-reactivation sequence and rebuild everything that depends on its result.
    ///
    /// The server sends `DeactivateAll` when it wants capability exchange to happen again — after a
    /// resize, and unprompted when something changes on its side. What comes back is authoritative:
    /// the negotiated desktop size may differ from anything that was asked for.
    async fn reactivate(&mut self) -> Result<FrameSize, RdpError> {
        tracing::debug!("rdp: the server asked to reactivate");

        let mut sequence: ConnectionActivationSequence = self.activation.create();
        let mut buf = ironrdp_pdu::WriteBuf::new();
        while !sequence.state().is_terminal() {
            single_sequence_step(&mut self.stream, &mut sequence, &mut buf)
                .await
                .map_err(|err| RdpError::Protocol(err.to_string()))?;
        }

        let ConnectionActivationState::Finalized {
            desktop_size,
            share_id,
            enable_server_pointer,
            pointer_software_rendering,
        } = sequence.connection_activation_state()
        else {
            return Err(RdpError::Protocol(
                "the reactivation sequence ended without a result".to_string(),
            ));
        };

        // The share id identifies the session to the server, and frame acknowledgements carry it. A
        // stale one is acknowledged into the void, which stalls the session exactly as dropping the
        // acknowledgement would.
        self.stage.set_share_id(share_id);
        self.stage.set_enable_server_pointer(enable_server_pointer);
        let processor = fast_path::ProcessorBuilder {
            io_channel_id: self.io_channel_id,
            user_channel_id: self.user_channel_id,
            share_id,
            enable_server_pointer,
            pointer_software_rendering,
            // Rebuilding loses bulk decompression: `Processor` hands out no way to recover the
            // decompressor it holds. Sessions negotiated without compression are unaffected, and a
            // session that had it will renegotiate on the capability exchange that just ran.
            bulk_decompressor: None,
        }
        .build();
        self.stage.set_fastpath_processor(processor);
        self.pointer_software_rendering = pointer_software_rendering;

        self.image = DecodedImage::new(PIXEL_FORMAT, desktop_size.width, desktop_size.height);

        let size = FrameSize::new(
            u32::from(desktop_size.width),
            u32::from(desktop_size.height),
        );
        tracing::info!(width = size.width, height = size.height, "rdp: reactivated");
        Ok(size)
    }

    /// Convert one of IronRDP's rectangles into ours, discarding what falls outside the framebuffer.
    ///
    /// Returns `None` for a rectangle with nothing left inside, rather than an empty one: an empty
    /// rectangle in the damage list would be an upload of nothing.
    fn clamp(&self, rect: &InclusiveRectangle) -> Option<Rect> {
        let (width, height) = (self.image.width(), self.image.height());
        if width == 0 || height == 0 {
            return None;
        }
        let right = rect.right.min(width - 1);
        let bottom = rect.bottom.min(height - 1);
        if rect.left > right || rect.top > bottom {
            return None;
        }
        Some(Rect {
            x: u32::from(rect.left),
            y: u32::from(rect.top),
            // Inclusive on the far edge, hence the +1. Both are non-zero by the check above.
            width: u32::from(right - rect.left) + 1,
            height: u32::from(bottom - rect.top) + 1,
        })
    }
}

/// Add `rect` to the damage list, collapsing the list if it has grown too long.
///
/// See the module documentation for why this is a list and not a running union.
fn push_damage(damage: &mut Vec<Rect>, rect: Rect) {
    if damage.len() < MAX_DAMAGE_RECTS {
        damage.push(rect);
        return;
    }
    let merged = damage.drain(..).fold(rect, union);
    damage.push(merged);
}

/// The smallest rectangle containing both.
fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

/// Flatten a session error, for the same reason [`crate::session`] flattens a connector error: its
/// variants name stages of a state machine, and its `Display` already says the useful part.
fn session_error(error: ironrdp_session::SessionError) -> RdpError {
    RdpError::Protocol(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_union_covers_both() {
        assert_eq!(
            union(rect(0, 0, 2, 2), rect(8, 8, 2, 2)),
            rect(0, 0, 10, 10)
        );
        assert_eq!(union(rect(4, 4, 2, 2), rect(4, 4, 2, 2)), rect(4, 4, 2, 2));
        // Containment, in both directions.
        assert_eq!(union(rect(0, 0, 9, 9), rect(3, 3, 1, 1)), rect(0, 0, 9, 9));
        assert_eq!(union(rect(3, 3, 1, 1), rect(0, 0, 9, 9)), rect(0, 0, 9, 9));
    }

    #[test]
    fn damage_stays_a_list_until_the_cap() {
        // The reason the list exists: IronRDP's "nothing happened" sentinel is a single pixel at the
        // origin, and unioning it into a running rectangle would drag every update back to (0, 0)
        // and turn a small change into a full-frame upload.
        let mut damage = Vec::new();
        push_damage(&mut damage, rect(0, 0, 1, 1));
        push_damage(&mut damage, rect(900, 700, 20, 20));

        assert_eq!(damage.len(), 2, "the sentinel must not swallow the update");
        assert_eq!(damage[1], rect(900, 700, 20, 20));
    }

    #[test]
    fn damage_collapses_once_it_is_long_enough_not_to_be_worth_it() {
        let mut damage = Vec::new();
        for i in 0..MAX_DAMAGE_RECTS {
            let i = u32::try_from(i).expect("the cap fits in a u32");
            push_damage(&mut damage, rect(i, i, 1, 1));
        }
        assert_eq!(damage.len(), MAX_DAMAGE_RECTS);

        push_damage(&mut damage, rect(500, 500, 4, 4));
        assert_eq!(damage.len(), 1, "past the cap it becomes one rectangle");
        assert_eq!(damage[0], rect(0, 0, 504, 504));
    }

    /// The clamping is pure arithmetic over a size, so it is tested without a session.
    fn clamp_to(width: u16, height: u16, rect: &InclusiveRectangle) -> Option<Rect> {
        if width == 0 || height == 0 {
            return None;
        }
        let right = rect.right.min(width - 1);
        let bottom = rect.bottom.min(height - 1);
        if rect.left > right || rect.top > bottom {
            return None;
        }
        Some(Rect {
            x: u32::from(rect.left),
            y: u32::from(rect.top),
            width: u32::from(right - rect.left) + 1,
            height: u32::from(bottom - rect.top) + 1,
        })
    }

    fn inclusive(left: u16, top: u16, right: u16, bottom: u16) -> InclusiveRectangle {
        InclusiveRectangle {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn inclusive_bounds_become_a_size_without_losing_the_last_row() {
        // 0..=0 is one pixel, not none. Getting this wrong leaves a seam down the right and bottom
        // edge of every update.
        assert_eq!(
            clamp_to(64, 64, &inclusive(0, 0, 0, 0)),
            Some(rect(0, 0, 1, 1))
        );
        assert_eq!(
            clamp_to(64, 64, &inclusive(10, 20, 19, 29)),
            Some(rect(10, 20, 10, 10))
        );
        // The far corner of the framebuffer is inside it.
        assert_eq!(
            clamp_to(64, 48, &inclusive(63, 47, 63, 47)),
            Some(rect(63, 47, 1, 1))
        );
    }

    #[test]
    fn a_rectangle_larger_than_the_framebuffer_is_cut_down_to_it() {
        // Not hypothetical: the RemoteFX decoder seeds its result with the clipping extents before
        // any bounds check, and a stale rectangle can arrive after a resize. Slicing on one of these
        // would read past the end of the framebuffer.
        assert_eq!(
            clamp_to(64, 48, &inclusive(0, 0, 999, 999)),
            Some(rect(0, 0, 64, 48))
        );
        assert_eq!(
            clamp_to(64, 48, &inclusive(60, 40, 100, 100)),
            Some(rect(60, 40, 4, 8))
        );
    }

    #[test]
    fn a_rectangle_wholly_outside_the_framebuffer_is_dropped() {
        assert_eq!(clamp_to(64, 48, &inclusive(64, 0, 70, 10)), None);
        assert_eq!(clamp_to(64, 48, &inclusive(0, 48, 10, 60)), None);
        // And a framebuffer with no pixels has nothing to damage.
        assert_eq!(clamp_to(0, 0, &inclusive(0, 0, 0, 0)), None);
    }

    #[test]
    fn the_framebuffer_is_described_with_an_alpha_channel() {
        // `BgrX32` would let the server's fourth byte through untouched, which is routinely zero,
        // and a fully transparent desktop is a hard bug to look at.
        assert!(
            PIXEL_FORMAT.has_alpha(),
            "the framebuffer format must carry alpha"
        );
        assert_eq!(PIXEL_FORMAT.bytes_per_pixel(), 4);
        assert_eq!(PixelFormat::Bgra8.bytes_per_pixel(), 4);
    }
}
