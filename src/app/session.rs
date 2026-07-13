use std::collections::HashMap;

use crate::app::dispatcher::Dispatcher;
use crate::app::view::View;
use crate::protocol::ids::{SpaceId, ViewId};
use crate::protocol::remote::Revision;
use crate::protocol::scene::{Scene, SceneBuilder};

pub(super) struct ClientSession {
    pub(super) scene: Scene,
    pub(super) scene_builder: SceneBuilder,
    pub(super) scene_revision: Revision,
    pub(super) views: HashMap<ViewId, View>,
    pub(super) next_view_id: u64,
    pub(super) focused: SpaceId,
    pub(super) dispatcher: Dispatcher,
}

impl ClientSession {
    pub fn new(
        scene: Scene,
        scene_builder: SceneBuilder,
        views: HashMap<ViewId, View>,
        next_view_id: u64,
        focused: SpaceId,
        dispatcher: Dispatcher,
    ) -> Self {
        Self {
            scene,
            scene_builder,
            scene_revision: Revision::default(),
            views,
            next_view_id,
            focused,
            dispatcher,
        }
    }
}
