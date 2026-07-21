//! 后端 Scene 模型：分配 SpaceId、维护树不变量并生成协议快照。

use std::collections::{HashMap, HashSet};

use modeleaf_protocol::geometry::Size;
use modeleaf_protocol::ids::{SpaceId, ViewId};
use modeleaf_protocol::scene::{Scene, SpaceNode};
use modeleaf_protocol::space::{
    Align, Arrangement, Axis, Layer, Sizing, Space, SpaceKind, SplitDirection,
};

struct MutableScene {
    root: SpaceId,
    size: Size,
    nodes: HashMap<SpaceId, SpaceNode>,
}

impl MutableScene {
    fn take(scene: &mut Scene) -> Self {
        let placeholder = Scene::from_parts(scene.root(), scene.size, HashMap::new());
        let (root, size, nodes) = std::mem::replace(scene, placeholder).into_parts();
        Self { root, size, nodes }
    }

    fn into_scene(self) -> Scene {
        Scene::from_parts(self.root, self.size, self.nodes)
    }
}

fn contains_view_except(
    scene: &MutableScene,
    view: ViewId,
    excluded_space: Option<SpaceId>,
) -> bool {
    scene.nodes.iter().any(|(space, node)| {
        Some(*space) != excluded_space
            && matches!(&node.space.kind, SpaceKind::Content { view: current, .. } if *current == view)
    })
}

fn is_tree_valid(scene: &MutableScene) -> bool {
    let Some(root) = scene.nodes.get(&scene.root) else {
        return false;
    };
    if root.parent.is_some() {
        return false;
    }

    let mut visited = HashSet::new();
    let mut views = HashSet::new();
    let mut stack = vec![scene.root];
    while let Some(space) = stack.pop() {
        if !visited.insert(space) {
            return false;
        }
        let Some(node) = scene.nodes.get(&space) else {
            return false;
        };
        match &node.space.kind {
            SpaceKind::Container { .. } => {
                if node.children.is_empty() {
                    return false;
                }
                for child in &node.children {
                    let Some(child_node) = scene.nodes.get(child) else {
                        return false;
                    };
                    if child_node.parent != Some(space) {
                        return false;
                    }
                    stack.push(*child);
                }
            }
            SpaceKind::Content { .. } if !node.children.is_empty() => return false,
            SpaceKind::Content { view, .. } if !views.insert(*view) => return false,
            SpaceKind::Content { .. } => {}
        }
    }
    visited.len() == scene.nodes.len()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneError {
    UnknownSpace(SpaceId),
    ExpectedContentLeaf(SpaceId),
    DuplicateView(ViewId),
    CannotCloseRoot(SpaceId),
    InvalidTree,
}

pub struct SceneBuilder {
    next_space_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitResult {
    pub new_space: SpaceId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseResult {
    pub removed_space: SpaceId,
    pub surviving_neighbor: Option<SpaceId>,
}

impl SceneBuilder {
    pub fn new() -> Self {
        Self { next_space_id: 0 }
    }

    fn alloc(&mut self) -> SpaceId {
        let id = SpaceId(self.next_space_id);
        self.next_space_id = self
            .next_space_id
            .checked_add(1)
            .expect("space id overflow");
        id
    }

    fn add_view(
        &mut self,
        scene: &mut MutableScene,
        view: ViewId,
        focusable: bool,
        sizing: Sizing,
    ) -> SpaceId {
        self.add_node(scene, SpaceKind::Content { view, focusable }, sizing)
    }

    fn add_container(
        &mut self,
        scene: &mut MutableScene,
        arrangement: Arrangement,
        sizing: Sizing,
    ) -> SpaceId {
        self.add_node(scene, SpaceKind::Container { arrangement }, sizing)
    }

    fn add_node(&mut self, scene: &mut MutableScene, kind: SpaceKind, sizing: Sizing) -> SpaceId {
        let id = self.alloc();
        scene.nodes.insert(
            id,
            SpaceNode {
                parent: None,
                children: Vec::new(),
                space: Space {
                    kind,
                    sizing,
                    layer: Layer::Base,
                },
            },
        );
        id
    }

    fn insert_child(
        &mut self,
        scene: &mut MutableScene,
        parent: SpaceId,
        child: SpaceId,
        index: usize,
    ) {
        scene
            .nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children
            .insert(index, child);
        scene.nodes.get_mut(&child).expect("child exists").parent = Some(parent);
    }

    fn replace_child(
        &mut self,
        scene: &mut MutableScene,
        parent: SpaceId,
        index: usize,
        child: SpaceId,
    ) {
        scene
            .nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children[index] = child;
        scene.nodes.get_mut(&child).expect("child exists").parent = Some(parent);
    }

    fn set_children(&mut self, scene: &mut MutableScene, parent: SpaceId, children: &[SpaceId]) {
        scene
            .nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children = children.to_vec();
        for child in children {
            scene.nodes.get_mut(child).expect("child exists").parent = Some(parent);
        }
    }

    pub fn split(
        &mut self,
        scene: &mut Scene,
        target: SpaceId,
        view: ViewId,
        focusable: bool,
        direction: SplitDirection,
    ) -> Result<SplitResult, SceneError> {
        let mut draft = MutableScene::take(scene);
        let result = self.split_draft(&mut draft, target, view, focusable, direction);
        *scene = draft.into_scene();
        result
    }

    fn split_draft(
        &mut self,
        scene: &mut MutableScene,
        target: SpaceId,
        view: ViewId,
        focusable: bool,
        direction: SplitDirection,
    ) -> Result<SplitResult, SceneError> {
        if !is_tree_valid(scene) {
            return Err(SceneError::InvalidTree);
        }
        let target_node = scene
            .nodes
            .get(&target)
            .ok_or(SceneError::UnknownSpace(target))?;
        if !matches!(&target_node.space.kind, SpaceKind::Content { .. })
            || !target_node.children.is_empty()
        {
            return Err(SceneError::ExpectedContentLeaf(target));
        }
        if contains_view_except(scene, view, None) {
            return Err(SceneError::DuplicateView(view));
        }

        let target_parent = target_node.parent;
        let target_sizing = target_node.space.sizing.clone();
        let (parent_axis, target_index) = match target_parent {
            Some(parent) => {
                let parent_node = scene.nodes.get(&parent).ok_or(SceneError::InvalidTree)?;
                let arrangement = match &parent_node.space.kind {
                    SpaceKind::Container { arrangement } => arrangement,
                    SpaceKind::Content { .. } => return Err(SceneError::InvalidTree),
                };
                let index = parent_node
                    .children
                    .iter()
                    .position(|child| *child == target)
                    .ok_or(SceneError::InvalidTree)?;
                let axis = match arrangement {
                    Arrangement::Flex { direction, .. } => *direction,
                };
                (Some(axis), Some(index))
            }
            None if scene.root == target => (None, None),
            None => return Err(SceneError::InvalidTree),
        };

        let new_space = self.add_view(scene, view, focusable, Sizing::Grow(1));
        if parent_axis == Some(direction.axis()) {
            let parent = target_parent.expect("matching axis has a parent");
            let index = target_index.expect("matching axis has an index")
                + usize::from(!direction.inserts_before());
            self.insert_child(scene, parent, new_space, index);
        } else {
            let container = self.add_container(
                scene,
                Arrangement::Flex {
                    direction: direction.axis(),
                    gap: 0,
                    align: Align::Stretch,
                },
                target_sizing,
            );
            let children = if direction.inserts_before() {
                [new_space, target]
            } else {
                [target, new_space]
            };

            if let Some(parent) = target_parent {
                self.replace_child(
                    scene,
                    parent,
                    target_index.expect("parent has target index"),
                    container,
                );
            } else {
                scene.root = container;
            }
            scene
                .nodes
                .get_mut(&target)
                .expect("validated target exists")
                .space
                .sizing = Sizing::Grow(1);
            self.set_children(scene, container, &children);
        }

        debug_assert!(is_tree_valid(scene));
        Ok(SplitResult { new_space })
    }

    pub fn close(&mut self, scene: &mut Scene, target: SpaceId) -> Result<CloseResult, SceneError> {
        let mut draft = MutableScene::take(scene);
        let result = self.close_draft(&mut draft, target);
        *scene = draft.into_scene();
        result
    }

    fn close_draft(
        &mut self,
        scene: &mut MutableScene,
        target: SpaceId,
    ) -> Result<CloseResult, SceneError> {
        if !is_tree_valid(scene) {
            return Err(SceneError::InvalidTree);
        }

        let target_node = scene
            .nodes
            .get(&target)
            .ok_or(SceneError::UnknownSpace(target))?;
        if !matches!(&target_node.space.kind, SpaceKind::Content { .. })
            || !target_node.children.is_empty()
        {
            return Err(SceneError::ExpectedContentLeaf(target));
        }

        let parent = target_node
            .parent
            .ok_or(SceneError::CannotCloseRoot(target))?;
        let (target_index, surviving_neighbor) = {
            let parent_node = scene.nodes.get(&parent).ok_or(SceneError::InvalidTree)?;
            if !matches!(&parent_node.space.kind, SpaceKind::Container { .. }) {
                return Err(SceneError::InvalidTree);
            }
            let index = parent_node
                .children
                .iter()
                .position(|child| *child == target)
                .ok_or(SceneError::InvalidTree)?;
            let neighbor = parent_node
                .children
                .get(index + 1)
                .copied()
                .or_else(|| {
                    index
                        .checked_sub(1)
                        .and_then(|previous| parent_node.children.get(previous).copied())
                })
                .ok_or(SceneError::InvalidTree)?;
            (index, neighbor)
        };

        scene
            .nodes
            .get_mut(&parent)
            .expect("validated parent exists")
            .children
            .remove(target_index);
        scene.nodes.remove(&target);

        let mut container = parent;
        loop {
            let (remaining, grandparent, container_sizing) = {
                let node = scene
                    .nodes
                    .get(&container)
                    .expect("validated container exists");
                if node.children.len() != 1 {
                    break;
                }
                (node.children[0], node.parent, node.space.sizing.clone())
            };

            scene
                .nodes
                .get_mut(&remaining)
                .expect("validated remaining child exists")
                .space
                .sizing = container_sizing;

            if let Some(grandparent) = grandparent {
                let parent_index = scene
                    .nodes
                    .get(&grandparent)
                    .expect("validated grandparent exists")
                    .children
                    .iter()
                    .position(|child| *child == container)
                    .expect("validated container is linked to grandparent");
                self.replace_child(scene, grandparent, parent_index, remaining);
                scene.nodes.remove(&container);
                container = grandparent;
            } else {
                scene.root = remaining;
                scene
                    .nodes
                    .get_mut(&remaining)
                    .expect("validated remaining child exists")
                    .parent = None;
                scene.nodes.remove(&container);
                break;
            }
        }

        debug_assert!(is_tree_valid(scene));
        Ok(CloseResult {
            removed_space: target,
            surviving_neighbor: Some(surviving_neighbor),
        })
    }

    pub fn replace_view(
        &mut self,
        scene: &mut Scene,
        target: SpaceId,
        view: ViewId,
        focusable: bool,
    ) -> Result<(), SceneError> {
        let mut draft = MutableScene::take(scene);
        let result = self.replace_view_draft(&mut draft, target, view, focusable);
        *scene = draft.into_scene();
        result
    }

    fn replace_view_draft(
        &mut self,
        scene: &mut MutableScene,
        target: SpaceId,
        view: ViewId,
        focusable: bool,
    ) -> Result<(), SceneError> {
        if !is_tree_valid(scene) {
            return Err(SceneError::InvalidTree);
        }

        let node = scene
            .nodes
            .get(&target)
            .ok_or(SceneError::UnknownSpace(target))?;
        if !node.children.is_empty() || !matches!(&node.space.kind, SpaceKind::Content { .. }) {
            return Err(SceneError::ExpectedContentLeaf(target));
        }
        if contains_view_except(scene, view, Some(target)) {
            return Err(SceneError::DuplicateView(view));
        }

        let node = scene.nodes.get_mut(&target).expect("validated node exists");
        let SpaceKind::Content {
            view: current_view,
            focusable: current_focusable,
        } = &mut node.space.kind
        else {
            unreachable!("validated content leaf")
        };
        *current_view = view;
        *current_focusable = focusable;
        debug_assert!(is_tree_valid(scene));
        Ok(())
    }

    pub fn set_sizing(
        &mut self,
        scene: &mut Scene,
        target: SpaceId,
        sizing: Sizing,
    ) -> Result<(), SceneError> {
        let mut draft = MutableScene::take(scene);
        let result = self.set_sizing_draft(&mut draft, target, sizing);
        *scene = draft.into_scene();
        result
    }

    fn set_sizing_draft(
        &mut self,
        scene: &mut MutableScene,
        target: SpaceId,
        sizing: Sizing,
    ) -> Result<(), SceneError> {
        if !is_tree_valid(scene) {
            return Err(SceneError::InvalidTree);
        }

        let node = scene
            .nodes
            .get_mut(&target)
            .ok_or(SceneError::UnknownSpace(target))?;
        node.space.sizing = sizing;

        debug_assert!(is_tree_valid(scene));
        Ok(())
    }
}

impl Default for SceneBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn build_editor_scene(
    builder: &mut SceneBuilder,
    width: i32,
    height: i32,
    editor: ViewId,
    status: ViewId,
) -> Result<(Scene, SpaceId), SceneError> {
    if editor == status {
        return Err(SceneError::DuplicateView(editor));
    }
    let mut scene = MutableScene {
        root: SpaceId(u64::MAX),
        size: Size { width, height },
        nodes: HashMap::new(),
    };
    let editor_space = builder.add_view(&mut scene, editor, true, Sizing::Grow(1));
    let status_space = builder.add_view(&mut scene, status, false, Sizing::Fixed(1));
    let root = builder.add_container(
        &mut scene,
        Arrangement::Flex {
            direction: Axis::Vertical,
            gap: 0,
            align: Align::Stretch,
        },
        Sizing::Grow(1),
    );
    scene.root = root;
    builder.set_children(&mut scene, root, &[editor_space, status_space]);
    debug_assert!(is_tree_valid(&scene));
    Ok((scene.into_scene(), editor_space))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "space id overflow")]
    fn space_id_allocation_has_an_explicit_overflow_policy() {
        let mut builder = SceneBuilder {
            next_space_id: u64::MAX,
        };

        builder.alloc();
    }

    #[test]
    fn standard_scene_marks_editor_focusable_and_status_inert() {
        let mut builder = SceneBuilder::new();
        let (scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();

        assert!(content_focusable(&scene, editor));
        let status = scene.node(scene.root()).children[1];
        assert!(!content_focusable(&scene, status));
    }

    #[test]
    fn duplicate_view_is_rejected_without_mutating_the_scene() {
        let mut builder = SceneBuilder::new();
        let (mut scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        let root_children = scene.node(scene.root()).children.clone();

        assert_eq!(
            builder.split(&mut scene, editor, ViewId(1), true, SplitDirection::Right,),
            Err(SceneError::DuplicateView(ViewId(1)))
        );
        assert_eq!(scene.node(scene.root()).children, root_children);
        assert_tree_valid(&scene);
    }

    #[test]
    fn standard_scene_rejects_duplicate_view_ids_before_allocating_spaces() {
        let mut builder = SceneBuilder::new();

        assert!(matches!(
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(0)),
            Err(SceneError::DuplicateView(ViewId(0)))
        ));
        let (scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();

        assert_eq!(editor, SpaceId(0));
        assert_tree_valid(&scene);
    }

    #[test]
    fn split_on_matching_axis_inserts_a_sibling_and_advances_id() {
        let mut builder = SceneBuilder::new();
        let (mut scene, _) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        let status = scene.node(scene.root()).children[1];

        let result = builder
            .split(&mut scene, status, ViewId(2), false, SplitDirection::Down)
            .unwrap();

        assert_eq!(result.new_space, SpaceId(3));
        assert_eq!(
            scene.node(status).parent,
            scene.node(result.new_space).parent
        );
        assert_tree_valid(&scene);
    }

    #[test]
    fn split_on_different_axis_wraps_target_in_a_new_container() {
        let mut builder = SceneBuilder::new();
        let (mut scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();

        let result = builder
            .split(&mut scene, editor, ViewId(2), true, SplitDirection::Right)
            .unwrap();

        let parent = scene.node(editor).parent.expect("split parent exists");
        assert_eq!(parent, scene.node(result.new_space).parent.unwrap());
        assert!(matches!(
            &scene.node(parent).space.kind,
            SpaceKind::Container {
                arrangement: Arrangement::Flex {
                    direction: Axis::Horizontal,
                    ..
                }
            }
        ));
        assert_tree_valid(&scene);
    }

    #[test]
    fn close_collapses_single_child_container_and_updates_root() {
        let mut builder = SceneBuilder::new();
        let (mut scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        let status = scene.node(scene.root()).children[1];

        let closed = builder.close(&mut scene, status).unwrap();

        assert_eq!(closed.removed_space, status);
        assert_eq!(closed.surviving_neighbor, Some(editor));
        assert_eq!(scene.root(), editor);
        assert_tree_valid(&scene);
    }

    #[test]
    fn replace_view_keeps_space_id_and_changes_focusability() {
        let mut builder = SceneBuilder::new();
        let (mut scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();

        builder
            .replace_view(&mut scene, editor, ViewId(9), false)
            .unwrap();

        assert!(matches!(
            &scene.node(editor).space.kind,
            SpaceKind::Content {
                view: ViewId(9),
                focusable: false,
            }
        ));
    }

    #[test]
    fn set_sizing_changes_only_the_requested_space() {
        let mut builder = SceneBuilder::new();
        let (mut scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        let status = scene.node(scene.root()).children[1];

        builder
            .set_sizing(&mut scene, editor, Sizing::Fixed(12))
            .unwrap();

        assert!(matches!(scene.node(editor).space.sizing, Sizing::Fixed(12)));
        assert!(matches!(scene.node(status).space.sizing, Sizing::Fixed(1)));
    }

    #[test]
    fn orthogonal_split_keeps_parent_axis_sizing_on_wrapper() {
        let mut builder = SceneBuilder::new();
        let (mut scene, _) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        let status = scene.node(scene.root()).children[1];

        let split = builder
            .split(&mut scene, status, ViewId(2), false, SplitDirection::Right)
            .unwrap();
        let wrapper = scene.node(status).parent.unwrap();

        assert!(matches!(scene.node(wrapper).space.sizing, Sizing::Fixed(1)));
        assert!(matches!(scene.node(status).space.sizing, Sizing::Grow(1)));
        assert!(matches!(
            scene.node(split.new_space).space.sizing,
            Sizing::Grow(1)
        ));
    }

    #[test]
    fn closing_orthogonal_split_restores_wrapper_sizing_to_survivor() {
        let mut builder = SceneBuilder::new();
        let (mut scene, _) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        let status = scene.node(scene.root()).children[1];
        let split = builder
            .split(&mut scene, status, ViewId(2), false, SplitDirection::Right)
            .unwrap();

        builder.close(&mut scene, split.new_space).unwrap();

        assert!(matches!(scene.node(status).space.sizing, Sizing::Fixed(1)));
        assert_tree_valid(&scene);
    }

    #[test]
    fn failed_split_leaves_tree_and_next_id_unchanged() {
        let mut builder = SceneBuilder::new();
        let (mut scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();

        assert_eq!(
            builder.split(
                &mut scene,
                SpaceId(999),
                ViewId(2),
                true,
                SplitDirection::Right,
            ),
            Err(SceneError::UnknownSpace(SpaceId(999)))
        );

        let split = builder
            .split(&mut scene, editor, ViewId(2), true, SplitDirection::Right)
            .unwrap();
        assert_eq!(split.new_space, SpaceId(3));
    }

    #[test]
    fn deleted_space_ids_are_not_reused() {
        let mut builder = SceneBuilder::new();
        let (mut scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ViewId(0), ViewId(1)).unwrap();
        let first = builder
            .split(&mut scene, editor, ViewId(2), true, SplitDirection::Right)
            .unwrap();
        builder.close(&mut scene, first.new_space).unwrap();

        let second = builder
            .split(&mut scene, editor, ViewId(3), true, SplitDirection::Right)
            .unwrap();
        assert_eq!(second.new_space, SpaceId(5));
    }

    fn content_focusable(scene: &Scene, space: SpaceId) -> bool {
        match &scene.node(space).space.kind {
            SpaceKind::Content { focusable, .. } => *focusable,
            SpaceKind::Container { .. } => panic!("space must be content"),
        }
    }

    fn assert_tree_valid(scene: &Scene) {
        let mut visited = HashSet::new();
        let mut stack = vec![scene.root()];

        while let Some(space) = stack.pop() {
            assert!(visited.insert(space), "space visited twice: {space:?}");
            let node = scene.node(space);
            match &node.space.kind {
                SpaceKind::Container { .. } => {
                    for child in &node.children {
                        assert_eq!(scene.node(*child).parent, Some(space));
                        stack.push(*child);
                    }
                }
                SpaceKind::Content { .. } => assert!(node.children.is_empty()),
            }
        }

        assert_eq!(visited.len(), scene.nodes().count());
    }
}
