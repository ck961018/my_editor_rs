use std::io;

use crate::kernel::Kernel;
use crate::mode::{Mode, ModeRegistry};
#[cfg(test)]
use crate::mode_name::ModeName;
use crate::session::{ClientSession, EditorSessionInit, InitialView};
use crate::theme::FaceEnvironment;
use vell_core::buffer::Buffer;
use vell_core::content::Content;
use vell_core::content_store::ContentStore;
use vell_protocol::content_query::{FaceOverride, ThemeName};
use vell_protocol::editor_options::EditorOptions;
use vell_protocol::ids::{ContentId, ViewId};

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

#[cfg(test)]
pub(super) fn bootstrap_editor(
    buffer: Buffer,
    width: usize,
    height: usize,
    configured_modes: Vec<Box<dyn Mode>>,
) -> io::Result<EditorBootstrap> {
    bootstrap_editor_with_options_and_theme(
        buffer,
        width,
        height,
        configured_modes,
        None,
        Vec::new(),
        EditorOptions::default(),
    )
}

pub(super) fn bootstrap_editor_with_options_and_theme(
    buffer: Buffer,
    width: usize,
    height: usize,
    configured_modes: Vec<Box<dyn Mode>>,
    theme: Option<&ThemeName>,
    face_overrides: Vec<FaceOverride>,
    options: EditorOptions,
) -> io::Result<EditorBootstrap> {
    let mut ids = BootstrapIds::default();
    let editor_content = ids.content();
    let editor_view = ids.view();
    let mut contents = ContentStore::default();
    contents
        .insert(editor_content, Content::Buffer(buffer))
        .expect("bootstrap allocates unique content ids");
    let mut modes = ModeRegistry::new();
    for mode in configured_modes {
        modes
            .register_boxed(mode)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    }
    let mut kernel = Kernel::new(contents, modes);
    let buffer_definition = kernel.buffer_view_definition().clone();
    let (contents, modes, classifier, mode_contents) = kernel.attachment_runtime_parts();
    let face_environment =
        FaceEnvironment::with_overrides(theme, face_overrides).map_err(io::Error::other)?;
    let session = ClientSession::editor(
        contents,
        modes,
        classifier,
        mode_contents,
        width,
        height,
        EditorSessionInit {
            editor: InitialView {
                view: editor_view,
                content: editor_content,
            },
            next_view_id: ids.next_view,
            buffer_definition,
        },
        face_environment,
        options,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
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
    mode_contents: &mut crate::mode::ModeContentStore,
    width: usize,
    height: usize,
    editor_content: ContentId,
) -> ClientSession {
    let mut ids = BootstrapIds::default();
    let editor_view = ids.view();
    let buffer_definition = vell_protocol::view::ViewDefinition::buffer();
    let classifier = crate::content_classifier::ContentClassifier::default();
    ClientSession::editor(
        contents,
        modes,
        &classifier,
        mode_contents,
        width,
        height,
        EditorSessionInit {
            editor: InitialView {
                view: editor_view,
                content: editor_content,
            },
            next_view_id: ids.next_view,
            buffer_definition,
        },
        FaceEnvironment::new(None).expect("built-in themes must be valid"),
        EditorOptions::default(),
    )
    .expect("test session attachment plan is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OrderedTestMode {
        name: ModeName,
        before: Option<ModeName>,
        adapters: crate::mode::ModeAdapters,
    }

    impl Mode for OrderedTestMode {
        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[crate::mode_name::ModeActionName] {
            &[]
        }

        fn adapters(&self) -> crate::mode::ModeAdapters {
            self.adapters
        }

        fn before(&self) -> Option<&ModeName> {
            self.before.as_ref()
        }
    }

    fn ordered_mode(
        name: &str,
        before: Option<&str>,
        adapters: crate::mode::ModeAdapters,
    ) -> Box<dyn Mode> {
        Box::new(OrderedTestMode {
            name: ModeName::new(name),
            before: before.map(ModeName::new),
            adapters,
        })
    }

    #[test]
    fn session_bootstrap_uses_explicit_content_roles() {
        let editor = ContentId(7);
        let mut contents = ContentStore::default();
        contents
            .insert(editor, Content::Buffer(Buffer::new()))
            .unwrap();
        let modes = ModeRegistry::new();
        let mut mode_contents = crate::mode::ModeContentStore::default();

        let session = create_editor_session(&contents, &modes, &mut mode_contents, 40, 5, editor);

        assert_eq!(session.views()[&ViewId(0)].require_document(), editor);
        // 状态栏是 editor view 的直属 Pane，不再消耗 view id。
        assert_eq!(session.next_view_id_for_test(), 1);
    }

    #[test]
    fn bootstrap_stably_orders_forward_references_per_content_kind() {
        let bootstrap = bootstrap_editor(
            Buffer::new(),
            40,
            5,
            vec![
                ordered_mode("base", None, crate::mode::ModeAdapters::buffer()),
                ordered_mode("overlay", Some("base"), crate::mode::ModeAdapters::buffer()),
                ordered_mode("tail", None, crate::mode::ModeAdapters::buffer()),
            ],
        )
        .unwrap();

        assert_eq!(
            bootstrap.session.view_modes().mode_names(ViewId(0)),
            ["overlay", "base", "tail"].map(ModeName::new)
        );
    }

    #[test]
    fn bootstrap_rejects_invalid_mode_ordering() {
        let duplicate = bootstrap_editor(
            Buffer::new(),
            40,
            5,
            vec![
                ordered_mode("same", None, crate::mode::ModeAdapters::buffer()),
                ordered_mode("same", None, crate::mode::ModeAdapters::buffer()),
            ],
        )
        .err()
        .unwrap();
        assert_eq!(duplicate.kind(), io::ErrorKind::InvalidInput);
        assert!(
            duplicate
                .to_string()
                .contains("'same' is already registered")
        );

        let unknown = bootstrap_editor(
            Buffer::new(),
            40,
            5,
            vec![ordered_mode(
                "orphan",
                Some("missing"),
                crate::mode::ModeAdapters::buffer(),
            )],
        )
        .err()
        .unwrap();
        assert_eq!(unknown.kind(), io::ErrorKind::InvalidInput);
        assert!(
            unknown
                .to_string()
                .contains("orders before unknown mode 'missing'")
        );

        let cycle = bootstrap_editor(
            Buffer::new(),
            40,
            5,
            vec![
                ordered_mode("first", Some("second"), crate::mode::ModeAdapters::buffer()),
                ordered_mode("second", Some("first"), crate::mode::ModeAdapters::buffer()),
            ],
        )
        .err()
        .unwrap();
        assert_eq!(cycle.kind(), io::ErrorKind::InvalidInput);
        assert!(
            cycle
                .to_string()
                .contains("mode attachment ordering contains a cycle")
        );
        assert!(cycle.to_string().contains("first, second"));
    }
}
