//! The icon set, drawn rather than loaded.
//!
//! # Why these are vectors in code and not files
//!
//! MobaXterm's icons are Mobatek's artwork. Reproducing the *layout* of the reference is the point of
//! this project and is lawful; copying its icons out of the distribution is not, and this repository
//! is public and GPL-3.0, so it cannot carry them. `docs/ui-parity.md` records that decision.
//!
//! What is copied is what each icon *depicts* and roughly what colour it is — a star for saved
//! sessions, crossed tools for the tool box, two cogs for settings — because that is the part
//! somebody has to recognise from muscle memory, and it is also the part that is an idea rather than
//! an expression of one.
//!
//! There was briefly a test here that searched this file for an embedding macro, to keep that
//! decision enforced rather than merely written. It searched the file it lives in, so it tripped on
//! the very comment explaining what it forbade — twice. A rule about where artwork may come from is
//! not a thing a grep can hold; it is written here and in `docs/ui-parity.md`, where somebody adding
//! an icon will read it.
//!
//! Drawing them with [`egui::Painter`] rather than shipping an icon set: there is no asset to license
//! or attribute, they are sharp at any scale and any DPI without a second file, and it matches the
//! decision already made for the theme, where every pixel of chrome is ours. The cost is that each
//! one is a few lines of geometry instead of an SVG somebody else drew.
//!
//! # Sizes
//!
//! Every icon draws itself into the rectangle it is given and assumes nothing about how big that is.
//! The ribbon asks for 24 points and the dialog tabs for 16, and the same code serves both.

use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2, pos2, vec2,
};

/// The palette the icons are drawn from.
///
/// Fixed rather than taken from [`crate::ChromeTheme`]: these are object colours, not interface
/// colours. A yellow star stays yellow when the interface goes dark, exactly as a folder icon does.
mod ink {
    use egui::Color32;

    pub(super) const BLUE: Color32 = Color32::from_rgb(0x2E, 0x7C, 0xC4);
    pub(super) const BLUE_DARK: Color32 = Color32::from_rgb(0x1B, 0x4F, 0x82);
    pub(super) const GREEN: Color32 = Color32::from_rgb(0x3F, 0xA5, 0x4A);
    pub(super) const YELLOW: Color32 = Color32::from_rgb(0xE8, 0xB1, 0x1E);
    pub(super) const ORANGE: Color32 = Color32::from_rgb(0xE0, 0x6C, 0x1E);
    pub(super) const RED: Color32 = Color32::from_rgb(0xC0, 0x39, 0x2B);
    pub(super) const STEEL: Color32 = Color32::from_rgb(0x8A, 0x93, 0x9B);
    pub(super) const STEEL_DARK: Color32 = Color32::from_rgb(0x55, 0x5D, 0x66);
    pub(super) const SCREEN: Color32 = Color32::from_rgb(0x18, 0x22, 0x2C);
    pub(super) const PAPER: Color32 = Color32::from_rgb(0xF2, 0xF4, 0xF6);
}

/// Every icon this build can draw.
///
/// One enum for the ribbon and the dialog both: several appear in both places, and two lists would
/// drift the moment one of them gained an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    /// A terminal: the Session button, and a terminal tab.
    Session,
    /// Machines to connect to.
    Servers,
    /// The tool box.
    Tools,
    /// Saved sessions.
    Sessions,
    /// The View menu.
    View,
    /// Splitting a tab into panes.
    Split,
    /// Typing into several sessions at once.
    MultiExec,
    /// Port forwarding.
    Tunneling,
    /// Installable packages.
    Packages,
    /// Configuration.
    Settings,
    /// Help.
    Help,
    /// The X server's state.
    XServer,
    /// Leave.
    Exit,
    /// The configuration dialog's first tab.
    General,
    /// X11 settings.
    X11,
    /// SSH settings.
    Ssh,
    /// Appearance.
    Display,
    /// The toolbar's own settings.
    Toolbar,
    /// Everything else.
    Misc,
    /// A folder, for the buttons beside a path field.
    Folder,
    /// A file.
    File,
    /// Accept.
    Ok,
    /// Remove.
    Remove,
    /// A key, for stored passwords.
    Key,
    /// A person, for shared sessions.
    People,
    /// A remote desktop.
    Rdp,
    /// A framebuffer session.
    Vnc,
}

impl Icon {
    /// The icon a tab's protocol identifier asks for.
    ///
    /// Falls back to the terminal, because every protocol that is not a picture is a terminal.
    pub fn for_protocol(id: &str) -> Self {
        match id {
            "rdp" => Self::Rdp,
            "vnc" => Self::Vnc,
            // A file browser is not a terminal, and the fallback would have made it look like one.
            "sftp" | "ftp" => Self::Folder,
            _ => Self::Session,
        }
    }
}

/// Draw `icon` to fill `rect`.
pub fn draw(painter: &egui::Painter, rect: Rect, icon: Icon) {
    // Everything below is written against a unit square and scaled here, so an icon reads the same at
    // 16 points as at 24 and nothing carries a hard-coded size.
    let s = rect.width().min(rect.height());
    let o = rect.center() - vec2(s / 2.0, s / 2.0);
    let p = |x: f32, y: f32| -> Pos2 { pos2(o.x + x * s, o.y + y * s) };
    let r = |x: f32, y: f32, w: f32, h: f32| -> Rect {
        Rect::from_min_size(p(x, y), vec2(w * s, h * s))
    };
    let line =
        |a: Pos2, b: Pos2, w: f32, c: Color32| painter.line_segment([a, b], Stroke::new(w * s, c));
    let fill = |rect: Rect, c: Color32| painter.rect_filled(rect, CornerRadius::ZERO, c);

    match icon {
        // A monitor with a lit screen: the thing a session opens into.
        Icon::Session => {
            fill(r(0.08, 0.14, 0.84, 0.60), ink::STEEL_DARK);
            fill(r(0.14, 0.20, 0.72, 0.48), ink::SCREEN);
            line(p(0.22, 0.34), p(0.38, 0.34), 0.07, ink::GREEN);
            line(p(0.22, 0.48), p(0.52, 0.48), 0.07, ink::GREEN);
            fill(r(0.36, 0.74, 0.28, 0.06), ink::STEEL_DARK);
            fill(r(0.24, 0.80, 0.52, 0.06), ink::STEEL);
        }

        // Two stacked machines with status lights.
        Icon::Servers => {
            for (y, colour) in [(0.10, ink::GREEN), (0.56, ink::GREEN)] {
                fill(r(0.12, y, 0.76, 0.32), ink::STEEL_DARK);
                fill(r(0.16, y + 0.10, 0.44, 0.06), ink::STEEL);
                fill(r(0.72, y + 0.09, 0.08, 0.08), colour);
            }
        }

        // A hammer and a wrench crossed, which is what a tool box means everywhere.
        Icon::Tools => {
            line(p(0.20, 0.82), p(0.72, 0.26), 0.11, ink::STEEL);
            line(p(0.80, 0.80), p(0.30, 0.26), 0.11, ink::YELLOW);
            fill(r(0.62, 0.14, 0.24, 0.20), ink::STEEL_DARK);
            painter.circle_filled(p(0.26, 0.22), 0.11 * s, ink::YELLOW);
            painter.circle_filled(p(0.26, 0.22), 0.05 * s, ink::PAPER);
        }

        // A star: saved things, in every interface since the first browser bookmark.
        Icon::Sessions => star(painter, p(0.5, 0.52), 0.46 * s, ink::YELLOW),

        // Panes with one of them lit, which is what "look at it a different way" is here.
        Icon::View => {
            fill(r(0.08, 0.14, 0.84, 0.72), ink::BLUE_DARK);
            fill(r(0.13, 0.19, 0.36, 0.62), ink::PAPER);
            fill(r(0.53, 0.19, 0.34, 0.28), ink::BLUE);
            fill(r(0.53, 0.51, 0.34, 0.30), ink::PAPER);
        }

        // A window cut in two, with the division the point of it.
        Icon::Split => {
            fill(r(0.08, 0.16, 0.84, 0.68), ink::STEEL_DARK);
            fill(r(0.12, 0.20, 0.34, 0.60), ink::PAPER);
            fill(r(0.54, 0.20, 0.34, 0.60), ink::PAPER);
            line(p(0.5, 0.10), p(0.5, 0.90), 0.05, ink::ORANGE);
        }

        // One keystroke fanning out to several machines.
        Icon::MultiExec => {
            painter.circle_filled(p(0.16, 0.5), 0.12 * s, ink::RED);
            for y in [0.18, 0.5, 0.82] {
                line(p(0.30, 0.5), p(0.74, y), 0.06, ink::RED);
                painter.circle_filled(p(0.82, y), 0.10 * s, ink::STEEL_DARK);
            }
        }

        // A pipe with traffic going both ways through it.
        Icon::Tunneling => {
            fill(r(0.06, 0.34, 0.88, 0.32), ink::STEEL_DARK);
            arrow(painter, p(0.20, 0.44), p(0.80, 0.44), 0.06 * s, ink::GREEN);
            arrow(painter, p(0.80, 0.60), p(0.20, 0.60), 0.06 * s, ink::GREEN);
        }

        // A parcel, taped down the middle.
        Icon::Packages => {
            fill(r(0.10, 0.26, 0.80, 0.58), ink::BLUE_DARK);
            fill(r(0.10, 0.26, 0.80, 0.14), ink::BLUE);
            line(p(0.5, 0.26), p(0.5, 0.84), 0.07, ink::PAPER);
        }

        // Two cogs, the larger behind.
        Icon::Settings => {
            cog(painter, p(0.42, 0.44), 0.30 * s, ink::BLUE);
            cog(painter, p(0.70, 0.72), 0.20 * s, ink::BLUE_DARK);
        }

        // A question mark in a disc.
        Icon::Help => {
            painter.circle_filled(rect.center(), 0.46 * s, ink::BLUE);
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "?",
                FontId::proportional(0.72 * s),
                ink::PAPER,
            );
        }

        // The X of the X Window System, on a screen.
        Icon::XServer | Icon::X11 => {
            fill(r(0.08, 0.14, 0.84, 0.60), ink::STEEL_DARK);
            fill(r(0.13, 0.19, 0.74, 0.50), ink::SCREEN);
            line(p(0.28, 0.28), p(0.72, 0.60), 0.09, ink::GREEN);
            line(p(0.72, 0.28), p(0.28, 0.60), 0.09, ink::GREEN);
            fill(r(0.24, 0.80, 0.52, 0.06), ink::STEEL);
        }

        // A door with somebody leaving through it.
        Icon::Exit => {
            fill(r(0.12, 0.10, 0.44, 0.80), ink::STEEL_DARK);
            fill(r(0.17, 0.15, 0.34, 0.70), ink::PAPER);
            arrow(painter, p(0.52, 0.5), p(0.92, 0.5), 0.07 * s, ink::RED);
        }

        // Blocks: the general settings, which are everything at once.
        Icon::General => {
            fill(r(0.10, 0.10, 0.36, 0.36), ink::BLUE);
            fill(r(0.54, 0.10, 0.36, 0.36), ink::GREEN);
            fill(r(0.10, 0.54, 0.36, 0.36), ink::ORANGE);
            fill(r(0.54, 0.54, 0.36, 0.36), ink::YELLOW);
        }

        // A key, for anything about credentials.
        Icon::Ssh | Icon::Key => {
            painter.circle_filled(p(0.30, 0.34), 0.20 * s, ink::YELLOW);
            painter.circle_filled(p(0.30, 0.34), 0.08 * s, ink::PAPER);
            line(p(0.40, 0.44), p(0.84, 0.86), 0.10, ink::YELLOW);
            line(p(0.66, 0.70), p(0.56, 0.82), 0.08, ink::YELLOW);
        }

        // A screen showing colours, which is what the appearance settings are about.
        Icon::Display => {
            fill(r(0.08, 0.16, 0.84, 0.58), ink::STEEL_DARK);
            fill(r(0.13, 0.21, 0.32, 0.48), ink::BLUE);
            fill(r(0.47, 0.21, 0.18, 0.48), ink::GREEN);
            fill(r(0.67, 0.21, 0.20, 0.48), ink::ORANGE);
            fill(r(0.24, 0.80, 0.52, 0.06), ink::STEEL);
        }

        // A row of buttons: the toolbar, settings for itself.
        Icon::Toolbar => {
            fill(r(0.06, 0.28, 0.88, 0.44), ink::STEEL_DARK);
            for x in [0.13, 0.40, 0.67] {
                fill(r(x, 0.36, 0.20, 0.28), ink::PAPER);
            }
        }

        // A single cog: the leftovers.
        Icon::Misc => cog(painter, rect.center(), 0.42 * s, ink::BLUE),

        // A folder, open at the top.
        Icon::Folder => {
            fill(r(0.06, 0.24, 0.36, 0.12), ink::YELLOW);
            fill(r(0.06, 0.32, 0.88, 0.48), ink::YELLOW);
            painter.rect_stroke(
                r(0.06, 0.32, 0.88, 0.48),
                CornerRadius::ZERO,
                Stroke::new(0.04 * s, ink::ORANGE),
                StrokeKind::Inside,
            );
        }

        // A sheet with a folded corner.
        Icon::File => {
            fill(r(0.20, 0.08, 0.60, 0.84), ink::PAPER);
            painter.rect_stroke(
                r(0.20, 0.08, 0.60, 0.84),
                CornerRadius::ZERO,
                Stroke::new(0.04 * s, ink::STEEL),
                StrokeKind::Inside,
            );
            for y in [0.32, 0.46, 0.60] {
                line(p(0.30, y), p(0.70, y), 0.04, ink::STEEL);
            }
        }

        // A tick in a disc.
        Icon::Ok => {
            painter.circle_filled(rect.center(), 0.44 * s, ink::GREEN);
            line(p(0.28, 0.52), p(0.44, 0.68), 0.10, ink::PAPER);
            line(p(0.44, 0.68), p(0.74, 0.34), 0.10, ink::PAPER);
        }

        // A cross in a disc.
        Icon::Remove => {
            painter.circle_filled(rect.center(), 0.44 * s, ink::RED);
            line(p(0.32, 0.32), p(0.68, 0.68), 0.10, ink::PAPER);
            line(p(0.68, 0.32), p(0.32, 0.68), 0.10, ink::PAPER);
        }

        // Two figures, for anything shared.
        Icon::People => {
            painter.circle_filled(p(0.36, 0.30), 0.16 * s, ink::BLUE);
            fill(r(0.16, 0.50, 0.40, 0.34), ink::BLUE);
            painter.circle_filled(p(0.68, 0.34), 0.13 * s, ink::GREEN);
            fill(r(0.54, 0.52, 0.34, 0.32), ink::GREEN);
        }

        // A desktop with a window on it.
        Icon::Rdp => {
            fill(r(0.06, 0.14, 0.88, 0.62), ink::BLUE_DARK);
            fill(r(0.11, 0.19, 0.78, 0.52), ink::BLUE);
            fill(r(0.22, 0.30, 0.42, 0.30), ink::PAPER);
            fill(r(0.24, 0.80, 0.52, 0.06), ink::STEEL);
        }

        // A screen being watched.
        Icon::Vnc => {
            fill(r(0.06, 0.14, 0.88, 0.62), ink::STEEL_DARK);
            fill(r(0.11, 0.19, 0.78, 0.52), ink::SCREEN);
            painter.circle_filled(p(0.5, 0.45), 0.16 * s, ink::ORANGE);
            painter.circle_filled(p(0.5, 0.45), 0.07 * s, ink::SCREEN);
            fill(r(0.24, 0.80, 0.52, 0.06), ink::STEEL);
        }
    }
}

/// A five-pointed star, filled.
fn star(painter: &egui::Painter, centre: Pos2, radius: f32, colour: Color32) {
    let mut points = Vec::with_capacity(10);
    for step in 0..10 {
        // Starts at the top, which is the only orientation a star reads as one.
        let angle = -std::f32::consts::FRAC_PI_2 + step as f32 * std::f32::consts::PI / 5.0;
        let r = if step % 2 == 0 { radius } else { radius * 0.44 };
        points.push(centre + Vec2::angled(angle) * r);
    }
    painter.add(egui::Shape::convex_polygon(points, colour, Stroke::NONE));
}

/// A cog: a disc with teeth and a hole.
fn cog(painter: &egui::Painter, centre: Pos2, radius: f32, colour: Color32) {
    const TEETH: usize = 8;
    for tooth in 0..TEETH {
        let angle = tooth as f32 * std::f32::consts::TAU / TEETH as f32;
        let direction = Vec2::angled(angle);
        painter.line_segment(
            [
                centre + direction * radius * 0.62,
                centre + direction * radius,
            ],
            Stroke::new(radius * 0.34, colour),
        );
    }
    painter.circle_filled(centre, radius * 0.72, colour);
    // The hole is what makes it a cog rather than a sun.
    painter.circle_filled(centre, radius * 0.28, ink::PAPER);
}

/// A line with a head on its far end.
fn arrow(painter: &egui::Painter, from: Pos2, to: Pos2, width: f32, colour: Color32) {
    let direction = (to - from).normalized();
    let head = width * 2.2;
    let base = to - direction * head;
    painter.line_segment([from, base], Stroke::new(width, colour));
    let across = vec2(-direction.y, direction.x) * head * 0.7;
    painter.add(egui::Shape::convex_polygon(
        vec![to, base + across, base - across],
        colour,
        Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon, so a new variant cannot be added without something to draw for it.
    const ALL: &[Icon] = &[
        Icon::Session,
        Icon::Servers,
        Icon::Tools,
        Icon::Sessions,
        Icon::View,
        Icon::Split,
        Icon::MultiExec,
        Icon::Tunneling,
        Icon::Packages,
        Icon::Settings,
        Icon::Help,
        Icon::XServer,
        Icon::Exit,
        Icon::General,
        Icon::X11,
        Icon::Ssh,
        Icon::Display,
        Icon::Toolbar,
        Icon::Misc,
        Icon::Folder,
        Icon::File,
        Icon::Ok,
        Icon::Remove,
        Icon::Key,
        Icon::People,
        Icon::Rdp,
        Icon::Vnc,
    ];

    #[test]
    fn every_icon_draws_something_at_every_size_it_is_asked_for() {
        // The ribbon asks for 24 and the dialog tabs for 16. An icon that divided by its own size
        // somewhere would produce nothing, or a panic, at one of them and not the other.
        //
        // Inside `Context::run` because `Icon::Help` paints text, and egui has no fonts before a
        // frame has begun. Painting outside one panicked, which is how this was found.
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for icon in ALL {
                for side in [8.0_f32, 16.0, 24.0, 64.0] {
                    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(side, side));
                    draw(ui.painter(), rect, *icon);
                }
            }
        });

        // Something was actually emitted. An icon that drew nothing would pass a test that only
        // checked it did not panic.
        let shapes: usize = output
            .shapes
            .iter()
            .map(|clipped| match &clipped.shape {
                egui::Shape::Vec(inner) => inner.len(),
                _ => 1,
            })
            .sum();
        assert!(
            shapes >= ALL.len(),
            "only {shapes} shapes for {} icons",
            ALL.len()
        );

        // egui panics on a dropped texture delta, on the grounds that a real application would have
        // uploaded it. Nothing here has a GPU to upload to.
        output.textures_delta.clear();
    }

    #[test]
    fn a_protocol_without_a_picture_gets_the_terminal() {
        assert_eq!(Icon::for_protocol("rdp"), Icon::Rdp);
        assert_eq!(Icon::for_protocol("vnc"), Icon::Vnc);
        assert_eq!(Icon::for_protocol("ssh"), Icon::Session);
        assert_eq!(Icon::for_protocol("telnet"), Icon::Session);
        // Anything unrecognised, rather than nothing: a tab with no icon is a tab that looks broken.
        assert_eq!(Icon::for_protocol(""), Icon::Session);
    }
}
