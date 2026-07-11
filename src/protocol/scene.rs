use std::collections::{HashMap, HashSet};

use crate::protocol::geometry::Size;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::space::{
    Align, Arrangement, Axis, Layer, Sizing, Space, SpaceKind, SplitDirection,
};

#[derive(Clone)]
pub struct SpaceNode {
    #[allow(dead_code)]
    pub id: SpaceId,
    pub parent: Option<SpaceId>,
    pub children: Vec<SpaceId>,
    pub space: Space,
}

pub struct Scene {
    root: SpaceId,
    pub size: Size,
    nodes: HashMap<SpaceId, SpaceNode>,
}

impl Scene {
    pub fn root(&self) -> SpaceId {
        self.root
    }

    pub fn contains(&self, id: SpaceId) -> bool {
        self.nodes.contains_key(&id)
    }

    pub fn node(&self, id: SpaceId) -> &SpaceNode {
        self.nodes.get(&id).expect("space id exists")
    }

    pub fn resize(&mut self, width: i32, height: i32) {
        self.size = Size { width, height };
    }

    fn is_tree_valid(&self) -> bool {
        let Some(root) = self.nodes.get(&self.root) else {
            return false;
        };
        if root.parent.is_some() {
            return false;
        }

        let mut visited = HashSet::new();
        let mut stack = vec![self.root];
        while let Some(space) = stack.pop() {
            if !visited.insert(space) {
                return false;
            }
            let Some(node) = self.nodes.get(&space) else {
                return false;
            };
            match &node.space.kind {
                SpaceKind::Container { .. } => {
                    for child in &node.children {
                        let Some(child_node) = self.nodes.get(child) else {
                            return false;
                        };
                        if child_node.parent != Some(space) {
                            return false;
                        }
                        stack.push(*child);
                    }
                }
                SpaceKind::Content { .. } if !node.children.is_empty() => return false,
                SpaceKind::Content { .. } => {}
            }
        }
        visited.len() == self.nodes.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneError {
    UnknownSpace(SpaceId),
    ExpectedContentLeaf(SpaceId),
    InvalidTree,
}

pub struct SceneBuilder {
    next_space_id: u64,
}

pub struct SplitResult {
    pub new_space: SpaceId,
}

impl SceneBuilder {
    pub fn new() -> Self {
        Self { next_space_id: 0 }
    }

    fn alloc(&mut self) -> SpaceId {
        let id = SpaceId(self.next_space_id);
        self.next_space_id += 1;
        id
    }

    fn add_content(
        &mut self,
        scene: &mut Scene,
        content: ContentId,
        focusable: bool,
        sizing: Sizing,
    ) -> SpaceId {
        self.add_node(scene, SpaceKind::Content { content, focusable }, sizing)
    }

    fn add_container(
        &mut self,
        scene: &mut Scene,
        arrangement: Arrangement,
        sizing: Sizing,
    ) -> SpaceId {
        self.add_node(scene, SpaceKind::Container { arrangement }, sizing)
    }

    fn add_node(&mut self, scene: &mut Scene, kind: SpaceKind, sizing: Sizing) -> SpaceId {
        let id = self.alloc();
        scene.nodes.insert(
            id,
            SpaceNode {
                id,
                parent: None,
                children: Vec::new(),
                space: Space {
                    id,
                    kind,
                    sizing,
                    layer: Layer::Base,
                },
            },
        );
        id
    }

    fn insert_child(&mut self, scene: &mut Scene, parent: SpaceId, child: SpaceId, index: usize) {
        scene
            .nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children
            .insert(index, child);
        scene.nodes.get_mut(&child).expect("child exists").parent = Some(parent);
    }

    fn replace_child(&mut self, scene: &mut Scene, parent: SpaceId, index: usize, child: SpaceId) {
        scene
            .nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children[index] = child;
        scene.nodes.get_mut(&child).expect("child exists").parent = Some(parent);
    }

    fn set_children(&mut self, scene: &mut Scene, parent: SpaceId, children: &[SpaceId]) {
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
        content: ContentId,
        focusable: bool,
        direction: SplitDirection,
    ) -> Result<SplitResult, SceneError> {
        let target_node = scene
            .nodes
            .get(&target)
            .ok_or(SceneError::UnknownSpace(target))?;
        if !matches!(&target_node.space.kind, SpaceKind::Content { .. })
            || !target_node.children.is_empty()
        {
            return Err(SceneError::ExpectedContentLeaf(target));
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

        let new_space = self.add_content(scene, content, focusable, Sizing::Grow(1));
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
            self.set_children(scene, container, &children);
        }

        debug_assert!(scene.is_tree_valid());
        Ok(SplitResult { new_space })
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
    editor: ContentId,
    status: ContentId,
) -> Result<(Scene, SpaceId), SceneError> {
    let mut scene = Scene {
        root: SpaceId(u64::MAX),
        size: Size { width, height },
        nodes: HashMap::new(),
    };
    let editor_space = builder.add_content(&mut scene, editor, true, Sizing::Grow(1));
    let status_space = builder.add_content(&mut scene, status, false, Sizing::Fixed(1));
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
    debug_assert!(scene.is_tree_valid());
    Ok((scene, editor_space))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_scene_marks_editor_focusable_and_status_inert() {
        let mut builder = SceneBuilder::new();
        let (scene, editor) =
            build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();

        assert!(content_focusable(&scene, editor));
        let status = scene.node(scene.root()).children[1];
        assert!(!content_focusable(&scene, status));
    }

    #[test]
    fn split_on_matching_axis_inserts_a_sibling_and_advances_id() {
        let mut builder = SceneBuilder::new();
        let (mut scene, _) =
            build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
        let status = scene.node(scene.root()).children[1];

        let result = builder
            .split(
                &mut scene,
                status,
                ContentId(1),
                false,
                SplitDirection::Down,
            )
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
            build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();

        let result = builder
            .split(
                &mut scene,
                editor,
                ContentId(0),
                true,
                SplitDirection::Right,
            )
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

        assert_eq!(visited.len(), scene.nodes.len());
    }
}
