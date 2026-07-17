use std::collections::HashMap;

use taffy::prelude::*;

use crate::protocol::geometry::{Rect, Size as SceneSize};
use crate::protocol::ids::SpaceId;
use crate::protocol::revision::Revision;
use crate::protocol::scene::{Scene, SpaceNode};
use crate::protocol::space::{Align, Arrangement, Axis, Sizing, SpaceKind};
use crate::tui::resolved::{RenderItem, ResolvedScene};

pub struct TaffyEngine {
    tree: TaffyTree,
    cached_revision: Option<Revision>,
    cached_scene: Option<ResolvedScene>,
}

struct CollectOut {
    items: Vec<RenderItem>,
    order: u64,
}

impl TaffyEngine {
    pub fn new() -> Self {
        Self {
            tree: TaffyTree::new(),
            cached_revision: None,
            cached_scene: None,
        }
    }

    pub(super) fn layout(&mut self, scene: &Scene, revision: Revision) -> &ResolvedScene {
        if self.cached_revision != Some(revision) {
            self.tree = TaffyTree::new();
            let mut map: HashMap<SpaceId, NodeId> = HashMap::new();
            let root_node = self.build_node(scene, scene.root(), None, Some(scene.size), &mut map);
            let available = Size {
                width: AvailableSpace::Definite(scene.size.width as f32),
                height: AvailableSpace::Definite(scene.size.height as f32),
            };
            self.tree
                .compute_layout(root_node, available)
                .expect("taffy layout computation failed");
            let mut out = CollectOut {
                items: Vec::new(),
                order: 0,
            };
            self.collect(scene, scene.root(), None, (0, 0), &map, &mut out);
            self.cached_scene = Some(ResolvedScene { items: out.items });
            self.cached_revision = Some(revision);
        }

        self.cached_scene
            .as_ref()
            .expect("layout cache initialized for revision")
    }

    fn build_node(
        &mut self,
        scene: &Scene,
        sid: SpaceId,
        parent_axis: Option<Axis>,
        root_size: Option<SceneSize>,
        map: &mut HashMap<SpaceId, NodeId>,
    ) -> NodeId {
        let node = scene.node(sid);
        let style = style_for(node, parent_axis, root_size);
        let taffy_id = match &node.space.kind {
            SpaceKind::Container { arrangement } => {
                let axis = match arrangement {
                    Arrangement::Flex { direction, .. } => *direction,
                };
                let child_ids: Vec<NodeId> = node
                    .children
                    .iter()
                    .map(|c| self.build_node(scene, *c, Some(axis), None, map))
                    .collect();
                self.tree.new_with_children(style, &child_ids).unwrap()
            }
            SpaceKind::Content { .. } => self.tree.new_leaf(style).unwrap(),
        };
        map.insert(sid, taffy_id);
        taffy_id
    }

    fn collect(
        &self,
        scene: &Scene,
        sid: SpaceId,
        parent_clip: Option<Rect>,
        parent_origin: (i32, i32),
        map: &HashMap<SpaceId, NodeId>,
        out: &mut CollectOut,
    ) {
        let node = scene.node(sid);
        let taffy_id = map[&sid];
        let layout = self.tree.layout(taffy_id).expect("layout computed");
        let rect = Rect {
            x: parent_origin.0 + layout.location.x.round() as i32,
            y: parent_origin.1 + layout.location.y.round() as i32,
            width: layout.size.width.round() as i32,
            height: layout.size.height.round() as i32,
        };
        let clip = match parent_clip {
            Some(p) => p.intersect(&rect),
            None => Some(rect),
        };
        let view_id = match &node.space.kind {
            SpaceKind::Content { view, .. } => Some(*view),
            SpaceKind::Container { .. } => None,
        };
        if let Some(view_id) = view_id {
            out.items.push(RenderItem {
                space_id: sid,
                view_id,
                rect,
                clip,
                layer: node.space.layer,
                z_index: 0,
                order: out.order,
            });
            out.order += 1;
        }
        if let SpaceKind::Container { .. } = &node.space.kind {
            for c in &node.children {
                self.collect(scene, *c, clip, (rect.x, rect.y), map, out);
            }
        }
    }
}

impl Default for TaffyEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn style_for(node: &SpaceNode, parent_axis: Option<Axis>, root_size: Option<SceneSize>) -> Style {
    let mut style = Style::default();
    match (parent_axis, &node.space.sizing) {
        (Some(Axis::Vertical), Sizing::Fixed(x)) => {
            style.size.height = LengthPercentageAuto::length(*x as f32).into();
        }
        (Some(Axis::Horizontal), Sizing::Fixed(x)) => {
            style.size.width = LengthPercentageAuto::length(*x as f32).into();
        }
        (_, Sizing::Grow(w)) => {
            style.flex_grow = *w as f32;
        }
        (None, Sizing::Fixed(_)) => {}
    }
    if let Some(s) = root_size {
        style.size.width = LengthPercentageAuto::length(s.width as f32).into();
        style.size.height = LengthPercentageAuto::length(s.height as f32).into();
    }
    if let SpaceKind::Container { arrangement, .. } = &node.space.kind {
        let (direction, gap, align) = match arrangement {
            Arrangement::Flex {
                direction,
                gap,
                align,
            } => (*direction, *gap, *align),
        };
        style.display = Display::Flex;
        style.flex_direction = match direction {
            Axis::Vertical => FlexDirection::Column,
            Axis::Horizontal => FlexDirection::Row,
        };
        let gap_val = LengthPercentage::length(gap as f32);
        style.gap = Size {
            width: gap_val,
            height: gap_val,
        };
        style.align_items = match align {
            Align::Stretch => Some(AlignItems::STRETCH),
            Align::Start => Some(AlignItems::FLEX_START),
            Align::Center => Some(AlignItems::CENTER),
            Align::End => Some(AlignItems::FLEX_END),
        };
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::geometry::Size as ProtocolSize;
    use crate::protocol::ids::ViewId;
    use crate::protocol::scene::SpaceNode;
    use crate::protocol::space::{Layer, Space};
    use crate::tui::test_scene::{editor_scene, split_editor_scene};

    fn item_for(scene: &ResolvedScene, content: ViewId) -> &RenderItem {
        scene.items.iter().find(|i| i.view_id == content).unwrap()
    }

    fn nested_offset_scene() -> Scene {
        let top = SpaceId(0);
        let left = SpaceId(1);
        let right = SpaceId(2);
        let row = SpaceId(3);
        let root = SpaceId(4);
        let content = |id, parent, view, sizing| SpaceNode {
            id,
            parent: Some(parent),
            children: Vec::new(),
            space: Space {
                id,
                kind: SpaceKind::Content {
                    view,
                    focusable: true,
                },
                sizing,
                layer: Layer::Base,
            },
        };
        let container = |id, parent, direction, children, sizing| SpaceNode {
            id,
            parent,
            children,
            space: Space {
                id,
                kind: SpaceKind::Container {
                    arrangement: Arrangement::Flex {
                        direction,
                        gap: 0,
                        align: Align::Stretch,
                    },
                },
                sizing,
                layer: Layer::Base,
            },
        };
        Scene::from_parts(
            root,
            ProtocolSize {
                width: 20,
                height: 10,
            },
            [
                content(top, root, ViewId(0), Sizing::Fixed(2)),
                content(left, row, ViewId(1), Sizing::Grow(1)),
                content(right, row, ViewId(2), Sizing::Grow(1)),
                container(
                    row,
                    Some(root),
                    Axis::Horizontal,
                    vec![left, right],
                    Sizing::Grow(1),
                ),
                container(root, None, Axis::Vertical, vec![top, row], Sizing::Grow(1)),
            ]
            .into_iter()
            .map(|node| (node.id, node))
            .collect(),
        )
    }

    #[test]
    fn editor_grows_and_status_fixed() {
        let (scene, _) = editor_scene(80, 24, ViewId(0), ViewId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene, Revision(0));
        assert_eq!(
            item_for(resolved, ViewId(0)).rect,
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 23
            }
        );
        assert_eq!(
            item_for(resolved, ViewId(1)).rect,
            Rect {
                x: 0,
                y: 23,
                width: 80,
                height: 1
            }
        );
    }

    #[test]
    fn items_in_dfs_order() {
        let (scene, _) = editor_scene(80, 24, ViewId(0), ViewId(1));
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene, Revision(0));
        assert_eq!(resolved.items.len(), 2); // 仅 Content 进 items（container 不进）
        assert_eq!(resolved.items[0].view_id, ViewId(0));
        assert_eq!(resolved.items[1].view_id, ViewId(1));
    }

    #[test]
    fn resize_changes_geometry() {
        let (mut scene, _) = editor_scene(80, 24, ViewId(0), ViewId(1));
        scene.size.width = 100;
        scene.size.height = 40;
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene, Revision(0));
        assert_eq!(item_for(resolved, ViewId(0)).rect.height, 39);
        assert_eq!(item_for(resolved, ViewId(0)).rect.width, 100);
    }

    #[test]
    fn distinct_view_items_keep_their_source_space_ids() {
        let (scene, left, right) = split_editor_scene(20, 2, ViewId(0), ViewId(1), ViewId(2));

        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene, Revision(0));

        let sources: Vec<_> = resolved
            .items
            .iter()
            .filter(|item| item.view_id != ViewId(1))
            .map(|item| (item.view_id, item.space_id))
            .collect();
        assert_eq!(sources, vec![(ViewId(0), left), (ViewId(2), right)]);
    }

    #[test]
    fn scene_revision_is_the_only_layout_invalidation_key() {
        let (mut scene, _) = editor_scene(80, 24, ViewId(0), ViewId(1));
        let mut engine = TaffyEngine::new();

        assert_eq!(
            item_for(engine.layout(&scene, Revision(0)), ViewId(0))
                .rect
                .width,
            80
        );

        scene.size.width = 100;
        assert_eq!(
            item_for(engine.layout(&scene, Revision(0)), ViewId(0))
                .rect
                .width,
            80
        );
        assert_eq!(
            item_for(engine.layout(&scene, Revision(1)), ViewId(0))
                .rect
                .width,
            100
        );
    }

    #[test]
    fn nested_locations_are_accumulated_from_all_ancestors() {
        let scene = nested_offset_scene();
        let mut engine = TaffyEngine::new();

        let resolved = engine.layout(&scene, Revision(0));

        assert_eq!(item_for(resolved, ViewId(1)).rect.y, 2);
        assert_eq!(item_for(resolved, ViewId(2)).rect.y, 2);
        assert_eq!(item_for(resolved, ViewId(2)).rect.x, 10);
    }
}
