//! 前端抽象层。App 和具体前端实现都依赖这里，避免 app <-> tui 互相依赖。

use std::io;

use crate::protocol::content_query::RenderQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{SpaceId, ViewId};
use crate::protocol::revision::Revision;
use crate::protocol::scene::Scene;
use crate::protocol::viewport::ViewportCommand;

pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;

    fn render(
        &mut self,
        scene: &Scene,
        scene_revision: Revision,
        query: &dyn RenderQuery,
        focused: SpaceId,
    ) -> io::Result<()>;

    /// 处理依赖实际布局尺寸的视口命令，并返回本次解析出的逻辑行数。
    fn execute_viewport_command(
        &mut self,
        scene: &Scene,
        scene_revision: Revision,
        view: ViewId,
        command: ViewportCommand,
    ) -> io::Result<usize>;
}
