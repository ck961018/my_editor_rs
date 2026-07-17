//! 前后端共享的 Scene 快照数据。树的构建与修改属于 app::scene_model。

use std::collections::HashMap;

use crate::protocol::geometry::Size;
use crate::protocol::ids::SpaceId;
use crate::protocol::space::Space;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ids::ViewId;
    use crate::protocol::space::{Layer, Sizing, SpaceKind};

    #[test]
    fn snapshot_exposes_root_size_and_nodes() {
        let root = SpaceId(7);
        let node = SpaceNode {
            parent: None,
            children: Vec::new(),
            space: Space {
                kind: SpaceKind::Content {
                    view: ViewId(3),
                    focusable: true,
                },
                sizing: Sizing::Grow(1),
                layer: Layer::Base,
            },
        };
        let scene = Scene::from_parts(
            root,
            Size {
                width: 80,
                height: 24,
            },
            [(root, node)].into(),
        );

        assert_eq!(scene.root(), root);
        assert_eq!(
            scene.size,
            Size {
                width: 80,
                height: 24
            }
        );
        assert!(scene.contains(root));
        assert!(matches!(
            scene.node(root).space.kind,
            SpaceKind::Content {
                view: ViewId(3),
                ..
            }
        ));
    }
}
