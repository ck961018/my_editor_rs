use crate::app::kernel::Kernel;
use crate::app::session::{ClientSession, EditorSessionInit, InitialView};
use crate::core::buffer::Buffer;
use crate::core::content::Content;
use crate::core::content_store::ContentStore;
use crate::core::mode::ModeRegistry;
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

pub(super) fn bootstrap_editor(buffer: Buffer, width: usize, height: usize) -> EditorBootstrap {
    let mut ids = BootstrapIds::default();
    let editor_content = ids.content();
    let status_content = ids.content();
    let editor_view = ids.view();
    let status_view = ids.view();

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
    let modes = ModeRegistry::builtin();
    let session = ClientSession::editor(
        &contents,
        &modes,
        width,
        height,
        EditorSessionInit {
            editor: InitialView {
                view: editor_view,
                content: editor_content,
            },
            status: InitialView {
                view: status_view,
                content: status_content,
            },
            next_view_id: ids.next_view,
        },
    );
    let kernel = Kernel::new(contents, modes);
    EditorBootstrap { kernel, session }
}

#[cfg(test)]
pub(super) fn create_editor_session(
    contents: &ContentStore,
    modes: &ModeRegistry,
    width: usize,
    height: usize,
    editor_content: ContentId,
    status_content: ContentId,
) -> ClientSession {
    let mut ids = BootstrapIds::default();
    let editor_view = ids.view();
    let status_view = ids.view();
    ClientSession::editor(
        contents,
        modes,
        width,
        height,
        EditorSessionInit {
            editor: InitialView {
                view: editor_view,
                content: editor_content,
            },
            status: InitialView {
                view: status_view,
                content: status_content,
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
        let modes = ModeRegistry::builtin();

        let session = create_editor_session(&contents, &modes, 40, 5, editor, status);

        assert_eq!(session.views()[&ViewId(0)].content(), editor);
        assert_eq!(session.views()[&ViewId(1)].content(), status);
        assert_eq!(session.next_view_id_for_test(), 2);
    }
}
