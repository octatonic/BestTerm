//! The `.mxtsessions` importer, against a file a real person actually uses.
//!
//! Every other test here feeds the importer a file this project wrote, which proves the parser agrees
//! with our idea of the format and nothing more. A file exported by the real application has all the
//! things an invented one does not: folders nested deeper than anyone would type by hand, protocols
//! nobody thought to cover, settings from versions that came and went, and the accumulated debris of
//! years of use.
//!
//! # Running it
//!
//! Skips itself unless pointed at a file, so `cargo test` stays green on a machine that has none:
//!
//! ```sh
//! BESTTERM_MXTSESSIONS="/path/to/MobaXterm Sessions.mxtsessions" cargo test -p bestterm-importers
//! ```
//!
//! # What it deliberately does not do
//!
//! It asserts on shape and counts, never on content. Somebody's session file is an inventory of their
//! infrastructure, and a test that printed host names would put that inventory into whatever log the
//! test output lands in. Nothing below prints a host, a user name or a secret.

use std::collections::BTreeMap;

use bestterm_core_model::NodeKind;
use bestterm_importers::mxtsessions;

/// The file to read, or `None` to skip.
fn real_file() -> Option<Vec<u8>> {
    let path = std::env::var_os("BESTTERM_MXTSESSIONS")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            eprintln!("BESTTERM_MXTSESSIONS is set but unreadable: {error}");
            None
        }
    }
}

#[test]
fn a_real_session_file_imports_without_losing_most_of_itself() {
    let Some(bytes) = real_file() else {
        eprintln!("BESTTERM_MXTSESSIONS is not set; skipping");
        return;
    };

    let import = mxtsessions::parse(&bytes);
    let ids = import.tree.walk();
    let nodes = ids.len();

    // Counted by protocol, because "it imported 200 sessions" hides the case where every SSH session
    // arrived and every RDP one was dropped.
    let mut by_protocol: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut sessions = 0;
    for id in &ids {
        let Some(node) = import.tree.get(*id) else {
            continue;
        };
        if let NodeKind::Session { config } = &node.kind {
            sessions += 1;
            *by_protocol.entry(config.protocol().id()).or_default() += 1;
        }
    }
    let folders = nodes - sessions;

    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    for skipped in &import.skipped {
        *by_reason
            .entry(format!("{:?}", skipped.reason))
            .or_default() += 1;
    }

    println!("nodes      {nodes}");
    println!("folders    {folders}");
    println!("sessions   {sessions}");
    println!("by protocol {by_protocol:?}");
    println!("secrets    {}", import.secrets.len());
    println!("notes      {}", import.notes.len());
    println!("skipped    {} {by_reason:?}", import.skipped.len());

    assert!(sessions > 0, "a real file yielded no sessions at all");

    // The bar is deliberately low and the point is the number in the message: a file where a third of
    // the entries are dropped is a file this importer does not understand, and that is worth failing
    // over rather than noting.
    let attempted = sessions + import.skipped.len();
    let kept = (sessions * 100) / attempted.max(1);
    println!("kept       {kept}% of {attempted}");
    assert!(
        kept >= 70,
        "only {kept}% of {attempted} entries survived; reasons: {by_reason:?}"
    );
}

#[test]
fn every_imported_session_keeps_a_name_and_a_home() {
    // A session with no name cannot be shown, and one with no folder cannot be found. Both are the
    // kind of thing a hand-written fixture never produces.
    let Some(bytes) = real_file() else {
        eprintln!("BESTTERM_MXTSESSIONS is not set; skipping");
        return;
    };

    let import = mxtsessions::parse(&bytes);
    let mut nameless = 0;
    let mut orphaned = 0;

    for id in import.tree.walk() {
        let Some(node) = import.tree.get(id) else {
            continue;
        };
        if node.name.trim().is_empty() {
            nameless += 1;
        }
        // A session at the top level legitimately has no parent; counted, not judged.
        if !node.kind.is_folder() && node.parent.is_none() {
            orphaned += 1;
        }
    }

    println!("nameless {nameless}  top-level sessions {orphaned}");
    assert_eq!(nameless, 0, "{nameless} imported nodes have no name");
}

#[test]
fn a_real_file_produces_no_duplicate_ids() {
    // Identity is what the tree is indexed by. Two nodes sharing an id would make one of them
    // unreachable, and a real file is where a collision would first show up.
    let Some(bytes) = real_file() else {
        eprintln!("BESTTERM_MXTSESSIONS is not set; skipping");
        return;
    };

    let import = mxtsessions::parse(&bytes);
    let mut ids = import.tree.walk();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "the tree contains duplicate node ids");
}
