use crate::protocol::geometry::Rect;
use crate::protocol::ids::{SpaceId, ViewId};
use crate::protocol::space::Layer;

#[derive(Clone)]
pub(super) struct RenderItem {
    pub(super) space_id: SpaceId,
    pub(super) view_id: ViewId,
    pub(super) rect: Rect,
    #[expect(
        dead_code,
        reason = "clipping metadata is retained at the layout-render boundary"
    )]
    pub(super) clip: Option<Rect>,
    #[expect(
        dead_code,
        reason = "layer metadata is retained at the layout-render boundary"
    )]
    pub(super) layer: Layer,
    #[expect(
        dead_code,
        reason = "stacking metadata is retained at the layout-render boundary"
    )]
    pub(super) z_index: i32,
    #[expect(
        dead_code,
        reason = "stable layout order is retained at the layout-render boundary"
    )]
    pub(super) order: u64,
}

pub(super) struct ResolvedScene {
    pub(super) items: Vec<RenderItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_item_holds_fields() {
        let it = RenderItem {
            space_id: SpaceId(1),
            view_id: ViewId(0),
            rect: Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 23,
            },
            clip: None,
            layer: Layer::Base,
            z_index: 0,
            order: 0,
        };
        assert_eq!(it.space_id, SpaceId(1));
        assert_eq!(it.view_id, ViewId(0));
        assert_eq!(it.rect.width, 80);
    }
}
