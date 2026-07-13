use std::collections::HashMap;

use crate::protocol::geometry::Size;
use crate::protocol::ids::{SpaceId, ViewId};
use crate::protocol::scene::{Scene, SpaceNode};
use crate::protocol::space::{Align, Arrangement, Axis, Layer, Sizing, Space, SpaceKind};

pub(crate) fn editor_scene(
    width: i32,
    height: i32,
    editor: ViewId,
    status: ViewId,
) -> (Scene, SpaceId) {
    let editor_space = SpaceId(0);
    let status_space = SpaceId(1);
    let root = SpaceId(2);
    let nodes = [
        content_node(editor_space, Some(root), editor, true, Sizing::Grow(1)),
        content_node(status_space, Some(root), status, false, Sizing::Fixed(1)),
        container_node(
            root,
            None,
            Axis::Vertical,
            vec![editor_space, status_space],
            Sizing::Grow(1),
        ),
    ]
    .into_iter()
    .map(|node| (node.id, node))
    .collect();
    (
        Scene::from_parts(root, Size { width, height }, nodes),
        editor_space,
    )
}

pub(crate) fn split_editor_scene(
    width: i32,
    height: i32,
    left_view: ViewId,
    status_view: ViewId,
    right_view: ViewId,
) -> (Scene, SpaceId, SpaceId) {
    let left = SpaceId(0);
    let status = SpaceId(1);
    let root = SpaceId(2);
    let right = SpaceId(3);
    let row = SpaceId(4);
    let nodes: HashMap<_, _> = [
        content_node(left, Some(row), left_view, true, Sizing::Grow(1)),
        content_node(status, Some(root), status_view, false, Sizing::Fixed(1)),
        content_node(right, Some(row), right_view, true, Sizing::Grow(1)),
        container_node(
            row,
            Some(root),
            Axis::Horizontal,
            vec![left, right],
            Sizing::Grow(1),
        ),
        container_node(
            root,
            None,
            Axis::Vertical,
            vec![row, status],
            Sizing::Grow(1),
        ),
    ]
    .into_iter()
    .map(|node| (node.id, node))
    .collect();
    (
        Scene::from_parts(root, Size { width, height }, nodes),
        left,
        right,
    )
}

fn content_node(
    id: SpaceId,
    parent: Option<SpaceId>,
    view: ViewId,
    focusable: bool,
    sizing: Sizing,
) -> SpaceNode {
    SpaceNode {
        id,
        parent,
        children: Vec::new(),
        space: Space {
            id,
            kind: SpaceKind::Content { view, focusable },
            sizing,
            layer: Layer::Base,
        },
    }
}

fn container_node(
    id: SpaceId,
    parent: Option<SpaceId>,
    direction: Axis,
    children: Vec<SpaceId>,
    sizing: Sizing,
) -> SpaceNode {
    SpaceNode {
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
    }
}
