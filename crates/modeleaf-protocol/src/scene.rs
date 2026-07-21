//! 前后端共享的 Scene 快照数据。树的构建与修改属于 app::scene_model。

use std::collections::HashMap;

use crate::geometry::Size;
use crate::ids::SpaceId;
use crate::space::Space;

#[derive(Clone)]
pub struct SpaceNode {
    pub parent: Option<SpaceId>,
    pub children: Vec<SpaceId>,
    pub space: Space,
}

#[derive(Clone)]
pub struct Scene {
    pub(crate) root: SpaceId,
    pub size: Size,
    pub(crate) nodes: HashMap<SpaceId, SpaceNode>,
}

impl Scene {
    /// 从完整快照数据构造 Scene；nodes 的 HashMap key 是 Space identity 的唯一真相源。
    pub fn from_parts(root: SpaceId, size: Size, nodes: HashMap<SpaceId, SpaceNode>) -> Self {
        Self { root, size, nodes }
    }

    pub fn root(&self) -> SpaceId {
        self.root
    }

    pub fn contains(&self, id: SpaceId) -> bool {
        self.nodes.contains_key(&id)
    }

    pub fn node(&self, id: SpaceId) -> &SpaceNode {
        self.nodes.get(&id).expect("space id exists")
    }

    pub fn nodes(&self) -> impl Iterator<Item = (SpaceId, &SpaceNode)> {
        self.nodes.iter().map(|(id, node)| (*id, node))
    }

    pub fn into_parts(self) -> (SpaceId, Size, HashMap<SpaceId, SpaceNode>) {
        (self.root, self.size, self.nodes)
    }
}
