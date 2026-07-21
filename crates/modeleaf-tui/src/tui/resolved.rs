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
