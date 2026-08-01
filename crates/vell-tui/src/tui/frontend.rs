//! TUI 的 Frontend 实现：SceneRenderer + `Output<W>`。

use std::io;

use crate::frontend::Frontend;
use crate::protocol::content_query::{ColorDepth, DisplayProfile, RenderQuery};
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{SpaceId, ViewId};
use crate::protocol::revision::Revision;
use crate::protocol::scene::Scene;
use crate::protocol::space::SplitDirection;
use crate::protocol::viewport::{ResolvedViewportCommand, ViewportCommand};
use crate::terminal::input::Input;
use crate::terminal::output::{Canvas, Output};
use crate::tui::scene_renderer::SceneRenderer;

pub struct TuiFrontend<W: io::Write> {
    input: Input,
    output: Output<W>,
    renderer: SceneRenderer,
    display_profile: DisplayProfile,
}

impl<W: io::Write> TuiFrontend<W> {
    pub fn new(output: Output<W>) -> Self {
        Self {
            input: Input::new(),
            output,
            renderer: SceneRenderer::new(),
            display_profile: detect_display_profile(),
        }
    }

    pub fn with_display_profile(output: Output<W>, display_profile: DisplayProfile) -> Self {
        Self {
            input: Input::new(),
            output,
            renderer: SceneRenderer::new(),
            display_profile,
        }
    }
}

impl<W: io::Write> Frontend for TuiFrontend<W> {
    fn display_profile(&self) -> DisplayProfile {
        self.display_profile
    }

    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        self.input.next_event().await
    }

    fn read_clipboard(&mut self) -> io::Result<Option<String>> {
        system_clipboard::read().map(Some)
    }

    fn write_clipboard(&mut self, text: &str) -> io::Result<()> {
        system_clipboard::write(text)
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

    fn resolve_focus_direction(
        &mut self,
        scene: &Scene,
        scene_revision: Revision,
        focused: SpaceId,
        direction: SplitDirection,
    ) -> io::Result<Option<SpaceId>> {
        Ok(self
            .renderer
            .resolve_focus_direction(scene, scene_revision, focused, direction))
    }
}

#[cfg(windows)]
mod system_clipboard {
    use std::io::{self, Write};
    use std::process::{Command, Stdio};

    pub(super) fn read() -> io::Result<String> {
        let output = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-STA",
                "-Command",
                "[Console]::Out.Write((Get-Clipboard -Raw))",
            ])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("PowerShell clipboard read failed"));
        }
        String::from_utf8(output.stdout)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub(super) fn write(text: &str) -> io::Result<()> {
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-STA",
                "-Command",
                "Set-Clipboard -Value ([Console]::In.ReadToEnd())",
            ])
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("piped clipboard stdin")
            .write_all(text.as_bytes())?;
        let status = child.wait()?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| io::Error::other("PowerShell clipboard write failed"))
    }
}

#[cfg(not(windows))]
mod system_clipboard {
    use std::io;

    pub(super) fn read() -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "system clipboard provider is unavailable on this platform",
        ))
    }

    pub(super) fn write(_text: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "system clipboard provider is unavailable on this platform",
        ))
    }
}

fn detect_display_profile() -> DisplayProfile {
    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let color_term = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let term_program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let dumb = term == "dumb";
    let monochrome = std::env::var_os("NO_COLOR").is_some() || dumb;
    let color_depth = if monochrome {
        ColorDepth::Monochrome
    } else if color_term.contains("truecolor") || color_term.contains("24bit") {
        ColorDepth::TrueColor
    } else if term.contains("256color") {
        ColorDepth::Ansi256
    } else {
        ColorDepth::Ansi16
    };
    let extended_underline = ["kitty", "wezterm", "iterm", "foot"]
        .iter()
        .any(|name| term.contains(name) || term_program.contains(name));
    DisplayProfile {
        color_depth,
        appearance: None,
        supports_italic: !dumb,
        supports_underline: !dumb,
        supports_extended_underline: extended_underline,
        supports_undercurl: extended_underline,
        supports_strikethrough: !dumb,
        supports_dim: !dumb,
    }
}
