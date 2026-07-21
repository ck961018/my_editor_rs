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
        (
            editor_space,
            content_node(Some(root), editor, true, Sizing::Grow(1)),
        ),
        (
            status_space,
            content_node(Some(root), status, false, Sizing::Fixed(1)),
        ),
        (
            root,
            container_node(
                None,
                Axis::Vertical,
                vec![editor_space, status_space],
                Sizing::Grow(1),
            ),
        ),
    ]
    .into_iter()
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
        (
            left,
            content_node(Some(row), left_view, true, Sizing::Grow(1)),
        ),
        (
            status,
            content_node(Some(root), status_view, false, Sizing::Fixed(1)),
        ),
        (
            right,
            content_node(Some(row), right_view, true, Sizing::Grow(1)),
        ),
        (
            row,
            container_node(
                Some(root),
                Axis::Horizontal,
                vec![left, right],
                Sizing::Grow(1),
            ),
        ),
        (
            root,
            container_node(None, Axis::Vertical, vec![row, status], Sizing::Grow(1)),
        ),
    ]
    .into_iter()
    .collect();
    (
        Scene::from_parts(root, Size { width, height }, nodes),
        left,
        right,
    )
}

fn content_node(
    parent: Option<SpaceId>,
    view: ViewId,
    focusable: bool,
    sizing: Sizing,
) -> SpaceNode {
    SpaceNode {
        parent,
        children: Vec::new(),
        space: Space {
            kind: SpaceKind::Content { view, focusable },
            sizing,
            layer: Layer::Base,
        },
    }
}

fn container_node(
    parent: Option<SpaceId>,
    direction: Axis,
    children: Vec<SpaceId>,
    sizing: Sizing,
) -> SpaceNode {
    SpaceNode {
        parent,
        children,
        space: Space {
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
