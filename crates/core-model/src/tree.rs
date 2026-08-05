//! The session tree.
//!
//! Flat in memory — a map from [`NodeId`] to node, so lookup by id is O(1) and a jump-host reference
//! costs nothing to follow. Nested on disk, which is a separate concern handled by [`crate::doc`].
//!
//! The tree deliberately does not implement `Serialize`. Deserialising straight into it would let a
//! hand-edited or merge-mangled file produce a structure that violates its own invariants — a child
//! whose parent does not exist, or a cycle. Loading goes through [`crate::doc`], which validates.

use std::collections::HashMap;

use crate::id::NodeId;
use crate::protocol::ProtocolConfig;
use crate::settings::{ResolvedSettings, SettingsOverride};

/// Things that can go wrong when editing the tree.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ModelError {
    /// No such node.
    #[error("no node with id {0}")]
    UnknownNode(NodeId),

    /// A session was given where a folder was required.
    #[error("node {0} is a session, not a folder")]
    NotAFolder(NodeId),

    /// The move would have made a node its own ancestor.
    #[error("cannot move {node} into {into}: it is inside itself")]
    WouldCreateCycle {
        /// The node being moved.
        node: NodeId,
        /// The proposed new parent.
        into: NodeId,
    },
}

/// Result alias for tree edits.
pub type Result<T> = std::result::Result<T, ModelError>;

/// What a node is.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    /// A container.
    Folder {
        /// Whether the UI shows it open. Persisted so the tree looks the same next launch.
        expanded: bool,
    },
    /// A connection.
    Session {
        /// How to reach the far end.
        config: Box<ProtocolConfig>,
    },
}

impl NodeKind {
    /// Whether this is a folder.
    pub fn is_folder(&self) -> bool {
        matches!(self, Self::Folder { .. })
    }

    /// The connection settings, if this is a session.
    pub fn config(&self) -> Option<&ProtocolConfig> {
        match self {
            // Dereferenced through the box so callers never see the indirection, which is a size
            // optimisation for the enum rather than part of the model.
            Self::Session { config } => Some(&**config),
            Self::Folder { .. } => None,
        }
    }
}

/// One folder or session.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Stable identity.
    pub id: NodeId,
    /// Containing folder, or `None` at the top level.
    pub parent: Option<NodeId>,
    /// Display name.
    pub name: String,
    /// Folder or session.
    pub kind: NodeKind,
    /// Settings this node imposes on itself and its descendants.
    pub settings: SettingsOverride,
    /// Free-form tags, searchable.
    pub tags: Vec<String>,
    /// Icon identifier. Imported `ImgNum` values are mapped to these.
    pub icon: Option<String>,
    /// User's note.
    pub comment: Option<String>,
    /// Ordered children. Always empty for a session.
    children: Vec<NodeId>,
}

impl Node {
    /// Ordered children.
    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    /// Whether this is a folder.
    pub fn is_folder(&self) -> bool {
        self.kind.is_folder()
    }
}

/// Everything needed to place a node while loading a validated document.
///
/// Deliberately has no `children` field: child links are established afterwards, one at a time,
/// through `SessionTree::link_loaded`.
pub(crate) struct NodeSeed {
    pub(crate) id: NodeId,
    pub(crate) parent: Option<NodeId>,
    pub(crate) name: String,
    pub(crate) kind: NodeKind,
    pub(crate) settings: SettingsOverride,
    pub(crate) tags: Vec<String>,
    pub(crate) icon: Option<String>,
    pub(crate) comment: Option<String>,
}

/// A tree of folders and sessions.
#[derive(Clone, Debug, Default)]
pub struct SessionTree {
    nodes: HashMap<NodeId, Node>,
    roots: Vec<NodeId>,
}

impl SessionTree {
    /// An empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Top-level nodes, in order.
    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    /// How many nodes the tree holds.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Whether a node exists.
    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    /// A node by id.
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// A node by id, mutably.
    ///
    /// Structure is not reachable this way: `children` and `parent` are only changed through
    /// [`Self::move_node`] and [`Self::remove`], which maintain the invariants.
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(&id)
    }

    /// Ordered children of a node, or an empty slice if it has none or does not exist.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.nodes
            .get(&id)
            .map(|node| node.children())
            .unwrap_or(&[])
    }

    /// Add a folder. `parent` of `None` puts it at the top level.
    pub fn add_folder(&mut self, parent: Option<NodeId>, name: impl Into<String>) -> Result<NodeId> {
        self.insert(parent, name.into(), NodeKind::Folder { expanded: true })
    }

    /// Add a session. `parent` of `None` puts it at the top level.
    pub fn add_session(
        &mut self,
        parent: Option<NodeId>,
        name: impl Into<String>,
        config: ProtocolConfig,
    ) -> Result<NodeId> {
        let kind = NodeKind::Session {
            config: Box::new(config),
        };
        self.insert(parent, name.into(), kind)
    }

    fn insert(&mut self, parent: Option<NodeId>, name: String, kind: NodeKind) -> Result<NodeId> {
        if let Some(parent_id) = parent {
            self.require_folder(parent_id)?;
        }

        let id = NodeId::new();
        self.nodes.insert(
            id,
            Node {
                id,
                parent,
                name,
                kind,
                settings: SettingsOverride::default(),
                tags: Vec::new(),
                icon: None,
                comment: None,
                children: Vec::new(),
            },
        );

        match parent {
            Some(parent_id) => {
                if let Some(node) = self.nodes.get_mut(&parent_id) {
                    node.children.push(id);
                }
            }
            None => self.roots.push(id),
        }

        Ok(id)
    }

    /// Remove a node and everything under it.
    ///
    /// Returns the ids removed, deepest last, so a caller can clean up anything that referenced
    /// them — open tabs, tunnels, jump-host chains.
    pub fn remove(&mut self, id: NodeId) -> Result<Vec<NodeId>> {
        if !self.contains(id) {
            return Err(ModelError::UnknownNode(id));
        }

        let doomed = self.subtree(id);

        // Unlink from the parent first, while the node still exists.
        let parent = self.nodes.get(&id).and_then(|node| node.parent);
        match parent {
            Some(parent_id) => {
                if let Some(node) = self.nodes.get_mut(&parent_id) {
                    node.children.retain(|child| *child != id);
                }
            }
            None => self.roots.retain(|root| *root != id),
        }

        for victim in &doomed {
            self.nodes.remove(victim);
        }

        Ok(doomed)
    }

    /// Move a node to a new parent, optionally at a given position among its siblings.
    ///
    /// `index` beyond the end appends. Moving a node to the parent it already has is a reorder.
    pub fn move_node(
        &mut self,
        id: NodeId,
        new_parent: Option<NodeId>,
        index: Option<usize>,
    ) -> Result<()> {
        if !self.contains(id) {
            return Err(ModelError::UnknownNode(id));
        }

        if let Some(parent_id) = new_parent {
            self.require_folder(parent_id)?;
            // Moving a folder inside itself would detach the whole subtree from the tree and leak
            // it: the nodes stay in the map but nothing reaches them, and `walk` never terminates.
            if parent_id == id || self.is_descendant_of(parent_id, id) {
                return Err(ModelError::WouldCreateCycle {
                    node: id,
                    into: parent_id,
                });
            }
        }

        // Detach.
        let old_parent = self.nodes.get(&id).and_then(|node| node.parent);
        match old_parent {
            Some(parent_id) => {
                if let Some(node) = self.nodes.get_mut(&parent_id) {
                    node.children.retain(|child| *child != id);
                }
            }
            None => self.roots.retain(|root| *root != id),
        }

        // Attach.
        if let Some(node) = self.nodes.get_mut(&id) {
            node.parent = new_parent;
        }
        let siblings = match new_parent {
            Some(parent_id) => match self.nodes.get_mut(&parent_id) {
                Some(node) => &mut node.children,
                None => return Err(ModelError::UnknownNode(parent_id)),
            },
            None => &mut self.roots,
        };
        let at = index.unwrap_or(siblings.len()).min(siblings.len());
        siblings.insert(at, id);

        Ok(())
    }

    /// Rename a node.
    pub fn rename(&mut self, id: NodeId, name: impl Into<String>) -> Result<()> {
        match self.nodes.get_mut(&id) {
            Some(node) => {
                node.name = name.into();
                Ok(())
            }
            None => Err(ModelError::UnknownNode(id)),
        }
    }

    /// Ancestors of a node, outermost first, excluding the node itself.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut chain = Vec::new();
        let mut cursor = self.nodes.get(&id).and_then(|node| node.parent);
        while let Some(current) = cursor {
            chain.push(current);
            cursor = self.nodes.get(&current).and_then(|node| node.parent);
            // A cycle cannot arise through the public API, but a corrupted file could describe one
            // and this must not become an infinite loop.
            if chain.len() > self.nodes.len() {
                break;
            }
        }
        chain.reverse();
        chain
    }

    /// Whether `id` sits anywhere beneath `ancestor`.
    pub fn is_descendant_of(&self, id: NodeId, ancestor: NodeId) -> bool {
        self.ancestors(id).contains(&ancestor)
    }

    /// A node and everything beneath it, depth-first, parents before children.
    pub fn subtree(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            if !self.contains(current) || out.contains(&current) {
                continue;
            }
            out.push(current);
            // Reversed so that popping yields the children in their display order.
            for child in self.children(current).iter().rev() {
                stack.push(*child);
            }
        }
        out
    }

    /// The whole tree, depth-first, in display order.
    pub fn walk(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        for root in &self.roots {
            out.extend(self.subtree(*root));
        }
        out
    }

    /// Names from the root down to and including the node.
    pub fn path(&self, id: NodeId) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .ancestors(id)
            .into_iter()
            .filter_map(|ancestor| self.nodes.get(&ancestor))
            .map(|node| node.name.as_str())
            .collect();
        if let Some(node) = self.nodes.get(&id) {
            names.push(node.name.as_str());
        }
        names
    }

    /// The path as a single display string.
    pub fn path_string(&self, id: NodeId) -> String {
        self.path(id).join(" / ")
    }

    /// Settings for a node, with folder inheritance applied.
    ///
    /// Ancestors are applied outermost first, so the closest one wins.
    pub fn resolve_settings(&self, id: NodeId) -> ResolvedSettings {
        let mut resolved = ResolvedSettings::default();
        for ancestor in self.ancestors(id) {
            if let Some(node) = self.nodes.get(&ancestor) {
                node.settings.apply_to(&mut resolved);
            }
        }
        if let Some(node) = self.nodes.get(&id) {
            node.settings.apply_to(&mut resolved);
        }
        resolved
    }

    /// Nodes matching `query`, in display order.
    ///
    /// Matches the name, the tags, the comment and — importantly — the hostname. In a tree of five
    /// hundred hosts most sessions are not named after the machine they reach, so searching only
    /// names finds the wrong half of them.
    pub fn search(&self, query: &str) -> Vec<NodeId> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }

        self.walk()
            .into_iter()
            .filter(|id| {
                let Some(node) = self.nodes.get(id) else {
                    return false;
                };
                let name_hit = node.name.to_lowercase().contains(&needle);
                let tag_hit = node
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&needle));
                let comment_hit = node
                    .comment
                    .as_deref()
                    .is_some_and(|text| text.to_lowercase().contains(&needle));
                let host_hit = node
                    .kind
                    .config()
                    .and_then(|config| config.host())
                    .is_some_and(|host| host.to_lowercase().contains(&needle));
                name_hit || tag_hit || comment_hit || host_hit
            })
            .collect()
    }

    /// Insert a node whose structure is already known, used when loading a document.
    ///
    /// Takes a `NodeSeed` rather than a [`Node`] so that `Node::children` stays private to this
    /// module: nothing outside it can hand the tree a pre-populated child list and bypass the
    /// invariants. Validation of the document itself lives in [`crate::doc`].
    pub(crate) fn insert_loaded(&mut self, seed: NodeSeed) {
        let id = seed.id;
        let parent = seed.parent;
        self.nodes.insert(
            id,
            Node {
                id,
                parent,
                name: seed.name,
                kind: seed.kind,
                settings: seed.settings,
                tags: seed.tags,
                icon: seed.icon,
                comment: seed.comment,
                children: Vec::new(),
            },
        );
        if parent.is_none() {
            self.roots.push(id);
        }
    }

    /// Record `child` as a child of `parent` while loading.
    pub(crate) fn link_loaded(&mut self, parent: NodeId, child: NodeId) {
        if let Some(node) = self.nodes.get_mut(&parent) {
            node.children.push(child);
        }
    }

    fn require_folder(&self, id: NodeId) -> Result<()> {
        match self.nodes.get(&id) {
            None => Err(ModelError::UnknownNode(id)),
            Some(node) if !node.is_folder() => Err(ModelError::NotAFolder(id)),
            Some(_) => Ok(()),
        }
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

    fn shell() -> ProtocolConfig {
        ProtocolConfig::LocalShell(LocalShellConfig::default())
    }

    /// prod/db/mongo-1, prod/web, plus a top-level local shell.
    fn sample() -> (SessionTree, NodeId, NodeId, NodeId, NodeId, NodeId) {
        let mut tree = SessionTree::new();
        let prod = tree.add_folder(None, "Production").expect("folder");
        let db = tree.add_folder(Some(prod), "db").expect("folder");
        let mongo = tree
            .add_session(Some(db), "mongo-1", ssh("mongo-1.int"))
            .expect("session");
        let web = tree
            .add_session(Some(prod), "web", ssh("web-1.int"))
            .expect("session");
        let local = tree.add_session(None, "Local", shell()).expect("session");
        (tree, prod, db, mongo, web, local)
    }

    #[test]
    fn a_new_tree_is_empty() {
        let tree = SessionTree::new();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
        assert!(tree.roots().is_empty());
    }

    #[test]
    fn nodes_land_where_they_were_put() {
        let (tree, prod, db, mongo, web, local) = sample();
        assert_eq!(tree.len(), 5);
        assert_eq!(tree.roots(), &[prod, local]);
        assert_eq!(tree.children(prod), &[db, web]);
        assert_eq!(tree.children(db), &[mongo]);
        assert_eq!(tree.get(mongo).expect("node").parent, Some(db));
    }

    #[test]
    fn a_session_cannot_be_a_parent() {
        let (mut tree, _, _, mongo, _, _) = sample();
        let err = tree.add_folder(Some(mongo), "nope").expect_err("must fail");
        assert_eq!(err, ModelError::NotAFolder(mongo));
    }

    #[test]
    fn adding_under_a_missing_parent_is_an_error() {
        let mut tree = SessionTree::new();
        let ghost = NodeId::new();
        assert_eq!(
            tree.add_folder(Some(ghost), "x").expect_err("must fail"),
            ModelError::UnknownNode(ghost)
        );
    }

    #[test]
    fn removing_a_folder_takes_its_subtree() {
        let (mut tree, prod, db, mongo, web, local) = sample();
        let removed = tree.remove(db).expect("removes");
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&db));
        assert!(removed.contains(&mongo));
        assert_eq!(tree.len(), 3);
        assert_eq!(tree.children(prod), &[web]);
        assert!(!tree.contains(mongo));
        assert!(tree.contains(local));
    }

    #[test]
    fn removing_a_root_unlinks_it_from_the_roots() {
        let (mut tree, prod, _, _, _, local) = sample();
        tree.remove(prod).expect("removes");
        assert_eq!(tree.roots(), &[local]);
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn removing_a_missing_node_is_an_error() {
        let mut tree = SessionTree::new();
        let ghost = NodeId::new();
        assert_eq!(
            tree.remove(ghost).expect_err("must fail"),
            ModelError::UnknownNode(ghost)
        );
    }

    #[test]
    fn moving_between_folders_updates_both_ends() {
        let (mut tree, prod, db, mongo, _, _) = sample();
        tree.move_node(mongo, Some(prod), Some(0)).expect("moves");
        assert_eq!(tree.get(mongo).expect("node").parent, Some(prod));
        assert_eq!(tree.children(prod).first(), Some(&mongo));
        assert!(tree.children(db).is_empty());
    }

    #[test]
    fn moving_to_the_top_level_adds_a_root() {
        let (mut tree, _, db, mongo, _, _) = sample();
        tree.move_node(mongo, None, None).expect("moves");
        assert_eq!(tree.get(mongo).expect("node").parent, None);
        assert!(tree.roots().contains(&mongo));
        assert!(tree.children(db).is_empty());
    }

    #[test]
    fn a_folder_cannot_be_moved_inside_itself() {
        let (mut tree, prod, db, _, _, _) = sample();
        // Directly...
        let into_self = tree.move_node(prod, Some(prod), None);
        assert_eq!(
            into_self.expect_err("must fail"),
            ModelError::WouldCreateCycle {
                node: prod,
                into: prod
            }
        );
        // ...and into its own descendant, which is the case that would silently orphan the subtree.
        let into_child = tree.move_node(prod, Some(db), None);
        assert_eq!(
            into_child.expect_err("must fail"),
            ModelError::WouldCreateCycle {
                node: prod,
                into: db
            }
        );
        // The failed moves changed nothing.
        assert_eq!(tree.get(prod).expect("node").parent, None);
        assert_eq!(tree.get(db).expect("node").parent, Some(prod));
    }

    #[test]
    fn reordering_within_a_parent_works() {
        let (mut tree, prod, db, _, web, _) = sample();
        assert_eq!(tree.children(prod), &[db, web]);
        tree.move_node(web, Some(prod), Some(0)).expect("moves");
        assert_eq!(tree.children(prod), &[web, db]);
    }

    #[test]
    fn an_index_past_the_end_appends() {
        let (mut tree, prod, db, _, web, _) = sample();
        tree.move_node(db, Some(prod), Some(999)).expect("moves");
        assert_eq!(tree.children(prod), &[web, db]);
    }

    #[test]
    fn ancestors_run_outermost_first() {
        let (tree, prod, db, mongo, _, _) = sample();
        assert_eq!(tree.ancestors(mongo), vec![prod, db]);
        assert_eq!(tree.ancestors(prod), Vec::<NodeId>::new());
    }

    #[test]
    fn paths_read_from_the_root_down() {
        let (tree, _, _, mongo, _, _) = sample();
        assert_eq!(tree.path(mongo), vec!["Production", "db", "mongo-1"]);
        assert_eq!(tree.path_string(mongo), "Production / db / mongo-1");
    }

    #[test]
    fn walk_visits_everything_in_display_order() {
        let (tree, prod, db, mongo, web, local) = sample();
        assert_eq!(tree.walk(), vec![prod, db, mongo, web, local]);
    }

    #[test]
    fn settings_inherit_from_the_nearest_ancestor() {
        let (mut tree, prod, db, mongo, web, _) = sample();

        tree.get_mut(prod).expect("node").settings = SettingsOverride {
            scrollback: Some(50_000),
            keepalive_secs: Some(30),
            ..Default::default()
        };
        tree.get_mut(db).expect("node").settings = SettingsOverride {
            keepalive_secs: Some(15),
            ..Default::default()
        };

        // mongo-1 sits under both: the closer folder wins on keepalive, the outer one still supplies
        // scrollback.
        let resolved = tree.resolve_settings(mongo);
        assert_eq!(resolved.keepalive_secs, 15);
        assert_eq!(resolved.scrollback, 50_000);

        // web is only under Production.
        let web_resolved = tree.resolve_settings(web);
        assert_eq!(web_resolved.keepalive_secs, 30);
        assert_eq!(web_resolved.scrollback, 50_000);
    }

    #[test]
    fn a_session_overrides_its_folders() {
        let (mut tree, prod, _, mongo, _, _) = sample();
        tree.get_mut(prod).expect("node").settings = SettingsOverride {
            scrollback: Some(50_000),
            ..Default::default()
        };
        tree.get_mut(mongo).expect("node").settings = SettingsOverride {
            scrollback: Some(1_000),
            ..Default::default()
        };
        assert_eq!(tree.resolve_settings(mongo).scrollback, 1_000);
    }

    #[test]
    fn an_unset_node_gets_the_defaults() {
        let (tree, _, _, _, _, local) = sample();
        assert_eq!(tree.resolve_settings(local), ResolvedSettings::default());
    }

    #[test]
    fn settings_of_an_unknown_node_are_the_defaults() {
        let tree = SessionTree::new();
        assert_eq!(
            tree.resolve_settings(NodeId::new()),
            ResolvedSettings::default()
        );
    }

    #[test]
    fn search_finds_a_session_by_hostname_it_is_not_named_after() {
        let (tree, _, _, mongo, _, _) = sample();
        // "mongo-1" is named after the host here, so use the domain part to prove host matching.
        assert_eq!(tree.search("int").len(), 2);
        assert!(tree.search("mongo-1.int").contains(&mongo));
    }

    #[test]
    fn search_matches_names_tags_and_comments() {
        let (mut tree, _, _, mongo, _, _) = sample();
        tree.get_mut(mongo).expect("node").tags = vec!["primary".to_string()];
        tree.get_mut(mongo).expect("node").comment = Some("holds the billing data".to_string());

        assert_eq!(tree.search("primary"), vec![mongo]);
        assert_eq!(tree.search("billing"), vec![mongo]);
        assert_eq!(tree.search("PRODUCTION").len(), 1);
    }

    #[test]
    fn search_for_nothing_returns_nothing() {
        let (tree, _, _, _, _, _) = sample();
        assert!(tree.search("").is_empty());
        assert!(tree.search("   ").is_empty());
        assert!(tree.search("no-such-host").is_empty());
    }

    #[test]
    fn renaming_changes_the_name_and_the_path() {
        let (mut tree, _, db, mongo, _, _) = sample();
        tree.rename(db, "databases").expect("renames");
        assert_eq!(tree.path_string(mongo), "Production / databases / mongo-1");
    }

    #[test]
    fn renaming_a_missing_node_is_an_error() {
        let mut tree = SessionTree::new();
        let ghost = NodeId::new();
        assert_eq!(
            tree.rename(ghost, "x").expect_err("must fail"),
            ModelError::UnknownNode(ghost)
        );
    }

    #[test]
    fn a_renamed_folder_does_not_break_a_jump_host_reference() {
        // The reason ids are UUIDs and not paths. See crates/core-model/src/id.rs.
        let mut tree = SessionTree::new();
        let folder = tree.add_folder(None, "Bastions").expect("folder");
        let bastion = tree
            .add_session(Some(folder), "edge", ssh("edge.example"))
            .expect("session");
        let target = tree
            .add_session(
                None,
                "internal",
                ProtocolConfig::Ssh(SshConfig {
                    host: "internal.int".to_string(),
                    jump_hosts: vec![bastion],
                    ..Default::default()
                }),
            )
            .expect("session");

        tree.rename(folder, "Jump hosts").expect("renames");
        tree.move_node(bastion, None, None).expect("moves");

        let config = tree.get(target).expect("node").kind.config().expect("ssh");
        let ProtocolConfig::Ssh(ssh_config) = config else {
            panic!("expected ssh");
        };
        assert_eq!(ssh_config.jump_hosts, vec![bastion]);
        assert!(tree.contains(bastion));
    }
}
