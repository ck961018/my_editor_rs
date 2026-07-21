use std::collections::HashMap;

use crate::command::{AppCommand, Command, ContentCommandContext};
use crate::dispatcher::{CommandSource, DispatchCommand};
use crate::view::View;
use vell_core::keymap::Keymap;
use vell_protocol::ids::{SpaceId, ViewId};
use vell_protocol::key_event::KeyEvent;
use vell_protocol::scene::Scene;
use vell_protocol::space::SpaceKind;

pub(super) fn resolve_command(
    command: Command,
    source: CommandSource,
    focused_view: ViewId,
    views: &HashMap<ViewId, View>,
) -> Option<DispatchCommand> {
    match command {
        Command::App(command) => Some(DispatchCommand::App(command)),
        Command::Noop => Some(DispatchCommand::Noop),
        Command::Content(command) if command.context() == ContentCommandContext::WithViewState => {
            let view = source.view_or(focused_view);
            Some(DispatchCommand::ContentWithView {
                command,
                view,
                content: views.get(&view)?.content(),
            })
        }
        Command::Content(command) => {
            let view = source.view_or(focused_view);
            Some(DispatchCommand::Content {
                command,
                content: views.get(&view)?.content(),
            })
        }
        Command::Mode(command) => {
            let view = source.view_or(focused_view);
            Some(DispatchCommand::Mode {
                command,
                view,
                content: views.get(&view)?.content(),
            })
        }
        Command::ModeInput(input) => {
            let view = source.view_or(focused_view);
            Some(DispatchCommand::ModeInput {
                input,
                view,
                content: views.get(&view)?.content(),
            })
        }
        Command::Viewport(command) => {
            let view = source.view_or(focused_view);
            Some(DispatchCommand::Viewport {
                command,
                view,
                content: views.get(&view)?.content(),
            })
        }
    }
}

pub(super) fn focused_view_id(scene: &Scene, focused: SpaceId) -> Option<ViewId> {
    match &scene.node(focused).space.kind {
        SpaceKind::Content { view, .. } => Some(*view),
        SpaceKind::Container { .. } => None,
    }
}

pub(super) fn default_global_keymap() -> Keymap<Command> {
    let mut keymap = Keymap::new();
    keymap.bind(KeyEvent::ctrl('q'), Command::App(AppCommand::Quit));
    keymap.bind(
        KeyEvent::ctrl('s'),
        Command::Content(crate::command::ContentCommand::Save),
    );
    keymap
}
