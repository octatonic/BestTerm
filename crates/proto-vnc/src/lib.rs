//! VNC, which the specification calls RFB.
//!
//! Written by hand, like every other codec in this tree, rather than bound to `libvncclient`. The
//! plan allowed either; this way there is no C toolchain in the build on either platform, no GPL C
//! library to isolate, and the parts that are easy to get wrong are the parts with tests on them.
//!
//! # What is here
//!
//! * [`auth`] — the handshake's security step, which is DES with three famous mistakes in it.
//! * [`pixels`] — the pixel format, and asking the server for the one the framebuffer already wants.
//! * [`decode`] — Raw, CopyRect and ZRLE, and the framebuffer they write into.
//!
//! # What is not here yet
//!
//! Tight, which is what TigerVNC prefers and is a JPEG codec in a trench coat. ZRLE is universally
//! supported and enough to make a session usable; Tight is a bandwidth optimisation on top and is
//! listed in `docs/ROADMAP.md` rather than half-built here.
//!
//! Nothing in this crate opens a socket. The connection lives in the `bestterm-vnc` helper process,
//! for the same reasons RDP's does: a decoder fault costs a tab rather than the application.

pub mod auth;
pub mod decode;
pub mod pixels;
pub mod session;

pub use auth::Security;
pub use decode::{DecodeError, Framebuffer};
pub use pixels::PixelFormat;
pub use session::{Desktop, Update, VncError};
