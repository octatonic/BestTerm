//! The saved window layout.
//!
//! Kept in the state directory rather than the config directory, and therefore *not* synchronised
//! between machines: a layout restored onto a different monitor arrangement is worse than no layout.
//!
//! The model is data only — no `egui`, no widgets — so the split geometry is testable, and so the
//! renderer can be replaced without touching what "two panes side by side" means.

use bestterm_core_model::NodeId;
use serde::{Deserialize, Serialize};

use crate::store::Document;

/// Narrowest a split may be dragged, as a fraction.
///
/// Clamped rather than free so a saved layout can never restore a pane that is zero pixels wide and
/// therefore impossible to grab and pull back open.
const MIN_RATIO: f32 = 0.05;

/// Widest a split may be dragged, as a fraction.
const MAX_RATIO: f32 = 0.95;

/// What a pane shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "content", rename_all = "kebab-case")]
pub enum PaneContent {
    /// A session from the tree, by id.
    Session {
        /// Which session.
        session: NodeId,
    },
    /// A local shell that is not a saved session.
    LocalShell {
        /// Shell id from discovery; `None` means the default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<String>,
    },
}

/// Which way a split divides its space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SplitAxis {
    /// Side by side.
    Vertical,
    /// One above the other.
    Horizontal,
}

/// A pane, or a split of two.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "kebab-case")]
pub enum PaneNode {
    /// A single pane.
    Leaf(PaneContent),
    /// Two children divided along an axis.
    Split {
        /// Direction of the divider.
        axis: SplitAxis,
        /// Fraction of the space given to the first child.
        ratio: f32,
        /// Left, or top.
        first: Box<PaneNode>,
        /// Right, or bottom.
        second: Box<PaneNode>,
    },
}

impl PaneNode {
    /// A single pane showing a session.
    pub fn session(id: NodeId) -> Self {
        Self::Leaf(PaneContent::Session { session: id })
    }

    /// A single pane showing a local shell.
    pub fn local_shell(shell: Option<String>) -> Self {
        Self::Leaf(PaneContent::LocalShell { shell })
    }

    /// How many panes this subtree holds.
    pub fn pane_count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Split { first, second, .. } => first.pane_count() + second.pane_count(),
        }
    }

    /// Every pane's content, left to right, top to bottom.
    pub fn panes(&self) -> Vec<&PaneContent> {
        match self {
            Self::Leaf(content) => vec![content],
            Self::Split { first, second, .. } => {
                let mut out = first.panes();
                out.extend(second.panes());
                out
            }
        }
    }

    /// Bring every ratio back into range, in place.
    ///
    /// Applied on load, because the file may have been hand-edited, and because a `NaN` ratio would
    /// otherwise propagate into layout arithmetic and make an entire tab invisible.
    pub fn clamp_ratios(&mut self) {
        if let Self::Split {
            ratio,
            first,
            second,
            ..
        } = self
        {
            if !ratio.is_finite() {
                *ratio = 0.5;
            }
            *ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);
            first.clamp_ratios();
            second.clamp_ratios();
        }
    }

    /// Drop panes whose session is no longer in the tree.
    ///
    /// Returns `None` when nothing is left. Sessions get deleted between runs, and a saved layout
    /// pointing at a gone session must not resurrect it or refuse to load.
    pub fn retain_sessions(&self, exists: &dyn Fn(NodeId) -> bool) -> Option<Self> {
        match self {
            Self::Leaf(PaneContent::Session { session }) => {
                if exists(*session) {
                    Some(self.clone())
                } else {
                    None
                }
            }
            Self::Leaf(_) => Some(self.clone()),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.retain_sessions(exists), second.retain_sessions(exists)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                // A split with one surviving child collapses into that child rather than leaving an
                // empty half.
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }
}

/// One tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabLayout {
    /// Title the user pinned, if any. Absent means the session or program decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Pane arrangement.
    pub root: PaneNode,
}

/// Window geometry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowLayout {
    /// Inner width in logical pixels.
    pub width: f32,
    /// Inner height in logical pixels.
    pub height: f32,
    /// Whether the window was maximised.
    pub maximized: bool,
}

impl Default for WindowLayout {
    fn default() -> Self {
        Self {
            width: 1180.0,
            height: 760.0,
            maximized: false,
        }
    }
}

/// State of the left panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SidebarLayout {
    /// Whether the panel is expanded.
    pub open: bool,
    /// Which panel the edge strip has selected.
    pub panel: String,
    /// Width in logical pixels.
    pub width: f32,
}

impl Default for SidebarLayout {
    fn default() -> Self {
        Self {
            open: true,
            panel: "sessions".to_string(),
            width: 220.0,
        }
    }
}

/// The saved layout file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutDoc {
    /// Window geometry.
    pub window: WindowLayout,
    /// Left panel state.
    pub sidebar: SidebarLayout,
    /// Open tabs, in order.
    pub tabs: Vec<TabLayout>,
    /// Index into [`Self::tabs`].
    pub active_tab: usize,
}

impl LayoutDoc {
    /// Make a loaded layout safe to use.
    ///
    /// Drops tabs whose sessions have gone, clamps split ratios, and brings `active_tab` into range.
    /// Everything here guards against a file that was valid when written and is not any more.
    pub fn sanitise(&mut self, session_exists: &dyn Fn(NodeId) -> bool) {
        let mut kept: Vec<TabLayout> = Vec::with_capacity(self.tabs.len());
        for tab in &self.tabs {
            if let Some(mut root) = tab.root.retain_sessions(session_exists) {
                root.clamp_ratios();
                kept.push(TabLayout {
                    title: tab.title.clone(),
                    root,
                });
            }
        }
        self.tabs = kept;

        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        }

        if !self.window.width.is_finite() || self.window.width <= 0.0 {
            self.window.width = WindowLayout::default().width;
        }
        if !self.window.height.is_finite() || self.window.height <= 0.0 {
            self.window.height = WindowLayout::default().height;
        }
        if !self.sidebar.width.is_finite() || self.sidebar.width <= 0.0 {
            self.sidebar.width = SidebarLayout::default().width;
        }
    }

    /// Total panes across every tab.
    pub fn pane_count(&self) -> usize {
        self.tabs.iter().map(|tab| tab.root.pane_count()).sum()
    }
}

impl Document for LayoutDoc {
    const VERSION: u32 = 1;
    const NAME: &'static str = "layout";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    fn split(first: PaneNode, second: PaneNode, ratio: f32) -> PaneNode {
        PaneNode::Split {
            axis: SplitAxis::Vertical,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    #[test]
    fn an_empty_layout_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("layout.toml");
        let original = LayoutDoc::default();
        store::save(&path, &original).expect("saves");
        assert_eq!(
            store::load::<LayoutDoc>(&path).expect("loads"),
            original
        );
    }

    #[test]
    fn a_nested_layout_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("layout.toml");

        let a = NodeId::new();
        let original = LayoutDoc {
            tabs: vec![
                TabLayout {
                    title: Some("pinned".to_string()),
                    root: split(
                        PaneNode::session(a),
                        split(
                            PaneNode::local_shell(None),
                            PaneNode::local_shell(Some("wsl:Ubuntu".to_string())),
                            0.4,
                        ),
                        0.6,
                    ),
                },
                TabLayout {
                    title: None,
                    root: PaneNode::local_shell(None),
                },
            ],
            active_tab: 1,
            ..Default::default()
        };

        store::save(&path, &original).expect("saves");
        let loaded: LayoutDoc = store::load(&path).expect("loads");
        assert_eq!(loaded, original);
        assert_eq!(loaded.pane_count(), 4);
    }

    #[test]
    fn pane_order_is_first_then_second() {
        let a = NodeId::new();
        let root = split(PaneNode::session(a), PaneNode::local_shell(None), 0.5);
        let panes = root.panes();
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0], &PaneContent::Session { session: a });
        assert!(matches!(panes[1], PaneContent::LocalShell { .. }));
    }

    #[test]
    fn ratios_are_clamped_into_a_grabbable_range() {
        let mut root = split(
            PaneNode::local_shell(None),
            PaneNode::local_shell(None),
            0.0,
        );
        root.clamp_ratios();
        match root {
            PaneNode::Split { ratio, .. } => assert_eq!(ratio, MIN_RATIO),
            _ => panic!("expected a split"),
        }
    }

    #[test]
    fn a_non_finite_ratio_becomes_a_half() {
        // NaN would propagate through the layout arithmetic and make the tab invisible.
        let mut root = split(
            PaneNode::local_shell(None),
            PaneNode::local_shell(None),
            f32::NAN,
        );
        root.clamp_ratios();
        match root {
            PaneNode::Split { ratio, .. } => assert_eq!(ratio, 0.5),
            _ => panic!("expected a split"),
        }
    }

    #[test]
    fn clamping_reaches_nested_splits() {
        let mut root = split(
            PaneNode::local_shell(None),
            split(PaneNode::local_shell(None), PaneNode::local_shell(None), 5.0),
            0.5,
        );
        root.clamp_ratios();
        let PaneNode::Split { second, .. } = &root else {
            panic!("expected a split");
        };
        match second.as_ref() {
            PaneNode::Split { ratio, .. } => assert_eq!(*ratio, MAX_RATIO),
            _ => panic!("expected a nested split"),
        }
    }

    #[test]
    fn a_pane_whose_session_is_gone_is_dropped() {
        let gone = NodeId::new();
        let root = PaneNode::session(gone);
        assert!(root.retain_sessions(&|_| false).is_none());
    }

    #[test]
    fn a_split_with_one_survivor_collapses_into_it() {
        // Leaving an empty half would render as a dead region the user cannot close.
        let alive = NodeId::new();
        let gone = NodeId::new();
        let root = split(PaneNode::session(alive), PaneNode::session(gone), 0.5);

        let kept = root
            .retain_sessions(&|id| id == alive)
            .expect("one survives");
        assert_eq!(kept, PaneNode::session(alive));
    }

    #[test]
    fn local_shells_survive_because_they_reference_nothing() {
        let root = PaneNode::local_shell(None);
        assert_eq!(root.retain_sessions(&|_| false), Some(root.clone()));
    }

    #[test]
    fn sanitising_drops_empty_tabs_and_fixes_the_active_index() {
        let gone = NodeId::new();
        let mut doc = LayoutDoc {
            tabs: vec![
                TabLayout {
                    title: None,
                    root: PaneNode::session(gone),
                },
                TabLayout {
                    title: None,
                    root: PaneNode::local_shell(None),
                },
            ],
            active_tab: 1,
            ..Default::default()
        };

        doc.sanitise(&|_| false);
        assert_eq!(doc.tabs.len(), 1);
        assert_eq!(doc.active_tab, 0);
    }

    #[test]
    fn sanitising_an_all_gone_layout_leaves_nothing_and_does_not_underflow() {
        let gone = NodeId::new();
        let mut doc = LayoutDoc {
            tabs: vec![TabLayout {
                title: None,
                root: PaneNode::session(gone),
            }],
            active_tab: 0,
            ..Default::default()
        };
        doc.sanitise(&|_| false);
        assert!(doc.tabs.is_empty());
        assert_eq!(doc.active_tab, 0);
    }

    #[test]
    fn sanitising_repairs_impossible_window_geometry() {
        let mut doc = LayoutDoc {
            window: WindowLayout {
                width: 0.0,
                height: f32::NAN,
                maximized: false,
            },
            sidebar: SidebarLayout {
                width: -10.0,
                ..Default::default()
            },
            ..Default::default()
        };
        doc.sanitise(&|_| true);
        assert_eq!(doc.window.width, WindowLayout::default().width);
        assert_eq!(doc.window.height, WindowLayout::default().height);
        assert_eq!(doc.sidebar.width, SidebarLayout::default().width);
    }

    #[test]
    fn an_unknown_key_in_a_layout_file_is_rejected() {
        assert!(toml::from_str::<LayoutDoc>("actve_tab = 1").is_err());
    }
}
