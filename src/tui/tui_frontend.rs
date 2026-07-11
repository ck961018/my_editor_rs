//! TUI 前端：SceneRenderer + Output<W>。Frontend::render 委托 SceneRenderer。

use std::io;

use crate::frontend::Frontend;
use crate::protocol::content_query::RenderQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::SpaceId;
use crate::protocol::scene::Scene;
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
        query: &dyn RenderQuery,
        focused: SpaceId,
    ) -> io::Result<()> {
        self.renderer
            .render(scene, query, focused, &mut self.output as &mut dyn Canvas)
    }
}
