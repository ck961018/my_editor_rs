//! 前端抽象层。App 和具体前端实现都依赖这里，避免 app <-> tui 互相依赖。

#![allow(
    async_fn_in_trait,
    reason = "workspace-only static dispatch does not require a Send future contract"
)]

use std::io;

use modeleaf_protocol::content_query::RenderQuery;
use modeleaf_protocol::frontend_event::FrontendEvent;
use modeleaf_protocol::ids::{SpaceId, ViewId};
use modeleaf_protocol::revision::Revision;
use modeleaf_protocol::scene::Scene;
use modeleaf_protocol::viewport::{ResolvedViewportCommand, ViewportCommand};

pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;

    fn render(
        &mut self,
        scene: &Scene,
        scene_revision: Revision,
        query: &dyn RenderQuery,
        focused: SpaceId,
    ) -> io::Result<()>;

    /// 根据实际布局尺寸解析视口命令，不修改前端状态。
    fn resolve_viewport_command(
        &mut self,
        scene: &Scene,
        scene_revision: Revision,
        view: ViewId,
        cursor_row: usize,
        command: ViewportCommand,
    ) -> io::Result<ResolvedViewportCommand>;

    /// 在 app 提交整个有序结果后应用已解析的视口变化。
    fn apply_viewport_command(&mut self, view: ViewId, command: ResolvedViewportCommand);
}
