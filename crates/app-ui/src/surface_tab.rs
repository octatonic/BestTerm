//! A tab showing a remote desktop.
//!
//! The terminal's counterpart: where [`crate::tab::TerminalTab`] owns a byte stream and an emulator,
//! this owns a frame stream and a texture. Everything protocol-specific is on the far side of
//! [`bestterm_surface::GraphicalSurface`], so nothing here knows whether it is looking at RDP or VNC.
//!
//! # Frames become a texture once per frame, not once per repaint
//!
//! egui redraws for its own reasons — a hover, a resize, a blinking cursor — and re-uploading an
//! unchanged desktop each time would spend a megabyte of bus bandwidth to draw the same picture. The
//! generation counter in [`bestterm_surface::FrameMeta`] is what makes that avoidable: it is compared
//! against the last one uploaded, and an unchanged frame is drawn from the texture already there.
//!
//! Damage rectangles are carried across the process boundary and are not used here yet. Uploading
//! only what changed needs a partial texture update, which egui exposes but which is worth doing when
//! there is something to measure it against; the counter above already removes the repeated work,
//! which is the larger of the two costs.
//!
//! # The picture is letterboxed, never stretched
//!
//! A remote desktop has the size the server agreed to. Fitting it to the pane by distorting it makes
//! text unreadable in a way that looks like a font problem, so it is scaled by one factor on both
//! axes and centred. Asking the server to match the pane is a separate thing, and advisory: plenty of
//! servers will not.

use bestterm_surface::{
    EventReceiver, FrameMeta, FrameSize, GraphicalSurface, InputEvent, Modifiers, PointerButton,
    SurfaceEvent, SurfaceKind,
};

use crate::keymap;

/// A question about the server's identity, waiting for an answer.
#[derive(Clone, Debug)]
pub(crate) struct ServerKeyQuestion {
    /// Which server.
    pub(crate) host: String,
    /// And on which port.
    pub(crate) port: u16,
    /// What it presented, in the form meant to be compared aloud.
    pub(crate) fingerprint: String,
    /// What was on record, when something was and it did not match.
    pub(crate) expected: Option<String>,
}

/// A tab showing a remote desktop.
pub(crate) struct SurfaceTab {
    /// The connection, whatever protocol is behind it.
    surface: Box<dyn GraphicalSurface>,
    /// What it reports.
    events: EventReceiver<SurfaceEvent>,
    /// The most recent frame, uploaded.
    texture: Option<egui::TextureHandle>,
    /// Which generation `texture` holds, so an unchanged frame is not uploaded again.
    uploaded: u64,
    /// The desktop's size, as the server last agreed it.
    size: FrameSize,
    /// What the tab is called.
    title: String,
    /// Why the session ended, once it has.
    closed: Option<Option<String>>,
    /// A question about the server's key, if one is outstanding.
    pub(crate) question: Option<ServerKeyQuestion>,
    /// The key this session settled on, for whoever owns the store.
    pub(crate) settled_key: Option<(String, bool)>,
    /// Problems the session survived, newest last.
    pub(crate) notices: Vec<String>,
    /// The pane size the server was last asked to match.
    ///
    /// Kept so a window drag does not ask once per frame: every request makes the server re-run
    /// capability exchange, and a drag produces sixty of them a second.
    asked_for: Option<FrameSize>,
}

impl std::fmt::Debug for SurfaceTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceTab")
            .field("title", &self.title)
            .field("size", &self.size)
            .field("generation", &self.uploaded)
            .finish_non_exhaustive()
    }
}

impl SurfaceTab {
    /// Take over a surface somebody else opened.
    /// Waking the interface is the surface's own job, done on the thread that reads the helper —
    /// see [`bestterm_helper_surface`]. This only drains what is already there.
    pub(crate) fn adopt(
        surface: Box<dyn GraphicalSurface>,
        events: EventReceiver<SurfaceEvent>,
        title: String,
    ) -> Self {
        Self {
            surface,
            events,
            texture: None,
            uploaded: 0,
            // Replaced by the first frame. Nothing is drawn before then.
            size: FrameSize::new(1, 1),
            title,
            closed: None,
            question: None,
            settled_key: None,
            notices: Vec::new(),
            asked_for: None,
        }
    }

    /// Which protocol this is.
    pub(crate) fn kind(&self) -> SurfaceKind {
        self.surface.kind()
    }

    /// What to put on the tab.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// One line for the status bar.
    pub(crate) fn status_line(&self) -> String {
        match &self.closed {
            Some(Some(reason)) => format!("{} — closed ({reason})", self.title),
            Some(None) => format!("{} — closed", self.title),
            None => format!(
                "{} {} — {}×{}",
                self.surface.kind(),
                self.title,
                self.size.width,
                self.size.height
            ),
        }
    }

    /// Whether the session has ended.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.is_some()
    }

    /// Take whatever the surface has reported. Returns true if anything changed.
    pub(crate) fn pump(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let mut latest: Option<FrameMeta> = None;

        while let Ok(event) = self.events.try_recv() {
            changed = true;
            match event {
                // Only the newest is worth uploading: an older frame's pixels are already gone from
                // the shared mapping, replaced by the newer one.
                SurfaceEvent::Frame(meta) => latest = Some(meta),
                SurfaceEvent::Resized(size) => {
                    self.size = size;
                    // The texture describes the old size. Dropped rather than kept, so a frame is
                    // never drawn stretched across a resize it predates.
                    self.texture = None;
                    self.uploaded = 0;
                }
                SurfaceEvent::Cursor(_) => {}
                SurfaceEvent::ClipboardOffer(text) => {
                    tracing::debug!(bytes = text.len(), "the remote end offered its clipboard");
                }
                SurfaceEvent::AskAboutServerKey {
                    host,
                    port,
                    fingerprint,
                    expected,
                } => {
                    self.question = Some(ServerKeyQuestion {
                        host,
                        port,
                        fingerprint,
                        expected,
                    });
                }
                SurfaceEvent::ServerKeySettled { fingerprint, store } => {
                    self.settled_key = Some((fingerprint, store));
                }
                SurfaceEvent::Error(detail) => self.notices.push(detail),
                SurfaceEvent::Closed { reason } => {
                    self.closed = Some(reason);
                    // Any question is moot: nothing is waiting for the answer.
                    self.question = None;
                }
            }
        }

        if let Some(meta) = latest {
            self.upload(ctx, &meta);
        }
        changed
    }

    /// Copy the newest frame into a texture.
    fn upload(&mut self, ctx: &egui::Context, meta: &FrameMeta) {
        if meta.generation == self.uploaded {
            return;
        }

        let (width, height) = (meta.size.width as usize, meta.size.height as usize);
        let stride = meta.stride as usize;
        let mut image =
            egui::ColorImage::new([width, height], vec![egui::Color32::BLACK; width * height]);
        let mut complete = false;

        self.surface.with_frame(&mut |actual, pixels| {
            // The metadata came down a channel and the pixels came out of shared memory, so they can
            // disagree if a resize landed between the two. Drawing on that disagreement is how a
            // frame gets read past its end.
            if actual.generation != meta.generation || actual.size != meta.size {
                return;
            }
            let needed = stride.saturating_mul(height);
            if pixels.len() < needed {
                return;
            }

            let swap_red_and_blue = matches!(actual.format, bestterm_surface::PixelFormat::Bgra8);
            for y in 0..height {
                let row = &pixels[y * stride..][..width * 4];
                for x in 0..width {
                    let p = &row[x * 4..][..4];
                    let (r, g, b) = if swap_red_and_blue {
                        (p[2], p[1], p[0])
                    } else {
                        (p[0], p[1], p[2])
                    };
                    // Alpha is forced opaque. The helper decodes into a format that carries one, but
                    // a desktop is not transparent, and a server that leaves it at zero would
                    // otherwise produce an invisible picture over the pane's background.
                    image.pixels[y * width + x] = egui::Color32::from_rgb(r, g, b);
                }
            }
            complete = true;
        });

        if !complete {
            return;
        }

        self.size = meta.size;
        self.uploaded = meta.generation;
        match &mut self.texture {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture = Some(ctx.load_texture(
                    format!("surface-{}", self.title),
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
    }

    /// Draw the desktop into `ui`, and send back whatever was typed at it.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let Some(texture) = self.texture.clone() else {
            ui.centered_and_justified(|ui| match &self.closed {
                Some(Some(reason)) => {
                    ui.label(reason.as_str());
                }
                Some(None) => {
                    ui.label("The session closed.");
                }
                None => {
                    ui.spinner();
                }
            });
            return;
        };

        // One factor on both axes, so the picture keeps its shape. Never above 1.0: enlarging a
        // desktop is blurry, and asking the server for a bigger one is the right answer instead.
        let (width, height) = (
            self.size.width.max(1) as f32,
            self.size.height.max(1) as f32,
        );
        let scale = (available.x / width).min(available.y / height).min(1.0);
        let drawn = egui::vec2(width * scale, height * scale);

        let response = ui
            .centered_and_justified(|ui| {
                ui.add(
                    egui::Image::new(&texture)
                        .fit_to_exact_size(drawn)
                        .sense(egui::Sense::click_and_drag()),
                )
            })
            .inner;

        if self.is_closed() {
            return;
        }

        // The desktop is smaller than the space for it, so ask for a bigger one. Only ever upward:
        // shrinking on every layout wobble would make the picture jitter, and a desktop smaller than
        // its pane is letterboxed, which is fine to look at.
        let wanted = FrameSize::new(available.x.max(1.0) as u32, available.y.max(1.0) as u32);
        if wanted.width > self.size.width || wanted.height > self.size.height {
            self.request_resize(wanted);
        }

        // Where the picture ended up, so a click can be mapped back onto the desktop.
        let origin = response.rect.min;
        self.forward_input(ui, origin, scale, &response);
    }

    /// Turn what egui saw into surface input.
    fn forward_input(
        &mut self,
        ui: &egui::Ui,
        origin: egui::Pos2,
        scale: f32,
        response: &egui::Response,
    ) {
        let to_desktop = |position: egui::Pos2| -> (u32, u32) {
            let x = ((position.x - origin.x) / scale).max(0.0);
            let y = ((position.y - origin.y) / scale).max(0.0);
            (
                (x as u32).min(self.size.width.saturating_sub(1)),
                (y as u32).min(self.size.height.saturating_sub(1)),
            )
        };

        let mut send = Vec::new();

        if let Some(position) = response
            .hover_pos()
            .or_else(|| response.interact_pointer_pos())
        {
            let (x, y) = to_desktop(position);
            // Movement first, always. A button event carries its own position, but a server that
            // never saw the pointer arrive will not show a hover state, and hover states are half of
            // what a desktop looks like.
            send.push(InputEvent::PointerMove { x, y });

            for (button, egui_button) in [
                (PointerButton::Left, egui::PointerButton::Primary),
                (PointerButton::Right, egui::PointerButton::Secondary),
                (PointerButton::Middle, egui::PointerButton::Middle),
                (PointerButton::X1, egui::PointerButton::Extra1),
                (PointerButton::X2, egui::PointerButton::Extra2),
            ] {
                if ui.input(|i| i.pointer.button_pressed(egui_button)) {
                    send.push(InputEvent::PointerButton {
                        button,
                        pressed: true,
                        x,
                        y,
                    });
                }
                if ui.input(|i| i.pointer.button_released(egui_button)) {
                    send.push(InputEvent::PointerButton {
                        button,
                        pressed: false,
                        x,
                        y,
                    });
                }
            }
        }

        // Only while the pane has the keyboard, or every window in the application would type into
        // the remote desktop.
        if response.has_focus() || response.clicked() {
            response.request_focus();
            let events = ui.input(|i| i.events.clone());
            for event in events {
                match event {
                    egui::Event::Key {
                        key,
                        physical_key,
                        pressed,
                        repeat,
                        modifiers,
                    } => {
                        // Repeats are dropped: the remote host generates its own from the key being
                        // held, and forwarding ours as well doubles them.
                        if repeat {
                            continue;
                        }
                        // Physical first. See `keymap`: the remote host applies its own layout, so
                        // sending the logical key applies one twice.
                        let Some(scancode) = keymap::scancode(physical_key.unwrap_or(key)) else {
                            continue;
                        };
                        send.push(InputEvent::Key {
                            scancode,
                            pressed,
                            mods: Modifiers {
                                shift: modifiers.shift,
                                ctrl: modifiers.ctrl,
                                alt: modifiers.alt,
                                meta: modifiers.command && !modifiers.ctrl,
                            },
                        });
                    }
                    egui::Event::MouseWheel { delta, .. } => {
                        send.push(InputEvent::Scroll {
                            dx: delta.x,
                            dy: delta.y,
                        });
                    }
                    _ => {}
                }
            }
        }

        for event in send {
            if let Err(error) = self.surface.send_input(event) {
                // Once. A broken pipe reports itself on every event, and the close notification is
                // already on its way.
                if self.notices.last().map(String::as_str) != Some(error.to_string().as_str()) {
                    self.notices.push(error.to_string());
                }
                break;
            }
        }
    }

    /// Ask the server for a desktop the size of the pane.
    ///
    /// Advisory in the strongest sense: plenty of servers cannot resize, and the one that can may
    /// answer with a size close to but not equal to this. Whatever comes back arrives as
    /// [`SurfaceEvent::Resized`], and that is the size the tab believes.
    pub(crate) fn request_resize(&mut self, size: FrameSize) {
        if self.asked_for == Some(size) {
            return;
        }
        self.asked_for = Some(size);
        if let Err(error) = self.surface.request_resize(size) {
            tracing::debug!(%error, "the surface would not resize");
        }
    }

    /// Answer the outstanding question about the server's key.
    pub(crate) fn answer_server_key(&mut self, accept: bool) {
        self.question = None;
        if let Err(error) = self.surface.answer_server_key(accept) {
            self.notices.push(error.to_string());
        }
    }

    /// Close the session.
    pub(crate) fn shutdown(&mut self) {
        if let Err(error) = self.surface.shutdown() {
            tracing::debug!(%error, "shutting down the surface failed");
        }
    }
}
