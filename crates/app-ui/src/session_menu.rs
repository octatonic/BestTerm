//! The context menu on a saved session, and what it asks the application to do.
//!
//! Measured from a screenshot of MobaXterm Professional 26.4.0.5512's right-click menu on a session,
//! in its order and with its separators:
//!
//! ```text
//!   Execute
//!   Connect as...
//!   Ping host
//!   ----
//!   Rename session
//!   Edit session
//!   Delete session
//!   Duplicate session
//!   Save session to file
//!   Create a desktop shortcut
//!   ----
//!   Save session settings as default presets
//!   Copy session settings
//! ```
//!
//! # Why the whole list is here when half of it does nothing
//!
//! Because the list *is* the measurement, and an entry missing from it is a thing nobody notices is
//! missing. Every item is drawn; the ones without behaviour report themselves through
//! [`SessionAction::Unimplemented`], which the application turns into a message naming the item. That
//! is the same decision `ui-chrome` made for the menu bar, for the same reason: a menu with items
//! quietly absent looks finished and is not.
//!
//! # A folder is not a session
//!
//! MobaXterm's menu on a folder is a different, shorter list. It has not been measured, so a folder
//! gets the three items that are unambiguously about a folder and nothing invented around them.

/// What the menu asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionAction {
    /// Open it, as it is saved.
    Execute,
    /// Open it, asking for a different user first.
    ConnectAs,
    /// Reachability, without opening anything.
    Ping,
    /// Change its name in the tree.
    Rename,
    /// Open the Session dialog on it.
    Edit,
    /// Remove it.
    Delete,
    /// Copy it beside itself.
    Duplicate,
    /// An item that is drawn and does nothing yet.
    ///
    /// Carries its own label, so the message names the thing that was clicked rather than saying
    /// "not implemented".
    Unimplemented(&'static str),
}

/// One row of the menu.
///
/// A separator is a variant rather than a flag, because the measured order includes where the
/// separators fall and a list of items with a separate list of positions is two things to keep in
/// step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    /// A clickable item.
    Item {
        /// What it says.
        label: &'static str,
        /// What it does.
        action: SessionAction,
    },
    /// A horizontal rule.
    Separator,
}

impl Row {
    /// A clickable row.
    const fn item(label: &'static str, action: SessionAction) -> Self {
        Self::Item { label, action }
    }
}

/// The menu on a session, in the measured order.
pub(crate) const SESSION_MENU: &[Row] = &[
    Row::item("Execute", SessionAction::Execute),
    Row::item("Connect as...", SessionAction::ConnectAs),
    Row::item("Ping host", SessionAction::Ping),
    Row::Separator,
    Row::item("Rename session", SessionAction::Rename),
    Row::item("Edit session", SessionAction::Edit),
    Row::item("Delete session", SessionAction::Delete),
    Row::item("Duplicate session", SessionAction::Duplicate),
    Row::item(
        "Save session to file",
        SessionAction::Unimplemented("Save session to file"),
    ),
    Row::item(
        "Create a desktop shortcut",
        SessionAction::Unimplemented("Create a desktop shortcut"),
    ),
    Row::Separator,
    Row::item(
        "Save session settings as default presets",
        SessionAction::Unimplemented("Save session settings as default presets"),
    ),
    Row::item(
        "Copy session settings",
        SessionAction::Unimplemented("Copy session settings"),
    ),
];

/// The menu on a folder.
///
/// Short, and short on purpose: the reference's folder menu has not been measured, so this is the
/// three items that are unambiguously about a folder rather than a guess at the rest.
pub(crate) const FOLDER_MENU: &[Row] = &[
    Row::item("Rename folder", SessionAction::Rename),
    Row::item("Delete folder", SessionAction::Delete),
];

/// Draw a menu and return what was clicked.
pub(crate) fn show(ui: &mut egui::Ui, rows: &[Row]) -> Option<SessionAction> {
    let mut chosen = None;
    for row in rows {
        match row {
            Row::Separator => {
                ui.separator();
            }
            Row::Item { label, action } => {
                if ui.button(*label).clicked() {
                    chosen = Some(*action);
                    ui.close();
                }
            }
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(rows: &[Row]) -> Vec<&'static str> {
        rows.iter()
            .filter_map(|row| match row {
                Row::Item { label, .. } => Some(*label),
                Row::Separator => None,
            })
            .collect()
    }

    #[test]
    fn the_session_menu_is_what_was_measured_in_its_order() {
        // From a screenshot of MobaXterm Professional 26.4.0.5512, not from memory. The order is part
        // of the measurement: somebody reaching for the fifth item down is reaching by position.
        assert_eq!(
            labels(SESSION_MENU),
            vec![
                "Execute",
                "Connect as...",
                "Ping host",
                "Rename session",
                "Edit session",
                "Delete session",
                "Duplicate session",
                "Save session to file",
                "Create a desktop shortcut",
                "Save session settings as default presets",
                "Copy session settings",
            ]
        );
    }

    #[test]
    fn the_separators_fall_where_the_reference_puts_them() {
        // Two, after "Ping host" and after "Create a desktop shortcut". They group the menu into
        // "connect", "manage" and "settings", which is why their positions are measured rather than
        // decorative.
        let positions: Vec<usize> = SESSION_MENU
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, Row::Separator))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(positions, vec![3, 10]);
    }

    #[test]
    fn every_item_that_does_nothing_says_which_item_it_was() {
        // "Not implemented" tells somebody nothing. The label travels with the action so the message
        // names the thing that was clicked.
        for row in SESSION_MENU.iter().chain(FOLDER_MENU) {
            if let Row::Item {
                label,
                action: SessionAction::Unimplemented(named),
            } = row
            {
                assert_eq!(label, named, "an item's message must name the item");
            }
        }
    }

    #[test]
    fn a_folder_gets_only_what_is_unambiguously_about_a_folder() {
        // The reference's folder menu is unmeasured. Inventing the rest would put items on screen that
        // the reference does not have, which is the failure mode `docs/ui-parity.md` exists to prevent.
        assert_eq!(labels(FOLDER_MENU), vec!["Rename folder", "Delete folder"]);
        assert!(
            !FOLDER_MENU.iter().any(|row| matches!(
                row,
                Row::Item {
                    action: SessionAction::Execute,
                    ..
                }
            )),
            "a folder is not something to execute"
        );
    }

    #[test]
    fn the_four_actions_with_behaviour_are_distinct_from_the_placeholders() {
        // The line between "this works" and "this is drawn": a placeholder that crept into the first
        // group would look like a working feature.
        let working: Vec<SessionAction> = SESSION_MENU
            .iter()
            .filter_map(|row| match row {
                Row::Item { action, .. } if !matches!(action, SessionAction::Unimplemented(_)) => {
                    Some(*action)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            working,
            vec![
                SessionAction::Execute,
                SessionAction::ConnectAs,
                SessionAction::Ping,
                SessionAction::Rename,
                SessionAction::Edit,
                SessionAction::Delete,
                SessionAction::Duplicate,
            ]
        );
    }
}
