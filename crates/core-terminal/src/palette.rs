//! Colour definitions and the default 256-colour table.
//!
//! This module is deliberately free of `alacritty_terminal` types. Colour *resolution* — turning a
//! VT colour reference into concrete RGB, honouring OSC overrides and dim/bright attributes — is
//! terminal semantics and lives next door in the emulator module. What lives here is the table it
//! resolves against, and it is testable on its own.

/// An 8-bit-per-channel colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgb {
    /// A colour from its channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// The same colour at two-thirds intensity, for the SGR 2 (dim) attribute.
    ///
    /// Two-thirds is what xterm and most emulators use; it is dark enough to read as dim without
    /// disappearing on a light background.
    pub const fn dimmed(self) -> Self {
        Self {
            r: (self.r as u16 * 2 / 3) as u8,
            g: (self.g as u16 * 2 / 3) as u8,
            b: (self.b as u16 * 2 / 3) as u8,
        }
    }

    /// Pack into `0xRRGGBB`, for hashing into a glyph cache key.
    pub const fn as_u32(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}

/// The colours a terminal draws with.
///
/// The defaults are xterm's, which is the right baseline: it is what `TERM=xterm-256color`
/// advertises, and every colour scheme in the world is expressed as a deviation from it.
///
/// BestTerm's shipped default will match the reference application's palette instead — that is a
/// `MEASURE` row in `docs/ui-parity.md` and is not guessed here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The 256-colour table: 0–15 ANSI, 16–231 the 6×6×6 cube, 232–255 the grey ramp.
    pub indexed: Box<[Rgb; 256]>,
    /// Default text colour.
    pub foreground: Rgb,
    /// Default background colour.
    pub background: Rgb,
    /// Cursor colour.
    pub cursor: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        Self::xterm()
    }
}

impl Palette {
    /// The xterm defaults.
    pub fn xterm() -> Self {
        Self {
            indexed: Box::new(build_indexed()),
            foreground: Rgb::new(0xD0, 0xD0, 0xD0),
            background: Rgb::new(0x0C, 0x0C, 0x0C),
            cursor: Rgb::new(0xD0, 0xD0, 0xD0),
        }
    }

    /// Look up an entry in the 256-colour table.
    ///
    /// Out-of-range indices resolve to the foreground rather than panicking: a malformed escape
    /// sequence from a remote host must not be able to crash the terminal.
    pub fn indexed(&self, index: usize) -> Rgb {
        self.indexed.get(index).copied().unwrap_or(self.foreground)
    }
}

/// The 16 standard ANSI colours, as xterm defines them.
const ANSI_16: [Rgb; 16] = [
    Rgb::new(0x00, 0x00, 0x00), // 0 black
    Rgb::new(0x80, 0x00, 0x00), // 1 red
    Rgb::new(0x00, 0x80, 0x00), // 2 green
    Rgb::new(0x80, 0x80, 0x00), // 3 yellow
    Rgb::new(0x00, 0x00, 0x80), // 4 blue
    Rgb::new(0x80, 0x00, 0x80), // 5 magenta
    Rgb::new(0x00, 0x80, 0x80), // 6 cyan
    Rgb::new(0xC0, 0xC0, 0xC0), // 7 white
    Rgb::new(0x80, 0x80, 0x80), // 8 bright black
    Rgb::new(0xFF, 0x00, 0x00), // 9 bright red
    Rgb::new(0x00, 0xFF, 0x00), // 10 bright green
    Rgb::new(0xFF, 0xFF, 0x00), // 11 bright yellow
    Rgb::new(0x00, 0x00, 0xFF), // 12 bright blue
    Rgb::new(0xFF, 0x00, 0xFF), // 13 bright magenta
    Rgb::new(0x00, 0xFF, 0xFF), // 14 bright cyan
    Rgb::new(0xFF, 0xFF, 0xFF), // 15 bright white
];

/// Intensity levels of the 6×6×6 colour cube. Not evenly spaced — this is xterm's ramp.
const CUBE_LEVELS: [u8; 6] = [0x00, 0x5F, 0x87, 0xAF, 0xD7, 0xFF];

fn build_indexed() -> [Rgb; 256] {
    let mut table = [Rgb::new(0, 0, 0); 256];

    table[..16].copy_from_slice(&ANSI_16);

    // 16..232: the 6x6x6 cube.
    let mut i = 16;
    for r in CUBE_LEVELS {
        for g in CUBE_LEVELS {
            for b in CUBE_LEVELS {
                table[i] = Rgb::new(r, g, b);
                i += 1;
            }
        }
    }
    debug_assert_eq!(i, 232);

    // 232..256: 24 steps of grey, 8 to 238.
    for (step, entry) in table[232..].iter_mut().enumerate() {
        let level = 8 + 10 * step as u8;
        *entry = Rgb::new(level, level, level);
    }

    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_fully_populated_and_correctly_partitioned() {
        let p = Palette::xterm();
        assert_eq!(p.indexed(0), Rgb::new(0, 0, 0));
        assert_eq!(p.indexed(15), Rgb::new(0xFF, 0xFF, 0xFF));
        // First cube entry is black, last is white.
        assert_eq!(p.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(p.indexed(231), Rgb::new(0xFF, 0xFF, 0xFF));
        // Grey ramp endpoints.
        assert_eq!(p.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(p.indexed(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn cube_is_ordered_r_then_g_then_b() {
        let p = Palette::xterm();
        // Index 16 + 36*r + 6*g + b
        assert_eq!(p.indexed(16 + 36 * 5), Rgb::new(0xFF, 0x00, 0x00));
        assert_eq!(p.indexed(16 + 6 * 5), Rgb::new(0x00, 0xFF, 0x00));
        assert_eq!(p.indexed(16 + 5), Rgb::new(0x00, 0x00, 0xFF));
    }

    #[test]
    fn out_of_range_index_is_not_a_panic() {
        let p = Palette::xterm();
        assert_eq!(p.indexed(9999), p.foreground);
    }

    #[test]
    fn dimmed_is_two_thirds_and_saturates_at_zero() {
        assert_eq!(Rgb::new(255, 255, 255).dimmed(), Rgb::new(170, 170, 170));
        assert_eq!(Rgb::new(0, 0, 0).dimmed(), Rgb::new(0, 0, 0));
        assert_eq!(Rgb::new(1, 2, 3).dimmed(), Rgb::new(0, 1, 2));
    }

    #[test]
    fn as_u32_packs_in_rrggbb_order() {
        assert_eq!(Rgb::new(0x12, 0x34, 0x56).as_u32(), 0x0012_3456);
    }
}
