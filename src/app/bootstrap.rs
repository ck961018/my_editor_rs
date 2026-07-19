use std::collections::HashMap;
use std::io;

use crate::app::kernel::Kernel;
use crate::app::mode::{Mode, ModeRegistry};
#[cfg(test)]
use crate::app::mode_name::ModeName;
use crate::app::script::ScriptMode;
use crate::app::session::{ClientSession, EditorSessionInit, InitialView};
use crate::core::buffer::Buffer;
use crate::core::content::Content;
use crate::core::content_store::ContentStore;
use crate::core::status_bar::StatusBar;
use crate::protocol::ids::{ContentId, ViewId};

pub(super) struct EditorBootstrap {
    pub kernel: Kernel,
    pub session: ClientSession,
}

#[derive(Default)]
struct BootstrapIds {
    next_content: u64,
    next_view: u64,
}

impl BootstrapIds {
    fn content(&mut self) -> ContentId {
        let id = ContentId(self.next_content);
        self.next_content = self
            .next_content
            .checked_add(1)
            .expect("bootstrap content id overflow");
        id
    }

    fn view(&mut self) -> ViewId {
        let id = ViewId(self.next_view);
        self.next_view = self
            .next_view
            .checked_add(1)
            .expect("bootstrap view id overflow");
        id
    }
}

pub(super) fn bootstrap_editor(
    buffer: Buffer,
    width: usize,
    height: usize,
    script_modes: Vec<ScriptMode>,
) -> io::Result<EditorBootstrap> {
    let mut ids = BootstrapIds::default();
    let editor_content = ids.content();
    let status_content = ids.content();
    let editor_view = ids.view();
    let status_view = ids.view();
    let mut editor_modes = Vec::new();

    let mut contents = ContentStore::default();
    contents
        .insert(editor_content, Content::Buffer(buffer))
        .expect("bootstrap allocates unique content ids");
    contents
        .insert(
            status_content,
            Content::StatusBar(StatusBar::new(editor_content)),
        )
        .expect("bootstrap allocates unique content ids");
    let mut modes = ModeRegistry::new();
    for mode in script_modes {
        let name = mode.name().clone();
        if modes.resolve_mode(&name).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("mode '{}' is already registered", name.as_str()),
            ));
        }
        if let Some(before) = mode.before() {
            let index = editor_modes
                .iter()
                .position(|candidate| candidate == before)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("mode '{}' cannot attach before unknown mode", name.as_str()),
                    )
                })?;
            editor_modes.insert(index, name);
        } else {
            editor_modes.push(name);
        }
        modes.register(mode);
    }
    let mut kernel = Kernel::new(
        contents,
        modes,
        HashMap::from([(editor_content, editor_modes.clone())]),
    );
    let (contents, modes, mode_contents) = kernel.mode_attachment_parts();
    let session = ClientSession::editor(
        contents,
        modes,
        mode_contents,
        width,
        height,
        EditorSessionInit {
            editor: InitialView {
                view: editor_view,
                content: editor_content,
                modes: editor_modes,
            },
            status: InitialView {
                view: status_view,
                content: status_content,
                modes: Vec::new(),
            },
            next_view_id: ids.next_view,
        },
    );
    Ok(EditorBootstrap { kernel, session })
}

#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "test helper exposes the editor session's independent inputs"
)]
pub(super) fn create_editor_session(
    contents: &ContentStore,
    modes: &ModeRegistry,
    mode_contents: &mut crate::app::mode::ModeContentStore,
    width: usize,
    height: usize,
    editor_content: ContentId,
    status_content: ContentId,
    editor_modes: Vec<ModeName>,
) -> ClientSession {
    let mut ids = BootstrapIds::default();
    let editor_view = ids.view();
    let status_view = ids.view();
    ClientSession::editor(
        contents,
        modes,
        mode_contents,
        width,
        height,
        EditorSessionInit {
            editor: InitialView {
                view: editor_view,
                content: editor_content,
                modes: editor_modes,
            },
            status: InitialView {
                view: status_view,
                content: status_content,
                modes: Vec::new(),
            },
            next_view_id: ids.next_view,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_bootstrap_uses_explicit_content_roles() {
        let editor = ContentId(7);
        let status = ContentId(11);
        let mut contents = ContentStore::default();
        contents
            .insert(editor, Content::Buffer(Buffer::new()))
            .unwrap();
        contents
            .insert(status, Content::StatusBar(StatusBar::new(editor)))
            .unwrap();
        let modes = ModeRegistry::new();
        let mut mode_contents = crate::app::mode::ModeContentStore::default();

        let session = create_editor_session(
            &contents,
            &modes,
            &mut mode_contents,
            40,
            5,
            editor,
            status,
            Vec::new(),
        );

        assert_eq!(session.views()[&ViewId(0)].content(), editor);
        assert_eq!(session.views()[&ViewId(1)].content(), status);
        assert_eq!(session.next_view_id_for_test(), 2);
    }
}
