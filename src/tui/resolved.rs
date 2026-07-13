use crate::protocol::geometry::Rect;
use crate::protocol::ids::{SpaceId, ViewId};
use crate::protocol::space::Layer;

#[derive(Clone)]
pub struct RenderItem {
    pub space_id: SpaceId,
    pub view_id: ViewId,
    pub rect: Rect,
    #[allow(dead_code)] // 预留布局原语，v0.2 renderer 不做 clip
    pub clip: Option<Rect>,
    #[allow(dead_code)] // 预留布局原语，v0.2 renderer 不读 layer/z/order
    pub layer: Layer,
    #[allow(dead_code)]
    pub z_index: i32,
    #[allow(dead_code)]
    pub order: u64,
}

pub struct ResolvedScene {
    pub items: Vec<RenderItem>,
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
