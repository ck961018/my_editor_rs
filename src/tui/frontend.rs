//! TUI 的 Frontend 实现：SceneRenderer + `Output<W>`。

use std::io;

use crate::frontend::Frontend;
use crate::protocol::content_query::RenderQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{SpaceId, ViewId};
use crate::protocol::revision::Revision;
use crate::protocol::scene::Scene;
use crate::protocol::viewport::{ResolvedViewportCommand, ViewportCommand};
use crate::terminal::input::Input;
use crate::terminal::output::{Canvas, Output};
use crate::tui::scene_renderer::SceneRenderer;

pub struct TuiFrontend<W: io::Write> {
    input: Input,
    output: Output<W>,
    renderer: SceneRenderer,
}

impl<W: io::Write> TuiFrontend<W> {
    pub fn new(output: Output<W>) -> Self {
        Self {
            input: Input::new(),
            output,
            renderer: SceneRenderer::new(),
        }
    }
}

impl<W: io::Write> Frontend for TuiFrontend<W> {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        self.input.next_event().await
    }
    fn render(
        &mut self,
        scene: &Scene,
        scene_revision: Revision,
        query: &dyn RenderQuery,
        focused: SpaceId,
    ) -> io::Result<()> {
        self.renderer.render(
            scene,
            scene_revision,
            query,
            focused,
            &mut self.output as &mut dyn Canvas,
        )
    }

    fn resolve_viewport_command(
        &mut self,
        scene: &Scene,
        scene_revision: Revision,
        view: ViewId,
        cursor_row: usize,
        command: ViewportCommand,
    ) -> io::Result<ResolvedViewportCommand> {
        Ok(self
            .renderer
            .resolve_viewport_command(scene, scene_revision, view, cursor_row, command))
    }

    fn apply_viewport_command(&mut self, view: ViewId, command: ResolvedViewportCommand) {
        self.renderer.apply_viewport_command(view, command);
    }
}
