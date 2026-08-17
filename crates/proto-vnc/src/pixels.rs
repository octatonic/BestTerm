//! The pixel format, and turning a server's idea of a pixel into ours.
//!
//! RFB lets a server describe pixels almost any way it likes — depth, byte order, and where each
//! colour's bits sit inside the value — and then lets the client ask for something else instead.
//! Asking is what this does: one format is requested, every server supports it, and the decoders
//! downstream then have exactly one layout to handle.
//!
//! # The format asked for
//!
//! 32 bits per pixel, depth 24, little-endian, true colour, with red at bit 16, green at 8 and blue at
//! 0. In memory on a little-endian machine that is B, G, R, then a spare byte — which is
//! [`bestterm_surface::PixelFormat::Bgra8`] exactly, so a decoded rectangle is copied into the
//! framebuffer without touching it.
//!
//! Choosing the format the renderer already wants, rather than converting afterwards, is worth a
//! paragraph because the alternative costs a pass over every pixel of every frame.
//!
//! # The format the server offers anyway
//!
//! A server may ignore `SetPixelFormat`. It is not supposed to, and in practice they honour it, but
//! [`PixelFormat::parse`] exists so the server's own answer is read rather than assumed — and so a
//! server that sends something else is refused with a message rather than producing a picture in
//! wrong colours that looks like a decoder bug.

/// How pixels are laid out on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelFormat {
    /// Bits each pixel occupies: 8, 16 or 32.
    pub bits_per_pixel: u8,
    /// Bits that carry colour.
    pub depth: u8,
    /// Whether the most significant byte comes first.
    pub big_endian: bool,
    /// Whether the value is a colour rather than an index into a palette.
    pub true_colour: bool,
    /// Largest value of each channel.
    pub red_max: u16,
    /// As above.
    pub green_max: u16,
    /// As above.
    pub blue_max: u16,
    /// How far up the value the channel sits.
    pub red_shift: u8,
    /// As above.
    pub green_shift: u8,
    /// As above.
    pub blue_shift: u8,
}

/// Bytes a pixel format occupies on the wire.
pub const PIXEL_FORMAT_LEN: usize = 16;

impl PixelFormat {
    /// The one this client asks for. See the module documentation.
    pub const BGRA: Self = Self {
        bits_per_pixel: 32,
        depth: 24,
        big_endian: false,
        true_colour: true,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    };

    /// Read one off the wire.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; PIXEL_FORMAT_LEN] = bytes.get(..PIXEL_FORMAT_LEN)?.try_into().ok()?;
        Some(Self {
            bits_per_pixel: bytes[0],
            depth: bytes[1],
            big_endian: bytes[2] != 0,
            true_colour: bytes[3] != 0,
            red_max: u16::from_be_bytes([bytes[4], bytes[5]]),
            green_max: u16::from_be_bytes([bytes[6], bytes[7]]),
            blue_max: u16::from_be_bytes([bytes[8], bytes[9]]),
            red_shift: bytes[10],
            green_shift: bytes[11],
            blue_shift: bytes[12],
            // Bytes 13 to 15 are padding, and are not read into anything: a server that puts
            // something there is not saying anything the protocol defines.
        })
    }

    /// Write one for the wire.
    pub fn encode(self) -> [u8; PIXEL_FORMAT_LEN] {
        let mut out = [0u8; PIXEL_FORMAT_LEN];
        out[0] = self.bits_per_pixel;
        out[1] = self.depth;
        out[2] = u8::from(self.big_endian);
        out[3] = u8::from(self.true_colour);
        out[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        out[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        out[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        out[10] = self.red_shift;
        out[11] = self.green_shift;
        out[12] = self.blue_shift;
        out
    }

    /// Bytes one pixel occupies.
    pub fn bytes_per_pixel(self) -> usize {
        usize::from(self.bits_per_pixel).div_ceil(8)
    }

    /// Whether this is the layout the framebuffer wants, so a copy needs no conversion.
    ///
    /// Compared field by field rather than with `==` because the padding and the depth are allowed to
    /// differ without changing where a byte goes: what matters is the size, the order, and where each
    /// channel sits.
    pub fn is_bgra(self) -> bool {
        self.bits_per_pixel == 32
            && !self.big_endian
            && self.true_colour
            && self.red_max == 255
            && self.green_max == 255
            && self.blue_max == 255
            && self.red_shift == 16
            && self.green_shift == 8
            && self.blue_shift == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_requested_format_round_trips() {
        let encoded = PixelFormat::BGRA.encode();
        assert_eq!(PixelFormat::parse(&encoded), Some(PixelFormat::BGRA));
    }

    #[test]
    fn the_requested_format_is_what_the_framebuffer_already_wants() {
        // The whole reason for asking: a rectangle decoded in this layout is copied into the
        // framebuffer untouched, where anything else costs a pass over every pixel of every frame.
        assert!(PixelFormat::BGRA.is_bgra());
        assert_eq!(PixelFormat::BGRA.bytes_per_pixel(), 4);
        assert_eq!(
            bestterm_surface::PixelFormat::Bgra8.bytes_per_pixel(),
            u32::try_from(PixelFormat::BGRA.bytes_per_pixel()).expect("four fits")
        );
    }

    #[test]
    fn a_format_in_the_other_byte_order_is_not_ours() {
        // The failure this catches produces a picture in swapped colours, which reads as a decoder
        // bug rather than as a server that ignored the request.
        let mut other = PixelFormat::BGRA;
        other.big_endian = true;
        assert!(!other.is_bgra());

        let mut swapped = PixelFormat::BGRA;
        swapped.red_shift = 0;
        swapped.blue_shift = 16;
        assert!(!swapped.is_bgra());
    }

    #[test]
    fn a_palette_format_is_not_ours() {
        let mut indexed = PixelFormat::BGRA;
        indexed.true_colour = false;
        assert!(!indexed.is_bgra());
    }

    #[test]
    fn sixteen_bit_colour_is_recognised_as_not_ours_rather_than_misread() {
        // Real servers offer this by default, which is why the request matters and why the answer is
        // read rather than assumed.
        let rgb565 = PixelFormat {
            bits_per_pixel: 16,
            depth: 16,
            big_endian: false,
            true_colour: true,
            red_max: 31,
            green_max: 63,
            blue_max: 31,
            red_shift: 11,
            green_shift: 5,
            blue_shift: 0,
        };
        assert!(!rgb565.is_bgra());
        assert_eq!(rgb565.bytes_per_pixel(), 2);
    }

    #[test]
    fn a_truncated_format_is_refused_rather_than_padded() {
        assert_eq!(PixelFormat::parse(&[0u8; 15]), None);
        assert_eq!(PixelFormat::parse(&[]), None);
        // And extra bytes are ignored, because the caller reads a stream and the next message follows.
        assert!(PixelFormat::parse(&[0u8; 32]).is_some());
    }

    #[test]
    fn odd_bit_depths_round_up_to_whole_bytes() {
        let mut eight = PixelFormat::BGRA;
        eight.bits_per_pixel = 8;
        assert_eq!(eight.bytes_per_pixel(), 1);
    }
}
