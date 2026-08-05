//! Node identity.

use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

/// Identifier of a node in the session tree.
///
/// A UUID rather than something cheaper, for two reasons that both bite later:
///
/// * **Renames.** A path-derived id (`"Prod/db/mongo-1"`) changes when any ancestor is renamed,
///   which silently breaks every reference to it — jump-host chains, tunnel owners, saved layouts.
/// * **Merges.** The session tree is meant to be synchronised through git. Sequentially allocated
///   ids collide the first time two machines each add a session and the branches meet, and the
///   collision is invisible: both nodes claim the same id and one reference silently points at the
///   wrong host.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct NodeId(Uuid);

impl Default for NodeId {
    /// A fresh identifier, same as [`NodeId::new`].
    ///
    /// Exists so node structs can be built with `..Default::default()`, where "the id I did not
    /// mention" can only sensibly mean "a new one".
    fn default() -> Self {
        Self::new()
    }
}

impl NodeId {
    /// A fresh, random identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wrap an existing UUID, for deserialisation and tests.
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying UUID.
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for NodeId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::from_str(s)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let a = NodeId::new();
        let b = NodeId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn display_and_parse_round_trip() {
        let id = NodeId::new();
        let text = id.to_string();
        assert_eq!(text.parse::<NodeId>().expect("parses"), id);
    }

    #[test]
    fn parsing_rubbish_is_an_error_not_a_panic() {
        assert!("not-a-uuid".parse::<NodeId>().is_err());
        assert!("".parse::<NodeId>().is_err());
    }

    /// Tested through TOML because that is the format the tree is stored in: an id must land as a
    /// plain quoted string, not a table, or the on-disk file stops being readable by a human.
    #[test]
    fn serialises_as_a_bare_string() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Holder {
            id: NodeId,
        }

        let id = NodeId::from_uuid(
            Uuid::parse_str("1b4e28ba-2fa1-11d2-883f-0016d3cca427").expect("valid uuid"),
        );
        let text = toml::to_string(&Holder { id }).expect("serialises");
        assert_eq!(
            text.trim(),
            r#"id = "1b4e28ba-2fa1-11d2-883f-0016d3cca427""#
        );

        let back: Holder = toml::from_str(&text).expect("deserialises");
        assert_eq!(back.id, id);
    }
}
