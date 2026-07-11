//! 前端抽象层。App 和具体前端实现都依赖这里，避免 app <-> tui 互相依赖。

use std::io;

use crate::protocol::content_query::RenderQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::SpaceId;
use crate::protocol::scene::Scene;

pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;

    fn render(
        &mut self,
        scene: &Scene,
        query: &dyn RenderQuery,
        focused: SpaceId,
    ) -> io::Result<()>;
}
