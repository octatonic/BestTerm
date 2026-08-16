//! Keys, from what the window system reports to what a remote desktop expects.
//!
//! [`bestterm_surface::InputEvent::Key`] carries a PC scan code set 1 make code, with keys a real
//! keyboard prefixes with `0xE0` written as `0x100 | code`. This turns egui's key into one.
//!
//! # Physical, not logical
//!
//! egui reports both: `key` is the logical key, which honours the person's keyboard layout, and
//! `physical_key` is where the key actually is. This uses the physical one, because a remote desktop
//! applies *its own* layout to the scan code it receives. Sending the logical key would apply the
//! layout twice — once here and once there — which is the classic "my keyboard is wrong over RDP",
//! where a French keyboard types QWERTY into a French Windows.
//!
//! egui's own documentation says `physical_key` is not recommended and makes sense only for cases
//! like a game where WSAD must stay where it is regardless of layout. A remote desktop is exactly
//! that case, and the fallback to the logical key exists for the platforms where egui does not report
//! a physical one yet.
//!
//! # What is missing
//!
//! Caps Lock, Num Lock, Scroll Lock, Print Screen, Pause and the context-menu key: egui has no
//! variant for any of them, so they cannot be forwarded, and a key that cannot be named cannot be
//! sent. The numeric keypad is likewise indistinguishable from the number row, so keypad Enter
//! arrives as the main Enter — which matters to programs that tell them apart.

/// The bit that marks a scan code as `0xE0`-prefixed.
const EXTENDED: u32 = 0x100;

/// Which scan code `key` is, or `None` for one this build cannot name.
///
/// The order below is the keyboard's, not the alphabet's: it is checked against a scan code chart,
/// and a list in row order can be read against one.
pub(crate) fn scancode(key: egui::Key) -> Option<u32> {
    use egui::Key as K;

    Some(match key {
        // Row one.
        K::Escape => 0x01,
        K::Num1 | K::Exclamationmark => 0x02,
        K::Num2 => 0x03,
        K::Num3 => 0x04,
        K::Num4 => 0x05,
        K::Num5 => 0x06,
        K::Num6 => 0x07,
        K::Num7 => 0x08,
        K::Num8 => 0x09,
        K::Num9 => 0x0A,
        K::Num0 => 0x0B,
        K::Minus => 0x0C,
        K::Equals | K::Plus => 0x0D,
        K::Backspace => 0x0E,

        // Row two.
        K::Tab => 0x0F,
        K::Q => 0x10,
        K::W => 0x11,
        K::E => 0x12,
        K::R => 0x13,
        K::T => 0x14,
        K::Y => 0x15,
        K::U => 0x16,
        K::I => 0x17,
        K::O => 0x18,
        K::P => 0x19,
        K::OpenBracket | K::OpenCurlyBracket => 0x1A,
        K::CloseBracket | K::CloseCurlyBracket => 0x1B,
        K::Enter => 0x1C,

        // Row three.
        K::ControlLeft => 0x1D,
        K::A => 0x1E,
        K::S => 0x1F,
        K::D => 0x20,
        K::F => 0x21,
        K::G => 0x22,
        K::H => 0x23,
        K::J => 0x24,
        K::K => 0x25,
        K::L => 0x26,
        K::Semicolon | K::Colon => 0x27,
        K::Quote => 0x28,
        K::Backtick => 0x29,

        // Row four.
        K::ShiftLeft => 0x2A,
        K::Backslash | K::Pipe => 0x2B,
        K::Z => 0x2C,
        K::X => 0x2D,
        K::C => 0x2E,
        K::V => 0x2F,
        K::B => 0x30,
        K::N => 0x31,
        K::M => 0x32,
        K::Comma => 0x33,
        K::Period => 0x34,
        K::Slash | K::Questionmark => 0x35,
        K::ShiftRight => 0x36,

        // Row five.
        K::AltLeft => 0x38,
        K::Space => 0x39,

        // Function keys. F11 and F12 are not adjacent to F10 in the chart, which is a quirk of the
        // original keyboard and not a mistake here.
        K::F1 => 0x3B,
        K::F2 => 0x3C,
        K::F3 => 0x3D,
        K::F4 => 0x3E,
        K::F5 => 0x3F,
        K::F6 => 0x40,
        K::F7 => 0x41,
        K::F8 => 0x42,
        K::F9 => 0x43,
        K::F10 => 0x44,
        K::F11 => 0x57,
        K::F12 => 0x58,

        // The `0xE0`-prefixed group: the keys a PC/XT keyboard did not have, which were added with a
        // prefix so that software written for one kept working.
        K::ControlRight => EXTENDED | 0x1D,
        K::AltRight => EXTENDED | 0x38,
        K::Home => EXTENDED | 0x47,
        K::ArrowUp => EXTENDED | 0x48,
        K::PageUp => EXTENDED | 0x49,
        K::ArrowLeft => EXTENDED | 0x4B,
        K::ArrowRight => EXTENDED | 0x4D,
        K::End => EXTENDED | 0x4F,
        K::ArrowDown => EXTENDED | 0x50,
        K::PageDown => EXTENDED | 0x51,
        K::Insert => EXTENDED | 0x52,
        K::Delete => EXTENDED | 0x53,
        K::SuperLeft => EXTENDED | 0x5B,
        K::SuperRight => EXTENDED | 0x5C,

        // F13 upwards, the editing shortcuts egui synthesises, and the browser key. None of them is a
        // key on the keyboard this maps, and inventing a code for one would send a real key nobody
        // pressed.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use egui::Key as K;

    use super::*;

    #[test]
    fn the_two_controls_and_the_two_alts_are_different_keys() {
        // The whole reason the extended bit exists. If these collided, a remote host would see the
        // left-hand key whichever was pressed, and AltGr — which is right Alt — would stop working.
        assert_eq!(scancode(K::ControlLeft), Some(0x1D));
        assert_eq!(scancode(K::ControlRight), Some(0x11D));
        assert_eq!(scancode(K::AltLeft), Some(0x38));
        assert_eq!(scancode(K::AltRight), Some(0x138));
        assert_ne!(scancode(K::ShiftLeft), scancode(K::ShiftRight));
    }

    #[test]
    fn the_navigation_block_is_extended() {
        // All of it, because these were added to the keyboard after the scan code set was fixed.
        for key in [
            K::Home,
            K::End,
            K::PageUp,
            K::PageDown,
            K::Insert,
            K::Delete,
            K::ArrowUp,
            K::ArrowDown,
            K::ArrowLeft,
            K::ArrowRight,
        ] {
            let code = scancode(key).unwrap_or_else(|| panic!("{key:?} has a scan code"));
            assert_eq!(code & EXTENDED, EXTENDED, "{key:?} is {code:#x}");
        }
    }

    #[test]
    fn no_two_keys_share_a_code() {
        // Except where they are deliberately the same physical key: egui reports `[` and `{` as
        // different keys, and on a keyboard they are one. Those pairs are listed rather than
        // discovered, so that a new collision is a failure and not a shrug.
        let deliberate: &[(K, K)] = &[
            (K::OpenBracket, K::OpenCurlyBracket),
            (K::CloseBracket, K::CloseCurlyBracket),
            (K::Semicolon, K::Colon),
            (K::Slash, K::Questionmark),
            (K::Backslash, K::Pipe),
            (K::Equals, K::Plus),
            (K::Num1, K::Exclamationmark),
        ];

        let mut seen: std::collections::HashMap<u32, K> = std::collections::HashMap::new();
        for key in every_key() {
            let Some(code) = scancode(key) else { continue };
            if let Some(previous) = seen.insert(code, key) {
                let allowed = deliberate
                    .iter()
                    .any(|(a, b)| (*a == key && *b == previous) || (*b == key && *a == previous));
                assert!(allowed, "{key:?} and {previous:?} both map to {code:#x}");
            }
        }
    }

    #[test]
    fn every_code_fits_what_the_wire_can_carry() {
        // A byte plus the extended bit. Anything larger is refused at the far end rather than
        // truncated, so this failing here is better than a key silently becoming another one.
        for key in every_key() {
            if let Some(code) = scancode(key) {
                assert!(code <= 0x1FF, "{key:?} is {code:#x}");
                assert_ne!(code & 0xFF, 0, "{key:?} has no make code");
            }
        }
    }

    #[test]
    fn the_letters_are_in_keyboard_order_and_not_alphabetical() {
        // Q W E R T Y, which is the check that catches a table typed from the alphabet.
        let row: Vec<u32> = [K::Q, K::W, K::E, K::R, K::T, K::Y]
            .iter()
            .map(|key| scancode(*key).expect("a letter has a scan code"))
            .collect();
        assert_eq!(row, vec![0x10, 0x11, 0x12, 0x13, 0x14, 0x15]);

        let home: Vec<u32> = [K::A, K::S, K::D, K::F, K::G]
            .iter()
            .map(|key| scancode(*key).expect("a letter has a scan code"))
            .collect();
        assert_eq!(home, vec![0x1E, 0x1F, 0x20, 0x21, 0x22]);
    }

    #[test]
    fn a_key_with_nowhere_to_go_is_refused_rather_than_invented() {
        // F13 and above exist in egui and not on the keyboard this maps. Giving them a code would
        // send a real key nobody pressed.
        assert_eq!(scancode(K::F13), None);
        assert_eq!(scancode(K::F24), None);
        // Copy/Cut/Paste are synthesised by egui from a shortcut, not reported by the keyboard.
        assert_eq!(scancode(K::Copy), None);
        assert_eq!(scancode(K::Paste), None);
    }

    /// Every key egui defines.
    fn every_key() -> impl Iterator<Item = K> {
        K::ALL.iter().copied()
    }
}
