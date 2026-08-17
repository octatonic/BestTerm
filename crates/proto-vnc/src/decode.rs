//! Turning rectangles into pixels.
//!
//! A framebuffer update is a list of rectangles, each with an encoding that says how its pixels were
//! packed. Three are implemented, which between them cover every server this is likely to meet:
//!
//! * **Raw** — the pixels, uncompressed. Mandatory: every RFB server supports it, so a session can
//!   always be had even if it is a slow one.
//! * **CopyRect** — "these pixels are already on screen, over there". A window drag or a scroll is
//!   almost entirely CopyRect, and it costs four bytes for an arbitrarily large region.
//! * **ZRLE** — zlib over run-length-encoded tiles, and the reason a VNC session is usable off a local
//!   network. Raw at 1080p is eight megabytes a frame.
//!
//! # The zlib stream is one stream for the whole connection
//!
//! This is the detail that makes ZRLE easy to get wrong. The compressor's dictionary carries from one
//! rectangle to the next and from one update to the next, so the decompressor is per-*connection*,
//! not per-rectangle. Creating a fresh one for each rectangle decodes the first correctly and then
//! produces rubbish, which looks like a corrupt image rather than like a protocol mistake.
//!
//! # Copying overlapping regions
//!
//! `CopyRect` may overlap its own destination — dragging a window a few pixels does exactly that —
//! so the rows are copied in an order that reads each one before it is overwritten. Copying top-down
//! when moving down smears the first row over the whole region, which is a distinctive and confusing
//! artefact.

use bestterm_surface::Rect;

/// Encodings this client asks for, in order of preference.
///
/// A server picks from this list, and the order is the preference: ZRLE where it can, CopyRect for
/// what has merely moved, Raw when there is nothing better. The pseudo-encoding for desktop size is
/// last because it is not an encoding at all — it is how a server says the desktop was resized.
pub const ENCODINGS: &[i32] = &[
    ZRLE,
    COPY_RECT,
    RAW,
    // Pseudo-encodings. Negative by convention, and not something a rectangle of pixels ever uses.
    DESKTOP_SIZE,
];

/// Uncompressed pixels.
pub const RAW: i32 = 0;
/// A region that is already on screen somewhere else.
pub const COPY_RECT: i32 = 1;
/// zlib over run-length-encoded tiles.
pub const ZRLE: i32 = 16;
/// "The desktop is now this size."
pub const DESKTOP_SIZE: i32 = -223;

/// Side of a ZRLE tile.
///
/// Fixed by the encoding, and the reason a wide rectangle is many tiles rather than one long run: a
/// 300-pixel row is five tiles, each with its own control byte.
const TILE: u32 = 64;

/// Where a tile is and how big it is.
///
/// A struct rather than four parameters threaded through three functions, which is both what clippy
/// asked for and what stops `width` and `height` being swapped at a call site.
#[derive(Clone, Copy, Debug)]
struct Tile {
    /// Left edge, in framebuffer coordinates.
    x: u32,
    /// Top edge, in framebuffer coordinates.
    y: u32,
    /// Width in pixels, at most [`TILE`].
    width: u32,
    /// Height in pixels, at most [`TILE`].
    height: u32,
}

impl Tile {
    /// How many pixels it holds.
    fn pixels(self) -> u32 {
        self.width * self.height
    }
}

/// Bytes a ZRLE pixel occupies on the wire.
///
/// Three, not four. ZRLE drops the byte that carries nothing when the format has 24 significant bits
/// in 32 — which ours does — and a decoder that reads four is off by a third of a tile immediately.
const CPIXEL: usize = 3;

/// What went wrong decoding.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The server used an encoding this build does not implement.
    #[error("the server used encoding {0}, which this build cannot decode")]
    UnknownEncoding(i32),

    /// A rectangle does not fit in the framebuffer.
    ///
    /// Refused rather than clamped: a rectangle outside the desktop means this end and the server
    /// disagree about how big it is, and drawing part of it would paint the disagreement onto the
    /// screen instead of reporting it.
    #[error(
        "a {width}x{height} rectangle at {x},{y} does not fit a {fb_width}x{fb_height} desktop"
    )]
    OutOfBounds {
        /// Where the rectangle was.
        x: u32,
        /// As above.
        y: u32,
        /// And how big.
        width: u32,
        /// As above.
        height: u32,
        /// The framebuffer's width.
        fb_width: u32,
        /// And height.
        fb_height: u32,
    },

    /// The compressed data ended early or would not inflate.
    #[error("zlib: {0}")]
    Zlib(String),

    /// A tile said something the encoding does not allow.
    #[error("{0}")]
    Malformed(String),
}

/// A framebuffer, and the zlib stream that has been running over it.
pub struct Framebuffer {
    /// Width in pixels.
    width: u32,
    /// Height in pixels.
    height: u32,
    /// BGRA, four bytes a pixel, no row padding.
    pixels: Vec<u8>,
    /// The connection's single zlib stream. See the module documentation.
    inflate: flate2::Decompress,
}

impl std::fmt::Debug for Framebuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Framebuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Framebuffer {
    /// A black desktop of the given size.
    pub fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        Self {
            width,
            height,
            pixels: vec![0u8; (width as usize) * (height as usize) * 4],
            inflate: flate2::Decompress::new(true),
        }
    }

    /// Change the size, discarding what was there.
    ///
    /// The zlib stream is *not* reset. A desktop resize does not restart the compressor at the far
    /// end, and resetting here would desynchronise the dictionary for every rectangle after it.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.pixels
            .resize((width as usize) * (height as usize) * 4, 0);
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The pixels, BGRA.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Bytes in a row.
    pub fn stride(&self) -> u32 {
        self.width * 4
    }

    /// Whether a rectangle is inside the desktop.
    fn check(&self, rect: Rect) -> Result<(), DecodeError> {
        let fits = rect
            .x
            .checked_add(rect.width)
            .is_some_and(|right| right <= self.width)
            && rect
                .y
                .checked_add(rect.height)
                .is_some_and(|bottom| bottom <= self.height);
        if fits {
            return Ok(());
        }
        Err(DecodeError::OutOfBounds {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            fb_width: self.width,
            fb_height: self.height,
        })
    }

    /// Write one pixel.
    fn put(&mut self, x: u32, y: u32, bgra: [u8; 4]) {
        let index = (y as usize * self.width as usize + x as usize) * 4;
        if let Some(slot) = self.pixels.get_mut(index..index + 4) {
            slot.copy_from_slice(&bgra);
        }
    }

    /// Apply a Raw rectangle: the pixels, in order, no compression.
    ///
    /// `data` is already in [`crate::pixels::PixelFormat::BGRA`], which is what was asked for, so the
    /// rows are copied rather than converted.
    pub fn apply_raw(&mut self, rect: Rect, data: &[u8]) -> Result<(), DecodeError> {
        self.check(rect)?;
        let row_bytes = rect.width as usize * 4;
        let needed = row_bytes * rect.height as usize;
        if data.len() < needed {
            return Err(DecodeError::Malformed(format!(
                "a raw rectangle needed {needed} bytes and {} arrived",
                data.len()
            )));
        }

        let stride = self.stride() as usize;
        for row in 0..rect.height as usize {
            let from = row * row_bytes;
            let to = ((rect.y as usize + row) * stride) + rect.x as usize * 4;
            self.pixels[to..to + row_bytes].copy_from_slice(&data[from..from + row_bytes]);
        }
        Ok(())
    }

    /// Apply a CopyRect: move a region that is already on screen.
    pub fn apply_copy(&mut self, rect: Rect, from_x: u32, from_y: u32) -> Result<(), DecodeError> {
        self.check(rect)?;
        self.check(Rect {
            x: from_x,
            y: from_y,
            width: rect.width,
            height: rect.height,
        })?;

        let stride = self.stride() as usize;
        let row_bytes = rect.width as usize * 4;

        // The order matters: the source and destination can overlap, which is exactly what dragging a
        // window does. Copying downwards while moving downwards smears the first row over the whole
        // region.
        let rows: Vec<u32> = if from_y < rect.y {
            (0..rect.height).rev().collect()
        } else {
            (0..rect.height).collect()
        };

        for row in rows {
            let from = ((from_y + row) as usize * stride) + from_x as usize * 4;
            let to = ((rect.y + row) as usize * stride) + rect.x as usize * 4;
            // `copy_within` rather than two slices, because the ranges may overlap within one row
            // when a region moves sideways.
            self.pixels.copy_within(from..from + row_bytes, to);
        }
        Ok(())
    }

    /// Apply a ZRLE rectangle.
    ///
    /// `data` is the compressed payload of this rectangle only; the zlib *stream* it belongs to spans
    /// the connection and lives in this framebuffer.
    pub fn apply_zrle(&mut self, rect: Rect, data: &[u8]) -> Result<(), DecodeError> {
        self.check(rect)?;

        let plain = self.inflate_all(data)?;
        let mut cursor = Cursor::new(&plain);

        // Tiles run left to right, top to bottom, and the ones at the right and bottom edges are
        // smaller than 64 where the rectangle does not divide evenly.
        let mut y = rect.y;
        while y < rect.y + rect.height {
            let tile_height = TILE.min(rect.y + rect.height - y);
            let mut x = rect.x;
            while x < rect.x + rect.width {
                let tile_width = TILE.min(rect.x + rect.width - x);
                self.apply_zrle_tile(
                    Tile {
                        x,
                        y,
                        width: tile_width,
                        height: tile_height,
                    },
                    &mut cursor,
                )?;
                x += tile_width;
            }
            y += tile_height;
        }
        Ok(())
    }

    /// Inflate a rectangle's payload through the connection's stream.
    fn inflate_all(&mut self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut out = Vec::with_capacity(data.len() * 4);
        let mut consumed = 0usize;
        let mut buffer = vec![0u8; 64 * 1024];

        loop {
            let before_in = self.inflate.total_in();
            let before_out = self.inflate.total_out();
            let status = self
                .inflate
                .decompress(
                    &data[consumed..],
                    &mut buffer,
                    flate2::FlushDecompress::None,
                )
                .map_err(|error| DecodeError::Zlib(error.to_string()))?;

            let read = (self.inflate.total_in() - before_in) as usize;
            let wrote = (self.inflate.total_out() - before_out) as usize;
            consumed += read;
            out.extend_from_slice(&buffer[..wrote]);

            match status {
                // The far end never ends the stream mid-connection; if it did, everything after is
                // undecodable and saying so beats producing rubbish.
                flate2::Status::StreamEnd => break,
                // No progress and nothing left to give it: this rectangle's payload is exhausted.
                _ if read == 0 && wrote == 0 => break,
                _ if consumed >= data.len() && wrote < buffer.len() => break,
                _ => {}
            }
        }
        Ok(out)
    }

    /// One ZRLE tile.
    fn apply_zrle_tile(&mut self, tile: Tile, cursor: &mut Cursor<'_>) -> Result<(), DecodeError> {
        let Tile {
            x,
            y,
            width,
            height,
        } = tile;
        let control = cursor.u8()?;
        let run_length_encoded = control & 0x80 != 0;
        let palette_size = control & 0x7F;

        match (run_length_encoded, palette_size) {
            // Raw pixels, one after another.
            (false, 0) => {
                for row in 0..height {
                    for column in 0..width {
                        let pixel = cursor.cpixel()?;
                        self.put(x + column, y + row, pixel);
                    }
                }
            }

            // One colour for the whole tile.
            (false, 1) => {
                let pixel = cursor.cpixel()?;
                for row in 0..height {
                    for column in 0..width {
                        self.put(x + column, y + row, pixel);
                    }
                }
            }

            // A palette, with indices packed as tightly as the palette allows.
            (false, size) => {
                let palette = cursor.palette(size)?;
                let bits = match size {
                    2 => 1,
                    3..=4 => 2,
                    5..=16 => 4,
                    other => {
                        return Err(DecodeError::Malformed(format!(
                            "a packed palette of {other} colours is not something ZRLE defines"
                        )));
                    }
                };
                for row in 0..height {
                    let mut bit = 0u32;
                    let mut byte = 0u8;
                    for column in 0..width {
                        if bit == 0 {
                            byte = cursor.u8()?;
                            bit = 8;
                        }
                        bit -= bits;
                        let index = usize::from((byte >> bit) & ((1 << bits) - 1));
                        let pixel = *palette.get(index).ok_or_else(|| {
                            DecodeError::Malformed(
                                "a palette index is past the palette".to_string(),
                            )
                        })?;
                        self.put(x + column, y + row, pixel);
                    }
                }
            }

            // Plain run-length encoding, no palette.
            (true, 0) => {
                let mut written = 0u32;
                while written < tile.pixels() {
                    let pixel = cursor.cpixel()?;
                    let run = cursor.run_length()?;
                    written = self.fill_run(tile, written, run, pixel)?;
                }
            }

            // Run-length encoding over a palette.
            (true, size) => {
                let palette = cursor.palette(size & 0x7F)?;
                let mut written = 0u32;
                while written < tile.pixels() {
                    let byte = cursor.u8()?;
                    let index = usize::from(byte & 0x7F);
                    let pixel = *palette.get(index).ok_or_else(|| {
                        DecodeError::Malformed("a palette index is past the palette".to_string())
                    })?;
                    // The high bit says a length follows; without it the run is one pixel.
                    let run = if byte & 0x80 != 0 {
                        cursor.run_length()?
                    } else {
                        1
                    };
                    written = self.fill_run(tile, written, run, pixel)?;
                }
            }
        }
        Ok(())
    }

    /// Write `run` pixels starting `written` into a tile, and return the new position.
    fn fill_run(
        &mut self,
        tile: Tile,
        written: u32,
        run: u32,
        pixel: [u8; 4],
    ) -> Result<u32, DecodeError> {
        // The length comes from the server, and a run that overruns its tile would write into the
        // next one -- which is a corrupt picture rather than an error, and therefore worth refusing.
        let end = written
            .checked_add(run)
            .ok_or_else(|| DecodeError::Malformed("a run length overflowed".to_string()))?;
        if end > tile.pixels() {
            return Err(DecodeError::Malformed(format!(
                "a run of {run} overruns a tile of {} pixels",
                tile.pixels()
            )));
        }
        for position in written..end {
            self.put(
                tile.x + position % tile.width,
                tile.y + position / tile.width,
                pixel,
            );
        }
        Ok(end)
    }
}

/// A reader over inflated ZRLE data.
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        let byte = *self
            .data
            .get(self.at)
            .ok_or_else(|| DecodeError::Malformed("a tile ended early".to_string()))?;
        self.at += 1;
        Ok(byte)
    }

    /// One compressed pixel: three bytes, not four. See [`CPIXEL`].
    fn cpixel(&mut self) -> Result<[u8; 4], DecodeError> {
        let bytes = self
            .data
            .get(self.at..self.at + CPIXEL)
            .ok_or_else(|| DecodeError::Malformed("a tile ended inside a pixel".to_string()))?;
        self.at += CPIXEL;
        // Blue, green, red on the wire in this format; the fourth byte is ours to set, and a desktop
        // is opaque.
        Ok([bytes[0], bytes[1], bytes[2], 0xFF])
    }

    fn palette(&mut self, size: u8) -> Result<Vec<[u8; 4]>, DecodeError> {
        let mut palette = Vec::with_capacity(usize::from(size));
        for _ in 0..size {
            palette.push(self.cpixel()?);
        }
        Ok(palette)
    }

    /// A run length: bytes of 255 accumulate, and the last one is the remainder, plus one.
    fn run_length(&mut self) -> Result<u32, DecodeError> {
        let mut length: u32 = 1;
        loop {
            let byte = self.u8()?;
            length = length
                .checked_add(u32::from(byte))
                .ok_or_else(|| DecodeError::Malformed("a run length overflowed".to_string()))?;
            if byte != 255 {
                return Ok(length);
            }
        }
    }
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

    fn pixel(fb: &Framebuffer, x: u32, y: u32) -> [u8; 4] {
        let index = (y as usize * fb.width as usize + x as usize) * 4;
        fb.pixels()[index..index + 4]
            .try_into()
            .expect("four bytes")
    }

    #[test]
    fn a_raw_rectangle_lands_where_it_was_put() {
        let mut fb = Framebuffer::new(4, 4);
        // A 2x2 red square at 1,1. Red is the third byte in BGRA.
        let data: Vec<u8> = std::iter::repeat_n([0u8, 0, 255, 255], 4)
            .flatten()
            .collect();
        fb.apply_raw(rect(1, 1, 2, 2), &data).expect("it fits");

        assert_eq!(pixel(&fb, 1, 1), [0, 0, 255, 255]);
        assert_eq!(pixel(&fb, 2, 2), [0, 0, 255, 255]);
        assert_eq!(pixel(&fb, 0, 0), [0, 0, 0, 0], "outside is untouched");
        assert_eq!(pixel(&fb, 3, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn a_rectangle_outside_the_desktop_is_refused_rather_than_clamped() {
        // It means this end and the server disagree about the size, and drawing part of it paints the
        // disagreement on the screen instead of reporting it.
        let mut fb = Framebuffer::new(4, 4);
        let data = vec![0u8; 4 * 4 * 4];
        assert!(matches!(
            fb.apply_raw(rect(3, 3, 4, 4), &data),
            Err(DecodeError::OutOfBounds { .. })
        ));
        assert!(matches!(
            fb.apply_raw(rect(0, 0, u32::MAX, 1), &data),
            Err(DecodeError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn a_short_raw_rectangle_is_refused_rather_than_padded() {
        let mut fb = Framebuffer::new(4, 4);
        assert!(matches!(
            fb.apply_raw(rect(0, 0, 2, 2), &[0u8; 3]),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn a_copy_downwards_over_itself_does_not_smear() {
        // The artefact this prevents is distinctive: copying top-down while moving down writes the
        // first row over the whole region, so a dragged window becomes one repeated line.
        let mut fb = Framebuffer::new(4, 4);
        // Row 0 white, everything else black.
        let white: Vec<u8> = std::iter::repeat_n([255u8; 4], 4).flatten().collect();
        fb.apply_raw(rect(0, 0, 4, 1), &white).expect("it fits");

        // Move rows 0..3 down by one, which overlaps.
        fb.apply_copy(rect(0, 1, 4, 3), 0, 0).expect("it fits");

        assert_eq!(pixel(&fb, 0, 1), [255; 4], "the white row moved down");
        assert_eq!(
            pixel(&fb, 0, 2),
            [0, 0, 0, 0],
            "and did not smear into the row below it"
        );
    }

    #[test]
    fn a_copy_upwards_over_itself_does_not_smear_either() {
        let mut fb = Framebuffer::new(4, 4);
        let white: Vec<u8> = std::iter::repeat_n([255u8; 4], 4).flatten().collect();
        fb.apply_raw(rect(0, 3, 4, 1), &white).expect("it fits");

        fb.apply_copy(rect(0, 0, 4, 3), 0, 1).expect("it fits");
        assert_eq!(pixel(&fb, 0, 2), [255; 4]);
        assert_eq!(pixel(&fb, 0, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn a_copy_from_outside_the_desktop_is_refused() {
        let mut fb = Framebuffer::new(4, 4);
        assert!(fb.apply_copy(rect(0, 0, 2, 2), 3, 3).is_err());
    }

    /// Compress `plain` the way a server's single zlib stream would.
    fn deflate(plain: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(plain).expect("in-memory write");
        encoder.finish().expect("in-memory finish")
    }

    #[test]
    fn a_zrle_tile_of_one_colour_fills_the_tile() {
        let mut fb = Framebuffer::new(4, 4);
        // Control byte 1: a palette of one, which is a solid tile. Then one three-byte pixel.
        let plain = vec![1u8, 0x10, 0x20, 0x30];
        fb.apply_zrle(rect(0, 0, 4, 4), &deflate(&plain))
            .expect("decodes");

        assert_eq!(pixel(&fb, 0, 0), [0x10, 0x20, 0x30, 0xFF]);
        assert_eq!(pixel(&fb, 3, 3), [0x10, 0x20, 0x30, 0xFF]);
    }

    #[test]
    fn a_zrle_pixel_is_three_bytes_and_not_four() {
        // The mistake this catches puts the decoder a third of a tile out of step immediately, and
        // the picture that comes out looks like static rather than like a bug.
        let mut fb = Framebuffer::new(2, 1);
        // Control 0: raw pixels. Two pixels, three bytes each.
        let plain = vec![0u8, 1, 2, 3, 4, 5, 6];
        fb.apply_zrle(rect(0, 0, 2, 1), &deflate(&plain))
            .expect("decodes");

        assert_eq!(pixel(&fb, 0, 0), [1, 2, 3, 0xFF]);
        assert_eq!(pixel(&fb, 1, 0), [4, 5, 6, 0xFF]);
    }

    #[test]
    fn a_run_length_accumulates_across_bytes_of_255() {
        // 255 means "and more". A decoder that reads one byte as the whole length stops after 256
        // pixels of a long run and leaves the rest of the tile black.
        //
        // Inside one tile, deliberately: 64x8 is 512 pixels, which needs a run longer than 255 to
        // reach. The first version of this test used a 300x1 rectangle and failed -- correctly, because
        // ZRLE tiles at 64 and a 300-wide rectangle is five tiles each with its own control byte. The
        // decoder was right and the test was wrong.
        let mut fb = Framebuffer::new(64, 8);
        let mut plain = vec![0x80u8, 9, 8, 7];
        // 300 pixels: 299 is 255 + 44.
        plain.extend_from_slice(&[255, 44]);
        // Then 212 more to fill the tile: 211 fits in one byte.
        plain.extend_from_slice(&[1, 2, 3, 211]);
        fb.apply_zrle(rect(0, 0, 64, 8), &deflate(&plain))
            .expect("decodes");

        assert_eq!(pixel(&fb, 0, 0), [9, 8, 7, 0xFF]);
        // Pixel 299 is row 4, column 43.
        assert_eq!(pixel(&fb, 43, 4), [9, 8, 7, 0xFF]);
        assert_eq!(
            pixel(&fb, 44, 4),
            [1, 2, 3, 0xFF],
            "the second run starts here"
        );
        assert_eq!(pixel(&fb, 63, 7), [1, 2, 3, 0xFF], "and reaches the end");
    }

    #[test]
    fn a_wide_rectangle_is_many_tiles_each_with_its_own_control_byte() {
        // The property the test above got wrong. 130 pixels across is three tiles: 64, 64 and 2.
        let mut fb = Framebuffer::new(130, 1);
        let mut plain = Vec::new();
        for colour in [10u8, 20, 30] {
            // A solid tile: palette of one, then its colour.
            plain.extend_from_slice(&[1, colour, colour, colour]);
        }
        fb.apply_zrle(rect(0, 0, 130, 1), &deflate(&plain))
            .expect("decodes");

        assert_eq!(pixel(&fb, 0, 0), [10, 10, 10, 0xFF]);
        assert_eq!(pixel(&fb, 64, 0), [20, 20, 20, 0xFF]);
        assert_eq!(pixel(&fb, 128, 0), [30, 30, 30, 0xFF]);
    }

    #[test]
    fn a_run_that_overruns_its_tile_is_refused() {
        // The length is the server's to choose, so it is the server's to get wrong -- and a run that
        // spills writes into the next tile, which is a corrupt picture rather than an error.
        let mut fb = Framebuffer::new(4, 1);
        let plain = vec![0x80u8, 1, 2, 3, 200];
        assert!(matches!(
            fb.apply_zrle(rect(0, 0, 4, 1), &deflate(&plain)),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn a_packed_palette_unpacks_at_the_right_width() {
        let mut fb = Framebuffer::new(4, 1);
        // A palette of two: one bit per index. Indices 0,1,1,0 pack into 0b0110_0000.
        let plain = vec![2u8, 0, 0, 0, 255, 255, 255, 0b0110_0000];
        fb.apply_zrle(rect(0, 0, 4, 1), &deflate(&plain))
            .expect("decodes");

        assert_eq!(pixel(&fb, 0, 0), [0, 0, 0, 0xFF]);
        assert_eq!(pixel(&fb, 1, 0), [255, 255, 255, 0xFF]);
        assert_eq!(pixel(&fb, 2, 0), [255, 255, 255, 0xFF]);
        assert_eq!(pixel(&fb, 3, 0), [0, 0, 0, 0xFF]);
    }

    #[test]
    fn a_palette_index_past_the_palette_is_refused() {
        let mut fb = Framebuffer::new(4, 1);
        // A run-length palette of one, with an index of 3 in it.
        let plain = vec![0x81u8, 0, 0, 0, 3];
        assert!(matches!(
            fb.apply_zrle(rect(0, 0, 4, 1), &deflate(&plain)),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn the_zlib_stream_carries_between_rectangles() {
        // The detail that makes ZRLE easy to get wrong: the compressor's dictionary spans the
        // connection, so a decompressor made fresh for each rectangle decodes the first and produces
        // rubbish for the second.
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&[1u8, 0x11, 0x22, 0x33]).expect("write");
        encoder.flush().expect("flush");
        let first = std::mem::take(encoder.get_mut());
        encoder.write_all(&[1u8, 0x44, 0x55, 0x66]).expect("write");
        encoder.flush().expect("flush");
        let second = std::mem::take(encoder.get_mut());

        let mut fb = Framebuffer::new(2, 2);
        fb.apply_zrle(rect(0, 0, 1, 1), &first).expect("first tile");
        assert_eq!(pixel(&fb, 0, 0), [0x11, 0x22, 0x33, 0xFF]);

        fb.apply_zrle(rect(1, 1, 1, 1), &second)
            .expect("the second tile needs the first's dictionary");
        assert_eq!(pixel(&fb, 1, 1), [0x44, 0x55, 0x66, 0xFF]);
    }

    #[test]
    fn a_resize_keeps_the_zlib_stream() {
        // A desktop resize does not restart the compressor at the far end, so resetting here would
        // desynchronise every rectangle after it.
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&[1u8, 1, 2, 3]).expect("write");
        encoder.flush().expect("flush");
        let first = std::mem::take(encoder.get_mut());
        encoder.write_all(&[1u8, 4, 5, 6]).expect("write");
        encoder.flush().expect("flush");
        let second = std::mem::take(encoder.get_mut());

        let mut fb = Framebuffer::new(2, 2);
        fb.apply_zrle(rect(0, 0, 1, 1), &first).expect("first");
        fb.resize(8, 8);
        fb.apply_zrle(rect(0, 0, 1, 1), &second)
            .expect("the stream survived the resize");
        assert_eq!(pixel(&fb, 0, 0), [4, 5, 6, 0xFF]);
    }

    #[test]
    fn the_encoding_list_prefers_the_cheap_ones_and_includes_the_mandatory_one() {
        // Raw is mandatory, so a session can always be had; ZRLE first because Raw at 1080p is eight
        // megabytes a frame.
        assert_eq!(ENCODINGS[0], ZRLE);
        assert!(ENCODINGS.contains(&RAW));
        assert!(ENCODINGS.contains(&COPY_RECT));
        assert!(
            ENCODINGS.iter().position(|e| *e == ZRLE) < ENCODINGS.iter().position(|e| *e == RAW),
            "the cheap encoding has to be preferred"
        );
    }

    #[test]
    fn a_framebuffer_is_never_zero_sized() {
        // A server that says the desktop is 0x0 would otherwise produce an empty buffer and an
        // out-of-bounds on the first rectangle.
        let fb = Framebuffer::new(0, 0);
        assert_eq!((fb.width(), fb.height()), (1, 1));
        assert_eq!(fb.pixels().len(), 4);
        assert_eq!(fb.stride(), 4);
    }
}
