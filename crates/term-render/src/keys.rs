//! Turning key presses into the bytes a terminal expects.
//!
//! The encoding is xterm's, which is what `TERM=xterm-256color` promises the remote end. Getting it
//! wrong is not a cosmetic problem: an editor that receives `\x1b[A` where it expected `\x1bOA`
//! inserts stray characters instead of moving the cursor.
//!
//! [`encode`] is free of `egui` types so it can be tested exhaustively; [`from_egui`] and
//! [`mods_from_egui`] are the thin adapters over it.

/// Modifier state accompanying a key press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMods {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt / Option, which terminals encode as a leading escape.
    pub alt: bool,
}

impl KeyMods {
    /// No modifiers held.
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
    };

    /// Whether no modifier is held.
    pub fn is_none(self) -> bool {
        !self.shift && !self.ctrl && !self.alt
    }

    /// The xterm modifier parameter: `1 + shift + 2·alt + 4·ctrl`.
    ///
    /// This is the number that appears in sequences like `CSI 1 ; 5 A` for Ctrl+Up.
    pub fn csi_param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

/// A key, reduced to what affects the bytes sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermKey {
    /// Return.
    Enter,
    /// Backspace.
    Backspace,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// Insert.
    Insert,
    /// Delete-forward.
    Delete,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// Function key, 1-based. Only 1–12 are encoded.
    Function(u8),
    /// A printable character.
    Char(char),
}

/// Encode a key press, or `None` if it sends nothing.
pub fn encode(key: TermKey, mods: KeyMods) -> Option<Vec<u8>> {
    let bytes = match key {
        // Escape works as a prefix for Alt on every terminal in common use.
        TermKey::Enter => with_alt(mods, b"\r".to_vec()),
        TermKey::Escape => with_alt(mods, b"\x1b".to_vec()),

        // Backspace sends DEL, not BS — that is xterm's behaviour and what `stty erase` expects.
        // Ctrl+Backspace is the one that sends BS, which shells map to "delete word".
        TermKey::Backspace => {
            let base = if mods.ctrl {
                b"\x08".to_vec()
            } else {
                b"\x7f".to_vec()
            };
            with_alt(mods, base)
        }

        // Shift+Tab is a distinct sequence, not Tab with a modifier parameter.
        TermKey::Tab => {
            if mods.shift {
                b"\x1b[Z".to_vec()
            } else {
                with_alt(mods, b"\t".to_vec())
            }
        }

        TermKey::Up => csi_letter(b'A', mods),
        TermKey::Down => csi_letter(b'B', mods),
        TermKey::Right => csi_letter(b'C', mods),
        TermKey::Left => csi_letter(b'D', mods),
        TermKey::Home => csi_letter(b'H', mods),
        TermKey::End => csi_letter(b'F', mods),

        TermKey::Insert => csi_tilde(2, mods),
        TermKey::Delete => csi_tilde(3, mods),
        TermKey::PageUp => csi_tilde(5, mods),
        TermKey::PageDown => csi_tilde(6, mods),

        TermKey::Function(n) => function_key(n, mods)?,

        TermKey::Char(ch) => {
            let mut out = Vec::with_capacity(5);
            if mods.alt {
                out.push(0x1b);
            }
            if mods.ctrl {
                // Ctrl with a key that has no control code sends nothing at all, rather than the
                // bare character — otherwise Ctrl+1 would type a "1".
                out.push(control_code(ch)?);
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            out
        }
    };

    Some(bytes)
}

/// `CSI A` unmodified, `CSI 1 ; n A` with modifiers.
fn csi_letter(final_byte: u8, mods: KeyMods) -> Vec<u8> {
    if mods.is_none() {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{}{}", mods.csi_param(), final_byte as char).into_bytes()
    }
}

/// `CSI n ~` unmodified, `CSI n ; m ~` with modifiers.
fn csi_tilde(number: u8, mods: KeyMods) -> Vec<u8> {
    if mods.is_none() {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{}~", mods.csi_param()).into_bytes()
    }
}

/// F1–F4 use the SS3 form, F5–F12 the tilde form with xterm's discontinuous numbering.
fn function_key(n: u8, mods: KeyMods) -> Option<Vec<u8>> {
    let ss3 = |letter: char| -> Vec<u8> {
        if mods.is_none() {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[1;{}{letter}", mods.csi_param()).into_bytes()
        }
    };

    Some(match n {
        1 => ss3('P'),
        2 => ss3('Q'),
        3 => ss3('R'),
        4 => ss3('S'),
        // The gaps at 16 and 22 are historical: xterm skipped them.
        5 => csi_tilde(15, mods),
        6 => csi_tilde(17, mods),
        7 => csi_tilde(18, mods),
        8 => csi_tilde(19, mods),
        9 => csi_tilde(20, mods),
        10 => csi_tilde(21, mods),
        11 => csi_tilde(23, mods),
        12 => csi_tilde(24, mods),
        _ => return None,
    })
}

/// The control code produced by holding Ctrl with `ch`, if there is one.
fn control_code(ch: char) -> Option<u8> {
    let lower = ch.to_ascii_lowercase();
    match lower {
        'a'..='z' => Some(lower as u8 - b'a' + 1),
        '@' | ' ' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' | '-' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn with_alt(mods: KeyMods, mut bytes: Vec<u8>) -> Vec<u8> {
    if mods.alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// Translate an `egui` key into a [`TermKey`].
///
/// Returns `None` for keys a terminal has no encoding for — modifier keys on their own, media keys,
/// F13 and above.
pub fn from_egui(key: egui::Key) -> Option<TermKey> {
    use egui::Key as K;

    let mapped = match key {
        K::Enter => TermKey::Enter,
        K::Backspace => TermKey::Backspace,
        K::Tab => TermKey::Tab,
        K::Escape => TermKey::Escape,
        K::Insert => TermKey::Insert,
        K::Delete => TermKey::Delete,
        K::Home => TermKey::Home,
        K::End => TermKey::End,
        K::PageUp => TermKey::PageUp,
        K::PageDown => TermKey::PageDown,
        K::ArrowUp => TermKey::Up,
        K::ArrowDown => TermKey::Down,
        K::ArrowLeft => TermKey::Left,
        K::ArrowRight => TermKey::Right,
        K::Space => TermKey::Char(' '),

        K::F1 => TermKey::Function(1),
        K::F2 => TermKey::Function(2),
        K::F3 => TermKey::Function(3),
        K::F4 => TermKey::Function(4),
        K::F5 => TermKey::Function(5),
        K::F6 => TermKey::Function(6),
        K::F7 => TermKey::Function(7),
        K::F8 => TermKey::Function(8),
        K::F9 => TermKey::Function(9),
        K::F10 => TermKey::Function(10),
        K::F11 => TermKey::Function(11),
        K::F12 => TermKey::Function(12),

        // Punctuation that has a control code, spelled out because `symbol_or_name` returns
        // typographic characters for some of these — `Key::Minus` yields U+2212, not '-'.
        K::OpenBracket => TermKey::Char('['),
        K::CloseBracket => TermKey::Char(']'),
        K::Backslash => TermKey::Char('\\'),
        K::Minus => TermKey::Char('-'),
        K::Questionmark => TermKey::Char('?'),

        // `Key::name()` yields a single ASCII character for exactly the letters A–Z and digits 0–9,
        // and a multi-character word for everything else, so this test is precise.
        other => {
            let name = other.name();
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) if ch.is_ascii() => TermKey::Char(ch.to_ascii_lowercase()),
                _ => return None,
            }
        }
    };

    Some(mapped)
}

/// Extract the modifiers a terminal cares about.
///
/// `command` and `mac_cmd` are ignored: on Windows and Linux they duplicate `ctrl`, and BestTerm
/// does not target macOS.
pub fn mods_from_egui(mods: &egui::Modifiers) -> KeyMods {
    KeyMods {
        shift: mods.shift,
        ctrl: mods.ctrl,
        alt: mods.alt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: TermKey, mods: KeyMods) -> Vec<u8> {
        encode(key, mods).expect("key should encode")
    }

    const CTRL: KeyMods = KeyMods {
        shift: false,
        ctrl: true,
        alt: false,
    };
    const ALT: KeyMods = KeyMods {
        shift: false,
        ctrl: false,
        alt: true,
    };
    const SHIFT: KeyMods = KeyMods {
        shift: true,
        ctrl: false,
        alt: false,
    };

    #[test]
    fn plain_characters_are_utf8() {
        assert_eq!(enc(TermKey::Char('a'), KeyMods::NONE), b"a");
        assert_eq!(enc(TermKey::Char('Z'), KeyMods::NONE), b"Z");
        assert_eq!(enc(TermKey::Char('ф'), KeyMods::NONE), "ф".as_bytes());
    }

    #[test]
    fn enter_backspace_tab_escape() {
        assert_eq!(enc(TermKey::Enter, KeyMods::NONE), b"\r");
        assert_eq!(enc(TermKey::Escape, KeyMods::NONE), b"\x1b");
        assert_eq!(enc(TermKey::Tab, KeyMods::NONE), b"\t");
        // DEL, not BS — this is the one everyone gets wrong.
        assert_eq!(enc(TermKey::Backspace, KeyMods::NONE), b"\x7f");
        assert_eq!(enc(TermKey::Backspace, CTRL), b"\x08");
    }

    #[test]
    fn shift_tab_is_its_own_sequence() {
        assert_eq!(enc(TermKey::Tab, SHIFT), b"\x1b[Z");
    }

    #[test]
    fn unmodified_arrows_use_the_csi_form() {
        assert_eq!(enc(TermKey::Up, KeyMods::NONE), b"\x1b[A");
        assert_eq!(enc(TermKey::Down, KeyMods::NONE), b"\x1b[B");
        assert_eq!(enc(TermKey::Right, KeyMods::NONE), b"\x1b[C");
        assert_eq!(enc(TermKey::Left, KeyMods::NONE), b"\x1b[D");
        assert_eq!(enc(TermKey::Home, KeyMods::NONE), b"\x1b[H");
        assert_eq!(enc(TermKey::End, KeyMods::NONE), b"\x1b[F");
    }

    #[test]
    fn modified_arrows_carry_the_xterm_parameter() {
        // 1 + 4 (ctrl) = 5
        assert_eq!(enc(TermKey::Right, CTRL), b"\x1b[1;5C");
        // 1 + 1 (shift) = 2
        assert_eq!(enc(TermKey::Left, SHIFT), b"\x1b[1;2D");
        // 1 + 2 (alt) = 3
        assert_eq!(enc(TermKey::Up, ALT), b"\x1b[1;3A");
        // 1 + 1 + 2 + 4 = 8
        let all = KeyMods {
            shift: true,
            ctrl: true,
            alt: true,
        };
        assert_eq!(all.csi_param(), 8);
        assert_eq!(enc(TermKey::Down, all), b"\x1b[1;8B");
    }

    #[test]
    fn navigation_keys_use_the_tilde_form() {
        assert_eq!(enc(TermKey::Insert, KeyMods::NONE), b"\x1b[2~");
        assert_eq!(enc(TermKey::Delete, KeyMods::NONE), b"\x1b[3~");
        assert_eq!(enc(TermKey::PageUp, KeyMods::NONE), b"\x1b[5~");
        assert_eq!(enc(TermKey::PageDown, KeyMods::NONE), b"\x1b[6~");
        assert_eq!(enc(TermKey::Delete, CTRL), b"\x1b[3;5~");
    }

    #[test]
    fn function_keys_follow_xterms_split_and_gaps() {
        assert_eq!(enc(TermKey::Function(1), KeyMods::NONE), b"\x1bOP");
        assert_eq!(enc(TermKey::Function(4), KeyMods::NONE), b"\x1bOS");
        assert_eq!(enc(TermKey::Function(5), KeyMods::NONE), b"\x1b[15~");
        // 16 is skipped.
        assert_eq!(enc(TermKey::Function(6), KeyMods::NONE), b"\x1b[17~");
        // 22 is skipped.
        assert_eq!(enc(TermKey::Function(11), KeyMods::NONE), b"\x1b[23~");
        assert_eq!(enc(TermKey::Function(12), KeyMods::NONE), b"\x1b[24~");
        assert_eq!(enc(TermKey::Function(1), CTRL), b"\x1b[1;5P");
        assert!(encode(TermKey::Function(13), KeyMods::NONE).is_none());
        assert!(encode(TermKey::Function(0), KeyMods::NONE).is_none());
    }

    #[test]
    fn ctrl_letters_map_to_control_codes() {
        assert_eq!(enc(TermKey::Char('a'), CTRL), [0x01]);
        assert_eq!(enc(TermKey::Char('c'), CTRL), [0x03]);
        assert_eq!(enc(TermKey::Char('d'), CTRL), [0x04]);
        assert_eq!(enc(TermKey::Char('z'), CTRL), [0x1a]);
        // Case must not matter: Ctrl+Shift+C is still ETX.
        assert_eq!(enc(TermKey::Char('C'), CTRL), [0x03]);
    }

    #[test]
    fn ctrl_punctuation_maps_to_the_c0_tail() {
        assert_eq!(enc(TermKey::Char(' '), CTRL), [0x00]);
        assert_eq!(enc(TermKey::Char('['), CTRL), [0x1b]);
        assert_eq!(enc(TermKey::Char('\\'), CTRL), [0x1c]);
        assert_eq!(enc(TermKey::Char(']'), CTRL), [0x1d]);
        assert_eq!(enc(TermKey::Char('?'), CTRL), [0x7f]);
    }

    #[test]
    fn ctrl_with_no_control_code_sends_nothing() {
        // Otherwise Ctrl+1 would type "1", which no terminal does.
        assert!(encode(TermKey::Char('1'), CTRL).is_none());
        assert!(encode(TermKey::Char('%'), CTRL).is_none());
    }

    #[test]
    fn alt_prefixes_an_escape() {
        assert_eq!(enc(TermKey::Char('f'), ALT), b"\x1bf");
        assert_eq!(enc(TermKey::Enter, ALT), b"\x1b\r");
        assert_eq!(enc(TermKey::Backspace, ALT), b"\x1b\x7f");
    }

    #[test]
    fn alt_and_ctrl_combine() {
        // Alt+Ctrl+A is ESC then SOH.
        assert_eq!(
            enc(
                TermKey::Char('a'),
                KeyMods {
                    shift: false,
                    ctrl: true,
                    alt: true
                }
            ),
            [0x1b, 0x01]
        );
    }

    #[test]
    fn egui_letters_and_digits_become_lowercase_chars() {
        assert_eq!(from_egui(egui::Key::A), Some(TermKey::Char('a')));
        assert_eq!(from_egui(egui::Key::Z), Some(TermKey::Char('z')));
        assert_eq!(from_egui(egui::Key::Num7), Some(TermKey::Char('7')));
    }

    #[test]
    fn egui_named_keys_map_across() {
        assert_eq!(from_egui(egui::Key::ArrowUp), Some(TermKey::Up));
        assert_eq!(from_egui(egui::Key::Enter), Some(TermKey::Enter));
        assert_eq!(from_egui(egui::Key::F7), Some(TermKey::Function(7)));
        assert_eq!(from_egui(egui::Key::Space), Some(TermKey::Char(' ')));
    }

    #[test]
    fn egui_minus_is_ascii_hyphen_not_the_typographic_minus() {
        // `Key::symbol_or_name()` would give U+2212 here; that must not reach the pty.
        assert_eq!(from_egui(egui::Key::Minus), Some(TermKey::Char('-')));
        assert_eq!(enc(TermKey::Char('-'), KeyMods::NONE), b"-");
    }

    #[test]
    fn egui_keys_without_a_terminal_meaning_are_dropped() {
        assert_eq!(from_egui(egui::Key::F13), None);
    }

    #[test]
    fn every_egui_key_either_maps_or_is_refused_without_panicking() {
        for key in egui::Key::ALL {
            let _ = from_egui(*key).and_then(|k| encode(k, KeyMods::NONE));
        }
    }

    #[test]
    fn modifier_extraction_ignores_command() {
        let mods = egui::Modifiers {
            alt: true,
            ctrl: false,
            shift: true,
            mac_cmd: true,
            command: true,
        };
        assert_eq!(
            mods_from_egui(&mods),
            KeyMods {
                shift: true,
                ctrl: false,
                alt: true
            }
        );
    }
}
