use std::collections::HashMap;

use taffy::prelude::*;

use crate::protocol::geometry::{Rect, Size as SceneSize};
use crate::protocol::ids::SpaceId;
use crate::protocol::scene::{Scene, SpaceNode};
use crate::protocol::space::{Align, Arrangement, Axis, Sizing, SpaceKind};
use crate::tui::resolved::{RenderItem, ResolvedScene};

pub struct TaffyEngine {
    tree: TaffyTree,
}

struct CollectOut {
    items: Vec<RenderItem>,
    order: u64,
}

impl TaffyEngine {
    pub fn new() -> Self {
        Self {
            tree: TaffyTree::new(),
        }
    }

    pub fn layout(&mut self, scene: &Scene) -> ResolvedScene {
        self.tree = TaffyTree::new();
        let mut map: HashMap<SpaceId, NodeId> = HashMap::new();
        let root_node = self.build_node(scene, scene.root(), None, Some(scene.size), &mut map);
        let available = Size {
            width: AvailableSpace::Definite(scene.size.width as f32),
            height: AvailableSpace::Definite(scene.size.height as f32),
        };
        let _ = self.tree.compute_layout(root_node, available);
        let mut out = CollectOut {
            items: Vec::new(),
            order: 0,
        };
        self.collect(scene, scene.root(), None, &map, &mut out);
        ResolvedScene { items: out.items }
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
        map: &HashMap<SpaceId, NodeId>,
        out: &mut CollectOut,
    ) {
        let node = scene.node(sid);
        let taffy_id = map[&sid];
        let layout = self.tree.layout(taffy_id).expect("layout computed");
        let rect = Rect {
            x: layout.location.x.round() as i32,
            y: layout.location.y.round() as i32,
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
                self.collect(scene, *c, clip, map, out);
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
    use crate::protocol::ids::ViewId;
    use crate::protocol::scene::{SceneBuilder, build_editor_scene};
    use crate::protocol::space::SplitDirection;

    fn item_for(scene: &ResolvedScene, content: ViewId) -> &RenderItem {
        scene.items.iter().find(|i| i.view_id == content).unwrap()
    }

    #[test]
    fn editor_grows_and_status_fixed() {
        let mut builder = SceneBuilder::new();
        let (scene, _) = build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene);
        assert_eq!(
            item_for(&resolved, ViewId(0)).rect,
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 23
            }
        );
        assert_eq!(
            item_for(&resolved, ViewId(1)).rect,
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
        let mut builder = SceneBuilder::new();
        let (scene, _) = build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene);
        assert_eq!(resolved.items.len(), 2); // 仅 Content 进 items（container 不进）
        assert_eq!(resolved.items[0].view_id, ViewId(0));
        assert_eq!(resolved.items[1].view_id, ViewId(1));
    }

    #[test]
    fn resize_changes_geometry() {
        let mut builder = SceneBuilder::new();
        let (mut scene, _) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        scene.resize(100, 40);
        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene);
        assert_eq!(item_for(&resolved, ViewId(0)).rect.height, 39);
        assert_eq!(item_for(&resolved, ViewId(0)).rect.width, 100);
    }

    #[test]
    fn distinct_view_items_keep_their_source_space_ids() {
        let mut builder = SceneBuilder::new();
        let (mut scene, left) =
            build_editor_scene(&mut builder, 20, 2, ViewId(0), ViewId(1)).unwrap();
        let right = builder
            .split(&mut scene, left, ViewId(2), true, SplitDirection::Right)
            .unwrap()
            .new_space;

        let mut engine = TaffyEngine::new();
        let resolved = engine.layout(&scene);

        let sources: Vec<_> = resolved
            .items
            .iter()
            .filter(|item| item.view_id != ViewId(1))
            .map(|item| (item.view_id, item.space_id))
            .collect();
        assert_eq!(sources, vec![(ViewId(0), left), (ViewId(2), right)]);
    }
}
