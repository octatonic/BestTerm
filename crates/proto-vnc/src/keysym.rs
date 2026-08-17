//! Scan codes to X11 keysyms.
//!
//! [`bestterm_surface::InputEvent::Key`] carries a PC set 1 scan code, because that is what RDP puts
//! on the wire and settling on one numbering was worth more than meeting both halfway. RFB wants an
//! X11 keysym. This is the table between them.
//!
//! # Why the unshifted keysym is the right answer
//!
//! A keysym names a *symbol*, so `a` and `A` are different keysyms — which looks like it means the
//! shift state has to be applied here. It does not. The remote X server has its own idea of the
//! modifier state, built from the Shift key transitions this client also sends, and it applies that
//! to whatever arrives. Sending `A` while Shift is held produces a capital letter twice over on some
//! servers and a stuck modifier on others.
//!
//! So: the unshifted symbol for the key's position, and Shift as a key in its own right. That is what
//! every working VNC client does, and it is the reason this table has one entry per physical key
//! rather than two.
//!
//! # What has no keysym here
//!
//! The scan codes egui cannot produce, since nothing upstream can send them: the keypad, the lock
//! keys, Print Screen and Pause. They are absent rather than guessed, because a wrong keysym types a
//! wrong character and a missing one types nothing — and the second is easier to notice and to fix.

/// The bit that marks a scan code as `0xE0`-prefixed.
const EXTENDED: u32 = 0x100;

/// The X11 keysym for a PC set 1 scan code, or `None` for one with no mapping here.
///
/// In keyboard order, checked against a scan code chart and an X11 keysym header, because a list in
/// row order can be read against the first and a list in keysym order can be read against the second
/// — and being able to check one of them is worth more than being able to skim both.
pub fn keysym(scancode: u32) -> Option<u32> {
    Some(match scancode {
        // Row one.
        0x01 => 0xFF1B, // Escape
        0x02 => 0x0031, // 1
        0x03 => 0x0032,
        0x04 => 0x0033,
        0x05 => 0x0034,
        0x06 => 0x0035,
        0x07 => 0x0036,
        0x08 => 0x0037,
        0x09 => 0x0038,
        0x0A => 0x0039,
        0x0B => 0x0030, // 0
        0x0C => 0x002D, // minus
        0x0D => 0x003D, // equal
        0x0E => 0xFF08, // BackSpace

        // Row two.
        0x0F => 0xFF09, // Tab
        0x10 => 0x0071, // q
        0x11 => 0x0077,
        0x12 => 0x0065,
        0x13 => 0x0072,
        0x14 => 0x0074,
        0x15 => 0x0079,
        0x16 => 0x0075,
        0x17 => 0x0069,
        0x18 => 0x006F,
        0x19 => 0x0070, // p
        0x1A => 0x005B, // bracketleft
        0x1B => 0x005D, // bracketright
        0x1C => 0xFF0D, // Return

        // Row three.
        0x1D => 0xFFE3, // Control_L
        0x1E => 0x0061, // a
        0x1F => 0x0073,
        0x20 => 0x0064,
        0x21 => 0x0066,
        0x22 => 0x0067,
        0x23 => 0x0068,
        0x24 => 0x006A,
        0x25 => 0x006B,
        0x26 => 0x006C, // l
        0x27 => 0x003B, // semicolon
        0x28 => 0x0027, // apostrophe
        0x29 => 0x0060, // grave

        // Row four.
        0x2A => 0xFFE1, // Shift_L
        0x2B => 0x005C, // backslash
        0x2C => 0x007A, // z
        0x2D => 0x0078,
        0x2E => 0x0063,
        0x2F => 0x0076,
        0x30 => 0x0062,
        0x31 => 0x006E,
        0x32 => 0x006D, // m
        0x33 => 0x002C, // comma
        0x34 => 0x002E, // period
        0x35 => 0x002F, // slash
        0x36 => 0xFFE2, // Shift_R

        // Row five.
        0x38 => 0xFFE9, // Alt_L
        0x39 => 0x0020, // space

        // Function keys.
        0x3B => 0xFFBE, // F1
        0x3C => 0xFFBF,
        0x3D => 0xFFC0,
        0x3E => 0xFFC1,
        0x3F => 0xFFC2,
        0x40 => 0xFFC3,
        0x41 => 0xFFC4,
        0x42 => 0xFFC5,
        0x43 => 0xFFC6,
        0x44 => 0xFFC7, // F10
        0x57 => 0xFFC8, // F11
        0x58 => 0xFFC9, // F12

        // The `0xE0`-prefixed group.
        code if code == EXTENDED | 0x1D => 0xFFE4, // Control_R
        // Alt_R, which on a European layout is AltGr and is a different keysym. Sending Alt_R there
        // gives an Alt that never unlocks the third level. Servers differ on whether they treat Alt_R as
        // AltGr; the ones that do not need ISO_Level3_Shift instead, which is a layout question this
        // table cannot answer from a scan code alone.
        code if code == EXTENDED | 0x38 => 0xFFEA, // Alt_R
        code if code == EXTENDED | 0x47 => 0xFF50, // Home
        code if code == EXTENDED | 0x48 => 0xFF52, // Up
        code if code == EXTENDED | 0x49 => 0xFF55, // Prior
        code if code == EXTENDED | 0x4B => 0xFF51, // Left
        code if code == EXTENDED | 0x4D => 0xFF53, // Right
        code if code == EXTENDED | 0x4F => 0xFF57, // End
        code if code == EXTENDED | 0x50 => 0xFF54, // Down
        code if code == EXTENDED | 0x51 => 0xFF56, // Next
        code if code == EXTENDED | 0x52 => 0xFF63, // Insert
        code if code == EXTENDED | 0x53 => 0xFFFF, // Delete
        code if code == EXTENDED | 0x5B => 0xFFEB, // Super_L
        code if code == EXTENDED | 0x5C => 0xFFEC, // Super_R

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_are_the_unshifted_symbol() {
        // Not the capital, even though a keysym names a symbol. The remote X server builds the
        // modifier state from the Shift transitions this client also sends and applies it itself;
        // sending `A` while Shift is held capitalises twice on some servers and sticks on others.
        assert_eq!(keysym(0x1E), Some(0x0061), "a, not A");
        assert_eq!(keysym(0x10), Some(0x0071), "q, not Q");
    }

    #[test]
    fn the_two_controls_shifts_and_alts_are_different_keysyms() {
        // The whole reason the extended bit exists. Collapsed, AltGr stops working on every European
        // layout and Control_R behaves as Control_L.
        assert_eq!(keysym(0x1D), Some(0xFFE3), "Control_L");
        assert_eq!(keysym(EXTENDED | 0x1D), Some(0xFFE4), "Control_R");
        assert_eq!(keysym(0x38), Some(0xFFE9), "Alt_L");
        assert_eq!(keysym(EXTENDED | 0x38), Some(0xFFEA), "Alt_R");
        assert_ne!(keysym(0x2A), keysym(0x36), "the two shifts");
    }

    #[test]
    fn the_navigation_block_maps_to_its_own_keysyms() {
        for (scancode, expected, name) in [
            (0x47u32, 0xFF50u32, "Home"),
            (0x48, 0xFF52, "Up"),
            (0x4B, 0xFF51, "Left"),
            (0x4D, 0xFF53, "Right"),
            (0x50, 0xFF54, "Down"),
            (0x4F, 0xFF57, "End"),
            (0x49, 0xFF55, "PageUp"),
            (0x51, 0xFF56, "PageDown"),
            (0x52, 0xFF63, "Insert"),
            (0x53, 0xFFFF, "Delete"),
        ] {
            assert_eq!(keysym(EXTENDED | scancode), Some(expected), "{name}");
        }
    }

    #[test]
    fn the_navigation_block_is_only_reachable_with_the_extended_bit() {
        // Without it these scan codes are the numeric keypad, which is a different set of keys and
        // has no mapping here. Answering them with the arrow keysyms would type an arrow when
        // somebody pressed a number.
        for scancode in [0x47u32, 0x48, 0x4B, 0x4D, 0x50, 0x53] {
            assert_eq!(keysym(scancode), None, "bare {scancode:#x} is the keypad");
        }
    }

    #[test]
    fn no_two_keys_share_a_keysym() {
        // A collision types the wrong character, and does it consistently enough to look like a
        // keyboard layout problem rather than a table one.
        let mut seen = std::collections::HashMap::new();
        for scancode in 0u32..=0x1FF {
            let Some(keysym) = keysym(scancode) else {
                continue;
            };
            if let Some(previous) = seen.insert(keysym, scancode) {
                panic!("{scancode:#x} and {previous:#x} both give keysym {keysym:#x}");
            }
        }
    }

    #[test]
    fn the_alphabet_is_in_keyboard_order_and_not_alphabetical() {
        // Q W E R T Y, which is the check that catches a table typed from the alphabet.
        let row: Vec<u32> = (0x10..=0x15).filter_map(keysym).collect();
        assert_eq!(row, vec![0x71, 0x77, 0x65, 0x72, 0x74, 0x79]);
    }

    #[test]
    fn a_key_with_no_mapping_is_refused_rather_than_guessed() {
        // A wrong keysym types a wrong character; a missing one types nothing, and the second is
        // easier both to notice and to fix.
        assert_eq!(keysym(0x00), None);
        assert_eq!(
            keysym(0x3A),
            None,
            "Caps Lock, which egui cannot send anyway"
        );
        assert_eq!(keysym(0x45), None, "Num Lock");
        assert_eq!(keysym(0xFFFF), None);
    }

    #[test]
    fn every_keysym_is_one_x11_actually_defines() {
        // Latin-1 characters are their own code point; everything else is in the 0xFF00 page. A value
        // outside both is a typo, and a typo here is a key that does nothing on one server and
        // something surprising on another.
        for scancode in 0u32..=0x1FF {
            let Some(keysym) = keysym(scancode) else {
                continue;
            };
            let latin1 = (0x20..=0xFF).contains(&keysym);
            let function = (0xFF00..=0xFFFF).contains(&keysym);
            assert!(
                latin1 || function,
                "{scancode:#x} gives {keysym:#x}, which is in neither page"
            );
        }
    }
}
