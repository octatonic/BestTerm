//! Importers for other clients' session formats.
//!
//! The point of this crate is migration: someone with four hundred sessions in another tool will not
//! retype them, so the fastest route to being useful is to read what they already have.
//!
//! # Rules every importer here follows
//!
//! * **Never fail as a whole.** One session using a protocol BestTerm does not support yet must not
//!   cost the user the other three hundred and ninety-nine. Everything unreadable is reported, with a
//!   reason, in [`Import::skipped`].
//! * **Never guess.** Where a format's specification does not say what a value means, the session is
//!   skipped and the raw value reported. A silently wrong import is worse than a visible gap: a
//!   serial console imported as a telnet host looks fine right up until someone connects.
//! * **Move secrets into the vault.** Formats that keep passwords in clear text hand them back as
//!   [`ImportedSecret`]s rather than writing them into the session tree, so importing *removes* a
//!   plaintext password from the user's configuration instead of copying it.
//!
//! # Example
//!
//! ```
//! use bestterm_importers::mxtsessions;
//!
//! let file = b"[Bookmarks]\r\nSubRep=Production\r\n\
//!              web=#109#0%web-1.int%22%deploy%%-1%-1#MobaFont%10#0##-1\r\n";
//!
//! let import = mxtsessions::parse(file);
//! assert_eq!(import.imported_sessions(), 1);
//! assert!(import.skipped.is_empty());
//! ```

pub mod mxtsessions;

mod cp1252;

pub use mxtsessions::{Import, ImportedSecret, SkipReason, Skipped};
