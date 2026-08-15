//! The credential vault, as the interface sees it.
//!
//! # What is deliberately absent
//!
//! There is no "remember my master password" and no way to skip the prompt. The vault's whole value is
//! that a stolen configuration directory is useless without something the person knows, and an option
//! that keeps the master password anywhere would trade exactly that away. When the operating system's
//! keystore is wired up it will hold the *data key* rather than the password — a distinction
//! `crates/core-vault` was built around and one worth not eroding here.
//!
//! # Locked is a normal state
//!
//! Nothing forces an unlock at startup. The vault opens when something needs a secret out of it, which
//! means somebody who only uses agent authentication is never asked for a master password at all.

use bestterm_config::ConfigStore;
use bestterm_core_vault::{Secret, Vault};

/// What the interface is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Prompt {
    /// A vault exists; it needs the master password.
    Unlock,
    /// No vault exists; one has to be created before anything can be stored.
    Create,
}

/// The vault and the state of the prompt over it.
#[derive(Default)]
pub(crate) struct VaultState {
    /// Open when unlocked.
    vault: Option<Vault>,
    /// What is on screen, if anything.
    pub(crate) prompt: Option<Prompt>,
    /// The master password as it is being typed.
    ///
    /// A [`String`] rather than a `Secret` only because the text field needs `&mut String`; it is
    /// wrapped and cleared the moment it is used.
    pub(crate) typed: String,
    /// Repeat field, shown only when creating.
    pub(crate) repeated: String,
    /// What went wrong with the last attempt, shown under the field.
    pub(crate) error: Option<String>,
    /// Why the prompt was raised, so the work can continue once it is answered.
    pub(crate) pending: Option<PendingUnlock>,
}

/// What to do once the vault opens.
///
/// Only the *kind* of waiting work, not its details: the session that raised the prompt is held by the
/// application, which is the only thing that can resume it, and keeping the credential name here as
/// well would be two copies of one fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingUnlock {
    /// Somebody was opening a session that needs a stored password.
    Session,
}

impl VaultState {
    /// Whether the vault is open.
    pub(crate) fn is_open(&self) -> bool {
        self.vault.is_some()
    }

    /// Ask for the master password, choosing the right question for whether a vault exists.
    pub(crate) fn ask(&mut self, store: Option<&ConfigStore>, pending: Option<PendingUnlock>) {
        if self.is_open() {
            return;
        }
        let exists = store
            .and_then(|store| store.load_vault_file().ok())
            .flatten()
            .is_some();

        self.prompt = Some(if exists {
            Prompt::Unlock
        } else {
            Prompt::Create
        });
        self.typed.clear();
        self.repeated.clear();
        self.error = None;
        self.pending = pending;
    }

    /// Close the prompt without answering it.
    pub(crate) fn cancel(&mut self) {
        self.prompt = None;
        self.pending = None;
        self.wipe_typed();
    }

    /// Try the typed password.
    ///
    /// Returns what to do next when it worked, so the caller can resume whatever raised the prompt.
    pub(crate) fn submit(&mut self, store: Option<&ConfigStore>) -> Option<PendingUnlock> {
        let prompt = self.prompt?;

        // Checked before the password is used for anything, so a mistyped repeat never reaches the
        // key derivation.
        if prompt == Prompt::Create && self.typed != self.repeated {
            self.error = Some("the two passwords do not match".to_string());
            return None;
        }
        if self.typed.is_empty() {
            self.error = Some("a master password is required".to_string());
            return None;
        }

        let master = Secret::new(self.typed.clone());
        let result = match prompt {
            Prompt::Create => Vault::create(&master).map_err(|error| error.to_string()),
            Prompt::Unlock => match store
                .and_then(|store| store.load_vault_file().ok())
                .flatten()
            {
                Some(file) => Vault::unlock(file, &master).map_err(|error| {
                    // The library distinguishes a wrong password from a damaged file; saying which
                    // is the difference between "try again" and "restore a backup".
                    format!("{error}")
                }),
                None => Err("the vault file has gone missing".to_string()),
            },
        };

        // Cleared whether it worked or not: a failed attempt is exactly when somebody walks away from
        // the screen.
        self.wipe_typed();

        match result {
            Ok(vault) => {
                let created = prompt == Prompt::Create;
                self.vault = Some(vault);
                self.prompt = None;
                self.error = None;
                if created {
                    // Written immediately so that a vault created and then abandoned still exists next
                    // time, rather than silently asking to be created again.
                    self.persist(store);
                }
                self.pending.take()
            }
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }

    /// Read a secret, if the vault is open and holds it.
    pub(crate) fn get(&self, name: &str) -> Option<Secret> {
        self.vault.as_ref()?.get(name).ok().flatten()
    }

    /// Store a secret and write the vault out.
    pub(crate) fn set(&mut self, store: Option<&ConfigStore>, name: &str, secret: &Secret) -> bool {
        let Some(vault) = self.vault.as_mut() else {
            return false;
        };
        if vault.set(name, secret).is_err() {
            return false;
        }
        self.persist(store);
        true
    }

    /// Write the vault to disk.
    fn persist(&self, store: Option<&ConfigStore>) {
        let (Some(vault), Some(store)) = (self.vault.as_ref(), store) else {
            return;
        };
        if let Err(error) = store.save_vault_file(&vault.to_file()) {
            tracing::error!(%error, "could not save the vault");
        }
    }

    /// Overwrite the typed password rather than merely dropping it.
    ///
    /// `String::clear` leaves the bytes in the allocation. This does not make the interface secure --
    /// the text sat in an editable field a moment ago, and the windowing layer has its own copies --
    /// but leaving a master password lying in a live heap when one line prevents it is not a trade
    /// worth making.
    fn wipe_typed(&mut self) {
        for field in [&mut self.typed, &mut self.repeated] {
            // Safety of a different kind: overwriting in place needs the bytes, and a password may be
            // any UTF-8, so the length in bytes is what has to be covered.
            let filler = "\0".repeat(field.len());
            field.replace_range(.., &filler);
            field.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locked_vault_answers_nothing() {
        let state = VaultState::default();
        assert!(!state.is_open());
        assert_eq!(state.get("anything").map(|s| s.expose().to_owned()), None);
    }

    #[test]
    fn creating_requires_the_two_passwords_to_agree() {
        let mut state = VaultState {
            prompt: Some(Prompt::Create),
            typed: "one".to_string(),
            repeated: "other".to_string(),
            ..VaultState::default()
        };

        assert!(state.submit(None).is_none());
        assert!(!state.is_open(), "a mismatch must not create anything");
        assert_eq!(
            state.error.as_deref(),
            Some("the two passwords do not match")
        );
    }

    #[test]
    fn an_empty_master_password_is_refused() {
        let mut state = VaultState {
            prompt: Some(Prompt::Create),
            ..VaultState::default()
        };
        assert!(state.submit(None).is_none());
        assert!(!state.is_open());
        assert!(state.error.is_some());
    }

    #[test]
    fn creating_opens_the_vault_and_clears_what_was_typed() {
        let typed = "correct horse battery staple".to_string();
        let mut state = VaultState {
            prompt: Some(Prompt::Create),
            repeated: typed.clone(),
            typed,
            ..VaultState::default()
        };

        state.submit(None);
        assert!(state.is_open());
        assert_eq!(state.prompt, None);
        assert!(state.typed.is_empty(), "the password was left in the field");
        assert!(state.repeated.is_empty());
    }

    #[test]
    fn a_secret_survives_being_stored_and_read_back() {
        let mut state = VaultState {
            prompt: Some(Prompt::Create),
            typed: "master".to_string(),
            repeated: "master".to_string(),
            ..VaultState::default()
        };
        state.submit(None);

        assert!(state.set(None, "prod/db", &Secret::new("hunter2")));
        assert_eq!(
            state.get("prod/db").map(|s| s.expose().to_owned()),
            Some("hunter2".to_string())
        );
    }

    #[test]
    fn nothing_can_be_stored_while_the_vault_is_locked() {
        // Returning false rather than silently doing nothing: a caller that thinks it saved a password
        // will not ask again, and the person will wonder why they keep being prompted.
        let mut state = VaultState::default();
        assert!(!state.set(None, "prod/db", &Secret::new("hunter2")));
    }

    #[test]
    fn a_wrong_password_leaves_the_vault_shut_and_says_so() {
        let mut state = VaultState {
            prompt: Some(Prompt::Unlock),
            typed: "wrong".to_string(),
            ..VaultState::default()
        };

        // With no store there is no file, which is the same shape of failure as a wrong password: the
        // vault stays shut and the reason is shown rather than swallowed.
        assert!(state.submit(None).is_none());
        assert!(!state.is_open());
        assert!(state.error.is_some());
        assert!(state.typed.is_empty());
    }

    #[test]
    fn what_was_typed_is_overwritten_and_not_merely_dropped() {
        let mut state = VaultState {
            typed: "a master password".to_string(),
            ..VaultState::default()
        };
        let capacity_before = state.typed.capacity();
        state.wipe_typed();
        assert!(state.typed.is_empty());
        // The allocation is reused, which is the point: the bytes it held were overwritten in place
        // rather than being left behind for whatever reads that memory next.
        assert_eq!(state.typed.capacity(), capacity_before);
    }
}
