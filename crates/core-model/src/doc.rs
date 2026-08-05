//! The on-disk form of the session tree, and the validation between it and [`SessionTree`].
//!
//! # Why the file is flat
//!
//! A nested document reads more naturally, but the tree is meant to be synchronised through git, and
//! nesting makes every move a large diff: relocating one session rewrites the block it came from and
//! the block it lands in, which is exactly the shape that produces conflicts. A flat list with a
//! `parent` reference turns the same move into a couple of changed lines.
//!
//! Sibling order comes from the order of appearance in the list, so reordering is also a small diff.
//!
//! # Why loading is validated
//!
//! [`SessionTree`] does not implement `Deserialize` on purpose. A hand-edited or merge-mangled file
//! can describe a structure that is not a tree — a parent that does not exist, a session with
//! children, a cycle. Refusing to load with a specific complaint is better than loading something
//! that violates the invariants the rest of the code relies on.

use serde::{Deserialize, Serialize};

use crate::id::NodeId;
use crate::protocol::ProtocolConfig;
use crate::settings::SettingsOverride;
use crate::tree::{NodeKind, NodeSeed, SessionTree};

/// Ways a stored tree can fail to be a tree.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DocError {
    /// The same id appears twice.
    #[error("id {0} appears more than once")]
    DuplicateId(NodeId),

    /// A node names a parent that is not in the file.
    #[error("node {node} names parent {parent}, which does not exist")]
    UnknownParent {
        /// The child.
        node: NodeId,
        /// The missing parent.
        parent: NodeId,
    },

    /// A node is inside a session rather than a folder.
    #[error("node {node} is inside {parent}, which is a session, not a folder")]
    ParentIsSession {
        /// The child.
        node: NodeId,
        /// The offending parent.
        parent: NodeId,
    },

    /// A node is both a folder and a session.
    #[error("node {0} has both a config and folder fields")]
    AmbiguousKind(NodeId),

    /// A node is neither a folder nor a session.
    #[error("node {0} has no config and is not marked as a folder")]
    UnknownKind(NodeId),

    /// Following parents from this node never reaches the top level.
    #[error("node {0} is part of a cycle")]
    Cycle(NodeId),
}

/// The stored form of one node.
///
/// A folder carries `expanded`; a session carries `config`. Exactly one must be present, and
/// [`TreeDoc::into_tree`] says so plainly rather than guessing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeDoc {
    /// Stable identity.
    pub id: NodeId,
    /// Containing folder, absent at the top level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<NodeId>,
    /// Display name.
    pub name: String,
    /// Icon identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Searchable tags.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// User's note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Settings imposed on this node and its descendants.
    #[serde(skip_serializing_if = "SettingsOverride::is_empty")]
    pub settings: SettingsOverride,
    /// Present, and only present, for folders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    /// Present, and only present, for sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Box<ProtocolConfig>>,
}

/// The stored form of a whole tree.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TreeDoc {
    /// Every node, in display order.
    pub nodes: Vec<NodeDoc>,
}

impl TreeDoc {
    /// Capture a tree in its stored form.
    pub fn from_tree(tree: &SessionTree) -> Self {
        let nodes = tree
            .walk()
            .into_iter()
            .filter_map(|id| tree.get(id))
            .map(|node| {
                let (expanded, config) = match &node.kind {
                    NodeKind::Folder { expanded } => (Some(*expanded), None),
                    NodeKind::Session { config } => (None, Some(config.clone())),
                };
                NodeDoc {
                    id: node.id,
                    parent: node.parent,
                    name: node.name.clone(),
                    icon: node.icon.clone(),
                    tags: node.tags.clone(),
                    comment: node.comment.clone(),
                    settings: node.settings.clone(),
                    expanded,
                    config,
                }
            })
            .collect();

        Self { nodes }
    }

    /// Rebuild a tree, rejecting anything that is not one.
    pub fn into_tree(self) -> Result<SessionTree, DocError> {
        let mut tree = SessionTree::new();

        // Pass one: every node, so the order of the list does not have to put parents first.
        let mut seen: Vec<NodeId> = Vec::with_capacity(self.nodes.len());
        for doc in &self.nodes {
            if seen.contains(&doc.id) {
                return Err(DocError::DuplicateId(doc.id));
            }
            seen.push(doc.id);

            let kind = match (doc.expanded, &doc.config) {
                (Some(_), Some(_)) => return Err(DocError::AmbiguousKind(doc.id)),
                (None, None) => return Err(DocError::UnknownKind(doc.id)),
                (Some(expanded), None) => NodeKind::Folder { expanded },
                (None, Some(config)) => NodeKind::Session {
                    config: config.clone(),
                },
            };

            tree.insert_loaded(NodeSeed {
                id: doc.id,
                parent: doc.parent,
                name: doc.name.clone(),
                kind,
                settings: doc.settings.clone(),
                tags: doc.tags.clone(),
                icon: doc.icon.clone(),
                comment: doc.comment.clone(),
            });
        }

        // Pass two: link children in list order, which is what makes list order the display order.
        for doc in &self.nodes {
            let Some(parent_id) = doc.parent else {
                continue;
            };
            let Some(parent) = tree.get(parent_id) else {
                return Err(DocError::UnknownParent {
                    node: doc.id,
                    parent: parent_id,
                });
            };
            if !parent.is_folder() {
                return Err(DocError::ParentIsSession {
                    node: doc.id,
                    parent: parent_id,
                });
            }
            tree.link_loaded(parent_id, doc.id);
        }

        // Pass three: every node must reach the top level. A cycle would otherwise make `walk`
        // silently skip a whole subtree, and `ancestors` rely on their own step limit to escape.
        let total = tree.len();
        for doc in &self.nodes {
            let mut cursor = doc.parent;
            let mut steps = 0usize;
            while let Some(current) = cursor {
                steps += 1;
                if steps > total {
                    return Err(DocError::Cycle(doc.id));
                }
                cursor = tree.get(current).and_then(|node| node.parent);
            }
        }

        Ok(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LocalShellConfig, SshConfig};

    fn ssh(host: &str) -> ProtocolConfig {
        ProtocolConfig::Ssh(SshConfig {
            host: host.to_string(),
            ..Default::default()
        })
    }

    fn sample() -> SessionTree {
        let mut tree = SessionTree::new();
        let prod = tree.add_folder(None, "Production").expect("folder");
        let db = tree.add_folder(Some(prod), "db").expect("folder");
        tree.add_session(Some(db), "mongo-1", ssh("mongo-1.int"))
            .expect("session");
        tree.add_session(Some(prod), "web", ssh("web-1.int"))
            .expect("session");
        tree.add_session(
            None,
            "Local",
            ProtocolConfig::LocalShell(LocalShellConfig::default()),
        )
        .expect("session");

        tree.get_mut(prod).expect("node").settings = SettingsOverride {
            scrollback: Some(50_000),
            ..Default::default()
        };
        tree
    }

    #[test]
    fn a_tree_survives_a_round_trip_through_toml() {
        let original = sample();
        let text = toml::to_string(&TreeDoc::from_tree(&original)).expect("serialises");
        let doc: TreeDoc = toml::from_str(&text).expect("deserialises");
        let rebuilt = doc.into_tree().expect("valid");

        assert_eq!(rebuilt.len(), original.len());
        assert_eq!(rebuilt.walk(), original.walk());
        assert_eq!(rebuilt.roots(), original.roots());

        for id in original.walk() {
            assert_eq!(rebuilt.get(id), original.get(id), "node {id} differs");
            assert_eq!(
                rebuilt.resolve_settings(id),
                original.resolve_settings(id),
                "settings of {id} differ"
            );
        }
    }

    #[test]
    fn an_empty_tree_round_trips() {
        let text = toml::to_string(&TreeDoc::from_tree(&SessionTree::new())).expect("serialises");
        let doc: TreeDoc = toml::from_str(&text).expect("deserialises");
        assert!(doc.into_tree().expect("valid").is_empty());
    }

    #[test]
    fn the_stored_form_keeps_display_order() {
        let tree = sample();
        let doc = TreeDoc::from_tree(&tree);
        let ids: Vec<NodeId> = doc.nodes.iter().map(|node| node.id).collect();
        assert_eq!(ids, tree.walk());
    }

    #[test]
    fn a_node_with_no_opinions_writes_no_settings_table() {
        let mut tree = SessionTree::new();
        tree.add_folder(None, "Plain").expect("folder");
        let text = toml::to_string(&TreeDoc::from_tree(&tree)).expect("serialises");
        assert!(
            !text.contains("settings"),
            "expected no settings table, got:\n{text}"
        );
    }

    #[test]
    fn children_may_appear_before_their_parents_in_the_file() {
        // Order-independence keeps a git merge that reshuffles lines from breaking the file.
        let parent = NodeId::new();
        let child = NodeId::new();
        let doc = TreeDoc {
            nodes: vec![
                NodeDoc {
                    id: child,
                    parent: Some(parent),
                    name: "child".to_string(),
                    config: Some(Box::new(ssh("h"))),
                    ..Default::default()
                },
                NodeDoc {
                    id: parent,
                    name: "parent".to_string(),
                    expanded: Some(true),
                    ..Default::default()
                },
            ],
        };

        let tree = doc.into_tree().expect("valid");
        assert_eq!(tree.children(parent), &[child]);
        assert_eq!(tree.roots(), &[parent]);
    }

    #[test]
    fn a_duplicate_id_is_rejected() {
        let id = NodeId::new();
        let doc = TreeDoc {
            nodes: vec![
                NodeDoc {
                    id,
                    name: "a".to_string(),
                    expanded: Some(true),
                    ..Default::default()
                },
                NodeDoc {
                    id,
                    name: "b".to_string(),
                    expanded: Some(true),
                    ..Default::default()
                },
            ],
        };
        assert_eq!(
            doc.into_tree().expect_err("must fail"),
            DocError::DuplicateId(id)
        );
    }

    #[test]
    fn a_missing_parent_is_rejected() {
        let node = NodeId::new();
        let parent = NodeId::new();
        let doc = TreeDoc {
            nodes: vec![NodeDoc {
                id: node,
                parent: Some(parent),
                name: "orphan".to_string(),
                config: Some(Box::new(ssh("h"))),
                ..Default::default()
            }],
        };
        assert_eq!(
            doc.into_tree().expect_err("must fail"),
            DocError::UnknownParent { node, parent }
        );
    }

    #[test]
    fn a_session_cannot_hold_children() {
        let parent = NodeId::new();
        let node = NodeId::new();
        let doc = TreeDoc {
            nodes: vec![
                NodeDoc {
                    id: parent,
                    name: "a session".to_string(),
                    config: Some(Box::new(ssh("h"))),
                    ..Default::default()
                },
                NodeDoc {
                    id: node,
                    parent: Some(parent),
                    name: "child".to_string(),
                    config: Some(Box::new(ssh("h2"))),
                    ..Default::default()
                },
            ],
        };
        assert_eq!(
            doc.into_tree().expect_err("must fail"),
            DocError::ParentIsSession { node, parent }
        );
    }

    #[test]
    fn a_node_that_is_both_kinds_is_rejected() {
        let id = NodeId::new();
        let doc = TreeDoc {
            nodes: vec![NodeDoc {
                id,
                name: "confused".to_string(),
                expanded: Some(true),
                config: Some(Box::new(ssh("h"))),
                ..Default::default()
            }],
        };
        assert_eq!(
            doc.into_tree().expect_err("must fail"),
            DocError::AmbiguousKind(id)
        );
    }

    #[test]
    fn a_node_that_is_neither_kind_is_rejected() {
        let id = NodeId::new();
        let doc = TreeDoc {
            nodes: vec![NodeDoc {
                id,
                name: "empty".to_string(),
                ..Default::default()
            }],
        };
        assert_eq!(
            doc.into_tree().expect_err("must fail"),
            DocError::UnknownKind(id)
        );
    }

    #[test]
    fn a_cycle_is_rejected_rather_than_hanging() {
        // Two folders each claiming the other as parent: reachable only by hand-editing or a bad
        // merge, and the case that would otherwise make `walk` skip both silently.
        let a = NodeId::new();
        let b = NodeId::new();
        let doc = TreeDoc {
            nodes: vec![
                NodeDoc {
                    id: a,
                    parent: Some(b),
                    name: "a".to_string(),
                    expanded: Some(true),
                    ..Default::default()
                },
                NodeDoc {
                    id: b,
                    parent: Some(a),
                    name: "b".to_string(),
                    expanded: Some(true),
                    ..Default::default()
                },
            ],
        };
        assert!(matches!(
            doc.into_tree().expect_err("must fail"),
            DocError::Cycle(_)
        ));
    }

    #[test]
    fn an_unknown_key_in_the_file_is_rejected() {
        let text = r#"
            [[nodes]]
            id = "1b4e28ba-2fa1-11d2-883f-0016d3cca427"
            name = "x"
            expanded = true
            colour = "red"
        "#;
        assert!(toml::from_str::<TreeDoc>(text).is_err());
    }
}
