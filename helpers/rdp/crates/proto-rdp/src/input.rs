//! Turning [`bestterm_surface::InputEvent`] into what RDP puts on the wire.
//!
//! All of it goes as fast-path input, which is what every RDP server since Windows Server 2003 wants
//! and what IronRDP's [`ActiveStage::process_fastpath_input`] takes.
//!
//! # Keys
//!
//! A key transition carries a PC set 1 scan code, with extended keys as `0x100 | code`. RDP wants the
//! low byte and a flag, so the conversion is a mask and a bit — but only because both sides agreed on
//! set 1 first. See [`bestterm_surface::InputEvent`] for why that had to be settled rather than left
//! to whichever numbering each side happened to use.
//!
//! Modifiers are not sent alongside a key. RDP tracks them from the key transitions themselves:
//! Shift goes down as its own event and stays down until its release arrives. Sending the modifier
//! state as well would be a second, disagreeing account of it, and the disagreement shows up as a
//! Shift the remote host thinks is still held.
//!
//! # The pointer
//!
//! A button press and a movement are the same PDU with different flags, and a press carries the
//! position with it — so a click is one event, not a move followed by a click. Buttons four and five
//! are a different PDU entirely, which is why they are separated here rather than folded in with a
//! wider flag set.
//!
//! # What is not sent
//!
//! [`InputEvent::Text`] and [`InputEvent::ClipboardProvide`]. Composed text needs the Unicode
//! keyboard event, which needs a decision about how it interacts with the scan-code stream an input
//! method also produces; the clipboard needs the clipboard virtual channel. Both are refused rather
//! than dropped, so the gap is visible where it happens.

use bestterm_surface::{InputEvent, PointerButton};
use ironrdp_pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};
use ironrdp_pdu::input::mouse::{MousePdu, PointerFlags};
use ironrdp_pdu::input::mouse_x::{MouseXPdu, PointerXFlags};

/// Marks a scan code as one a keyboard would prefix with `0xE0`.
///
/// The right-hand Control is `0x11D`; the left-hand one is `0x1D`.
const EXTENDED: u32 = 0x100;

/// Why an input event could not be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsendable {
    /// Composed text, which needs the Unicode keyboard event.
    Text,
    /// A clipboard offer, which needs the clipboard virtual channel.
    Clipboard,
    /// A scan code outside what RDP can carry.
    ///
    /// Set 1 make codes are one byte, so anything above `0x1FF` — a byte plus the extended bit — is
    /// not a scan code at all and would silently become a different key if it were masked down.
    Scancode {
        /// What arrived.
        scancode: u32,
    },
}

impl std::fmt::Display for Unsendable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => f.write_str("composed text cannot be sent to this server yet"),
            Self::Clipboard => f.write_str("the clipboard is not shared with this server yet"),
            Self::Scancode { scancode } => {
                write!(f, "{scancode:#x} is not a PC set 1 scan code")
            }
        }
    }
}

/// Convert one event, or say why it cannot be.
///
/// A pointer movement also has to reach [`ironrdp_session::ActiveStage::update_mouse_pos`], which is
/// what keeps a software-rendered cursor under the pointer; that is the caller's job, because it
/// needs the stage and this does not.
pub fn convert(event: &InputEvent) -> Result<FastPathInputEvent, Unsendable> {
    match event {
        InputEvent::Key {
            scancode, pressed, ..
        } => {
            if *scancode > 0x1FF {
                return Err(Unsendable::Scancode {
                    scancode: *scancode,
                });
            }
            let mut flags = KeyboardFlags::empty();
            if !pressed {
                flags |= KeyboardFlags::RELEASE;
            }
            if scancode & EXTENDED != 0 {
                flags |= KeyboardFlags::EXTENDED;
            }
            // Masked after the extended bit has been read off it, never before.
            let code = u8::try_from(scancode & 0xFF).map_err(|_| Unsendable::Scancode {
                scancode: *scancode,
            })?;
            Ok(FastPathInputEvent::KeyboardEvent(flags, code))
        }

        InputEvent::PointerMove { x, y } => Ok(FastPathInputEvent::MouseEvent(MousePdu {
            flags: PointerFlags::MOVE,
            number_of_wheel_rotation_units: 0,
            x_position: clamp_coord(*x),
            y_position: clamp_coord(*y),
        })),

        InputEvent::PointerButton {
            button,
            pressed,
            x,
            y,
        } => {
            let (x_position, y_position) = (clamp_coord(*x), clamp_coord(*y));
            match button {
                PointerButton::Left | PointerButton::Middle | PointerButton::Right => {
                    let mut flags = match button {
                        PointerButton::Left => PointerFlags::LEFT_BUTTON,
                        PointerButton::Middle => PointerFlags::MIDDLE_BUTTON_OR_WHEEL,
                        _ => PointerFlags::RIGHT_BUTTON,
                    };
                    // A release is the same flag without DOWN, not a separate flag. Sending DOWN on
                    // both is how a button gets stuck.
                    if *pressed {
                        flags |= PointerFlags::DOWN;
                    }
                    Ok(FastPathInputEvent::MouseEvent(MousePdu {
                        flags,
                        number_of_wheel_rotation_units: 0,
                        x_position,
                        y_position,
                    }))
                }
                // Buttons four and five ride a different PDU. Folding them into the one above would
                // mean inventing flag values that mean something else.
                PointerButton::X1 | PointerButton::X2 => {
                    let mut flags = match button {
                        PointerButton::X1 => PointerXFlags::BUTTON1,
                        _ => PointerXFlags::BUTTON2,
                    };
                    if *pressed {
                        flags |= PointerXFlags::DOWN;
                    }
                    Ok(FastPathInputEvent::MouseEventEx(MouseXPdu {
                        flags,
                        x_position,
                        y_position,
                    }))
                }
            }
        }

        InputEvent::Scroll { dx, dy } => {
            // Vertical wins when both are present. RDP carries one axis per PDU, and a diagonal
            // gesture split across two would arrive as two scrolls in sequence, which reads as a
            // stutter; the axis somebody meant is nearly always the larger one.
            let (axis, delta) = if dy.abs() >= dx.abs() {
                (PointerFlags::VERTICAL_WHEEL, *dy)
            } else {
                (PointerFlags::HORIZONTAL_WHEEL, *dx)
            };
            Ok(FastPathInputEvent::MouseEvent(MousePdu {
                flags: axis,
                number_of_wheel_rotation_units: wheel_units(delta),
                x_position: 0,
                y_position: 0,
            }))
        }

        InputEvent::Text(_) => Err(Unsendable::Text),
        InputEvent::ClipboardProvide(_) => Err(Unsendable::Clipboard),
    }
}

/// A framebuffer coordinate as RDP carries it.
///
/// Saturating rather than wrapping: a coordinate past 65535 is off any desktop RDP can negotiate, and
/// wrapping it would put the pointer at the opposite edge — a click in the wrong place is worse than
/// a click at the edge.
fn clamp_coord(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// Scroll lines as RDP counts them.
///
/// One notch is 120 units, which is the convention every desktop platform inherited from Windows.
/// Clamped for the same reason coordinates are.
fn wheel_units(lines: f32) -> i16 {
    let units = lines * 120.0;
    if units >= f32::from(i16::MAX) {
        i16::MAX
    } else if units <= f32::from(i16::MIN) {
        i16::MIN
    } else {
        units as i16
    }
}

#[cfg(test)]
mod tests {
    use bestterm_surface::Modifiers;

    use super::*;

    fn key(scancode: u32, pressed: bool) -> InputEvent {
        InputEvent::Key {
            scancode,
            pressed,
            mods: Modifiers::default(),
        }
    }

    #[test]
    fn a_press_and_a_release_differ_only_by_a_flag() {
        let FastPathInputEvent::KeyboardEvent(down, code) =
            convert(&key(0x1E, true)).expect("a scan code")
        else {
            panic!("expected a keyboard event");
        };
        assert_eq!(code, 0x1E);
        assert!(!down.contains(KeyboardFlags::RELEASE));

        let FastPathInputEvent::KeyboardEvent(up, code) =
            convert(&key(0x1E, false)).expect("a scan code")
        else {
            panic!("expected a keyboard event");
        };
        assert_eq!(code, 0x1E);
        assert!(up.contains(KeyboardFlags::RELEASE));
    }

    #[test]
    fn the_extended_bit_becomes_a_flag_and_not_part_of_the_code() {
        // Right Control is 0x11D and left Control is 0x1D. Masking before reading the bit off would
        // make them the same key, which is exactly the bug the contract was ambiguous enough to
        // invite.
        let FastPathInputEvent::KeyboardEvent(flags, code) =
            convert(&key(0x11D, true)).expect("a scan code")
        else {
            panic!("expected a keyboard event");
        };
        assert_eq!(code, 0x1D);
        assert!(flags.contains(KeyboardFlags::EXTENDED));

        let FastPathInputEvent::KeyboardEvent(flags, code) =
            convert(&key(0x1D, true)).expect("a scan code")
        else {
            panic!("expected a keyboard event");
        };
        assert_eq!(code, 0x1D);
        assert!(!flags.contains(KeyboardFlags::EXTENDED));
    }

    #[test]
    fn a_number_that_is_not_a_scan_code_is_refused_rather_than_truncated() {
        // A HID usage or a platform key code would land here. Masking it down produces a valid-looking
        // scan code for an entirely different key, which is a bug that types the wrong letter.
        assert_eq!(
            convert(&key(0x2000, true)),
            Err(Unsendable::Scancode { scancode: 0x2000 })
        );
    }

    #[test]
    fn a_click_carries_its_own_position() {
        // One event, not a move followed by a press: the two could otherwise be reordered and the
        // click would land where the pointer used to be.
        let event = InputEvent::PointerButton {
            button: PointerButton::Left,
            pressed: true,
            x: 640,
            y: 480,
        };
        let FastPathInputEvent::MouseEvent(pdu) = convert(&event).expect("a mouse event") else {
            panic!("expected a mouse event");
        };
        assert_eq!((pdu.x_position, pdu.y_position), (640, 480));
        assert!(pdu.flags.contains(PointerFlags::LEFT_BUTTON));
        assert!(pdu.flags.contains(PointerFlags::DOWN));
    }

    #[test]
    fn a_release_is_the_same_button_without_down() {
        let event = InputEvent::PointerButton {
            button: PointerButton::Right,
            pressed: false,
            x: 1,
            y: 2,
        };
        let FastPathInputEvent::MouseEvent(pdu) = convert(&event).expect("a mouse event") else {
            panic!("expected a mouse event");
        };
        assert!(pdu.flags.contains(PointerFlags::RIGHT_BUTTON));
        assert!(
            !pdu.flags.contains(PointerFlags::DOWN),
            "a button that never comes up is a button that stays pressed"
        );
    }

    #[test]
    fn the_extra_buttons_ride_their_own_pdu() {
        for (button, expected) in [
            (PointerButton::X1, PointerXFlags::BUTTON1),
            (PointerButton::X2, PointerXFlags::BUTTON2),
        ] {
            let event = InputEvent::PointerButton {
                button,
                pressed: true,
                x: 3,
                y: 4,
            };
            let FastPathInputEvent::MouseEventEx(pdu) = convert(&event).expect("a mouse event")
            else {
                panic!("buttons four and five are not ordinary mouse events");
            };
            assert!(pdu.flags.contains(expected));
            assert!(pdu.flags.contains(PointerXFlags::DOWN));
        }
    }

    #[test]
    fn a_scroll_picks_one_axis() {
        // RDP carries one axis per PDU. A diagonal gesture split across two arrives as two scrolls in
        // sequence, which reads as a stutter.
        let FastPathInputEvent::MouseEvent(pdu) =
            convert(&InputEvent::Scroll { dx: 0.2, dy: -3.0 }).expect("a mouse event")
        else {
            panic!("expected a mouse event");
        };
        assert!(pdu.flags.contains(PointerFlags::VERTICAL_WHEEL));
        assert_eq!(pdu.number_of_wheel_rotation_units, -360);

        let FastPathInputEvent::MouseEvent(pdu) =
            convert(&InputEvent::Scroll { dx: 2.0, dy: 0.5 }).expect("a mouse event")
        else {
            panic!("expected a mouse event");
        };
        assert!(pdu.flags.contains(PointerFlags::HORIZONTAL_WHEEL));
        assert_eq!(pdu.number_of_wheel_rotation_units, 240);
    }

    #[test]
    fn an_enormous_scroll_does_not_wrap_into_the_other_direction() {
        let FastPathInputEvent::MouseEvent(pdu) =
            convert(&InputEvent::Scroll { dx: 0.0, dy: 1e9 }).expect("a mouse event")
        else {
            panic!("expected a mouse event");
        };
        assert_eq!(pdu.number_of_wheel_rotation_units, i16::MAX);
    }

    #[test]
    fn a_coordinate_off_the_desktop_lands_at_the_edge_and_not_the_far_side() {
        let FastPathInputEvent::MouseEvent(pdu) =
            convert(&InputEvent::PointerMove { x: 999_999, y: 5 }).expect("a mouse event")
        else {
            panic!("expected a mouse event");
        };
        assert_eq!((pdu.x_position, pdu.y_position), (u16::MAX, 5));
    }

    #[test]
    fn what_cannot_be_sent_says_which_it_was() {
        assert_eq!(
            convert(&InputEvent::Text("привет".to_string())),
            Err(Unsendable::Text)
        );
        assert_eq!(
            convert(&InputEvent::ClipboardProvide("x".to_string())),
            Err(Unsendable::Clipboard)
        );
        // And the two read differently, because they are fixed by different work.
        assert_ne!(
            Unsendable::Text.to_string(),
            Unsendable::Clipboard.to_string()
        );
    }
}
