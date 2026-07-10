use std::collections::HashMap;
use std::collections::HashSet;

use crate::protocol::geometry::Size;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::space::{Align, Arrangement, Axis, Layer, Sizing, Space, SpaceKind};

#[derive(Clone)]
pub struct SpaceNode {
    #[allow(dead_code)] // 结构性 identity 字段，与 HashMap key 冗余但保留
    pub id: SpaceId,
    pub parent: Option<SpaceId>,
    pub children: Vec<SpaceId>,
    pub space: Space,
}

#[derive(Clone)]
pub struct Scene {
    pub root: SpaceId,
    pub size: Size,
    nodes: HashMap<SpaceId, SpaceNode>,
}

impl Scene {
    pub fn node(&self, id: SpaceId) -> &SpaceNode {
        self.nodes.get(&id).expect("space id exists")
    }
    #[allow(dead_code)] // 预留：v0.2 renderer 只读遍历，未来多场景编辑/space 增删时启用
    pub fn node_mut(&mut self, id: SpaceId) -> &mut SpaceNode {
        self.nodes.get_mut(&id).expect("space id exists")
    }
    pub fn resize(&mut self, width: i32, height: i32) {
        self.size = Size { width, height };
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BuildError {
    UnknownRoot,
    CycleDetected,
    DanglingChild,
}

pub struct SceneBuilder {
    nodes: HashMap<SpaceId, SpaceNode>,
    next_id: u64,
}

impl SceneBuilder {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            next_id: 0,
        }
    }

    fn alloc(&mut self, kind: SpaceKind) -> SpaceId {
        let id = SpaceId(self.next_id);
        self.next_id += 1;
        let children = match &kind {
            SpaceKind::Container { children, .. } => children.clone(),
            SpaceKind::Host { .. } => Vec::new(),
        };
        let node = SpaceNode {
            id,
            parent: None,
            children,
            space: Space {
                id,
                kind,
                sizing: Sizing::Grow(1),
                layer: Layer::Base,
            },
        };
        self.nodes.insert(id, node);
        id
    }

    pub fn host(&mut self, content: ContentId) -> SpaceHandle {
        SpaceHandle {
            id: self.alloc(SpaceKind::Host { content }),
        }
    }

    pub fn container(&mut self, arrangement: Arrangement, children: Vec<SpaceId>) -> SpaceHandle {
        SpaceHandle {
            id: self.alloc(SpaceKind::Container {
                arrangement,
                children,
            }),
        }
    }

    pub fn set_sizing(&mut self, id: SpaceId, sizing: Sizing) -> SpaceId {
        if let Some(node) = self.nodes.get_mut(&id) {
            node.space.sizing = sizing;
        }
        id
    }

    pub fn host_grow(&mut self, content: ContentId, weight: u32) -> SpaceId {
        let id = self.host(content).id;
        self.set_sizing(id, Sizing::Grow(weight))
    }

    pub fn host_fixed(&mut self, content: ContentId, size: i32) -> SpaceId {
        let id = self.host(content).id;
        self.set_sizing(id, Sizing::Fixed(size))
    }

    pub fn container_grow(
        &mut self,
        arrangement: Arrangement,
        children: Vec<SpaceId>,
        weight: u32,
    ) -> SpaceId {
        let id = self.container(arrangement, children).id;
        self.set_sizing(id, Sizing::Grow(weight))
    }

    pub fn snapshot(&mut self, root: SpaceId, size: Size) -> Result<Scene, BuildError> {
        if !self.nodes.contains_key(&root) {
            return Err(BuildError::UnknownRoot);
        }
        let mut visited: HashSet<SpaceId> = HashSet::new();
        let mut stack: Vec<SpaceId> = vec![root];
        while let Some(sid) = stack.pop() {
            if visited.contains(&sid) {
                return Err(BuildError::CycleDetected);
            }
            visited.insert(sid);
            let children = self
                .nodes
                .get(&sid)
                .ok_or(BuildError::DanglingChild)?
                .children
                .clone();
            for c in &children {
                if !self.nodes.contains_key(c) {
                    return Err(BuildError::DanglingChild);
                }
                if let Some(cnode) = self.nodes.get_mut(c) {
                    cnode.parent = Some(sid);
                }
                stack.push(*c);
            }
        }
        Ok(Scene {
            root,
            size,
            nodes: self.nodes.clone(),
        })
    }

    // 兼容包装：保留为预留 API（生产路径用 snapshot）。
    #[allow(dead_code)]
    pub fn finish(mut self, root: SpaceId, size: Size) -> Result<Scene, BuildError> {
        self.snapshot(root, size)
    }
}

impl Default for SceneBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SpaceHandle {
    pub id: SpaceId,
}
impl SpaceHandle {
    // 预留链式 API；生产路径用 host_grow/host_fixed。
    #[allow(dead_code)]
    pub fn fixed(self, b: &mut SceneBuilder, size: i32) -> SpaceId {
        if let Some(n) = b.nodes.get_mut(&self.id) {
            n.space.sizing = Sizing::Fixed(size);
        }
        self.id
    }
    // 预留链式 API；生产路径用 host_grow/host_fixed。
    #[allow(dead_code)]
    pub fn grow(self, b: &mut SceneBuilder, weight: u32) -> SpaceId {
        if let Some(n) = b.nodes.get_mut(&self.id) {
            n.space.sizing = Sizing::Grow(weight);
        }
        self.id
    }
}

/// 标准布局：root Vertical [editor Grow(1), status Fixed(1)]。
/// 返回 (Scene, editor_space_id)。
pub fn build_editor_scene(
    b: &mut SceneBuilder,
    width: i32,
    height: i32,
    editor: ContentId,
    status: ContentId,
) -> Result<(Scene, SpaceId), BuildError> {
    let ed = b.host_grow(editor, 1);
    let st = b.host_fixed(status, 1);
    let root = b.container_grow(
        Arrangement::Flex {
            direction: Axis::Vertical,
            gap: 0,
            align: Align::Stretch,
        },
        vec![ed, st],
        1,
    );
    let scene = b.snapshot(root, Size { width, height })?;
    Ok((scene, ed))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn build_editor_scene_has_two_hosts() {
        let mut builder = SceneBuilder::new();
        let (scene, editor_space) =
            build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
        let root = scene.node(scene.root);
        match &root.space.kind {
            SpaceKind::Container { children, .. } => assert_eq!(children.len(), 2),
            _ => panic!("root must be container"),
        }
        assert_eq!(editor_space, SpaceId(0));
    }

    #[test]
    fn snapshot_does_not_reset_next_space_id() {
        let mut builder = SceneBuilder::new();
        let (scene, editor_space) =
            build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
        assert_eq!(editor_space, SpaceId(0));
        let extra = builder.host_grow(ContentId(2), 1);
        assert_eq!(extra, SpaceId(3));
        assert!(scene.node(editor_space).space.id == SpaceId(0));
    }

    #[test]
    fn repeated_snapshot_keeps_allocating_after_existing_nodes() {
        let mut builder = SceneBuilder::new();
        let (scene, _) =
            build_editor_scene(&mut builder, 80, 24, ContentId(0), ContentId(1)).unwrap();
        let root = scene.root;
        let _second = builder
            .snapshot(
                root,
                Size {
                    width: 100,
                    height: 40,
                },
            )
            .unwrap();
        let extra = builder.host_fixed(ContentId(2), 1);
        assert_eq!(extra, SpaceId(3));
    }
}
