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
                content: views.get(&view)?.document_content()?,
            })
        }
        Command::Content(command) => {
            let view = source.view_or(focused_view);
            Some(DispatchCommand::Content {
                command,
                content: views.get(&view)?.document_content()?,
            })
        }
        Command::Mode(command) => {
            let view = source.view_or(focused_view);
            let content = views.get(&view)?.document_content();
            Some(match content {
                Some(content) => DispatchCommand::Mode {
                    command,
                    view,
                    content,
                },
                None => DispatchCommand::ViewMode { command, view },
            })
        }
        Command::ModeInput(input) => {
            let view = source.view_or(focused_view);
            let content = views.get(&view)?.document_content();
            Some(match content {
                Some(content) => DispatchCommand::ModeInput {
                    input,
                    view,
                    content,
                },
                None => DispatchCommand::ViewModeInput { input, view },
            })
        }
        Command::Registered(invocation) => {
            let view = source.view_or(focused_view);
            Some(DispatchCommand::Registered {
                invocation,
                view,
                content: views.get(&view)?.document_content()?,
            })
        }
        Command::Viewport(command) => {
            let view = source.view_or(focused_view);
            Some(DispatchCommand::Viewport {
                command,
                view,
                content: views.get(&view)?.document_content()?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{ModeCommand, ModeInputCommand};
    use crate::mode_name::{ModeActionName, ModeName};
    use vell_protocol::view::{ViewDefinition, ViewDefinitionId};

    #[test]
    fn view_only_mode_commands_do_not_require_a_document_binding() {
        let view_id = ViewId(7);
        let definition = ViewDefinition::new(ViewDefinitionId::new("test.container"), []).unwrap();
        let view = View::with_definition(&definition, [], None).unwrap();
        let views = HashMap::from([(view_id, view)]);
        let source = CommandSource::Mode {
            view: view_id,
            index: 0,
        };
        let name = ModeName::new("container");

        assert!(matches!(
            resolve_command(
                Command::Mode(ModeCommand::new(
                    name.clone(),
                    ModeActionName::new("next")
                )),
                source,
                view_id,
                &views,
            ),
            Some(DispatchCommand::ViewMode { view, .. }) if view == view_id
        ));
        assert!(matches!(
            resolve_command(
                Command::ModeInput(ModeInputCommand::new(name, KeyEvent::char('x'))),
                source,
                view_id,
                &views,
            ),
            Some(DispatchCommand::ViewModeInput { view, .. }) if view == view_id
        ));
    }
}
