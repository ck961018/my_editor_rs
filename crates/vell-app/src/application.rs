use std::collections::HashMap;
use std::io;
use std::time::Instant;

#[cfg(test)]
use crate::behavior::BehaviorRecorder;
use crate::bootstrap::{bootstrap_editor, bootstrap_editor_with_theme};
use crate::buffer_lifecycle::normalize_path;
use crate::diagnostics::RuntimeDiagnostic;
use crate::kernel::{FileBaseline, Kernel};
use crate::mode::{Mode, ModeBackground};
use crate::mode_name::ModeName;
use crate::mode_resolver::AttachmentPlanError;
use crate::session::ClientSession;
use vell_core::buffer::Buffer;
use vell_core::transaction::TextStateId;
use vell_frontend::Frontend;
use vell_mode::command_registry::{CommandEntry, CommandPending, CommandTaskId};
use vell_protocol::content_query::{FaceOverride, ThemeName};
use vell_protocol::ids::{ContentId, ViewId};

pub(super) struct CommandTaskTarget {
    pub content: ContentId,
    pub revision: u64,
}

pub(super) struct PendingCommandInvocation {
    pub pending: CommandPending,
    pub view: ViewId,
    pub content: ContentId,
    pub expected_state: TextStateId,
}

pub struct App<F: Frontend> {
    pub(super) kernel: Kernel,
    pub(super) session: ClientSession,
    pub(super) frontend: F,
    pub(super) runtime_diagnostics: Vec<RuntimeDiagnostic>,
    pub(super) next_command_task: u64,
    pub(super) command_tasks: HashMap<CommandTaskId, CommandTaskTarget>,
    pub(super) pending_commands: Vec<PendingCommandInvocation>,
    #[cfg(test)]
    pub(super) behavior: BehaviorRecorder,
}

impl<F: Frontend> App<F> {
    pub fn register_command(&mut self, command: CommandEntry) -> Option<CommandEntry> {
        self.kernel.commands_mut().register(command)
    }

    #[allow(dead_code, reason = "unconfigured application constructor")]
    pub fn new(path: Option<&str>, width: usize, height: usize, frontend: F) -> io::Result<Self> {
        Self::build(
            path,
            width,
            height,
            frontend,
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
        )
    }

    pub fn with_modes(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        modes: Vec<Box<dyn Mode>>,
    ) -> io::Result<Self> {
        Self::build(
            path,
            width,
            height,
            frontend,
            modes,
            Vec::new(),
            None,
            Vec::new(),
        )
    }

    pub fn with_modes_and_theme(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        modes: Vec<Box<dyn Mode>>,
        theme: impl Into<String>,
    ) -> io::Result<Self> {
        let theme = ThemeName::new(theme);
        Self::build(
            path,
            width,
            height,
            frontend,
            modes,
            Vec::new(),
            Some(&theme),
            Vec::new(),
        )
    }

    pub fn with_modes_and_visuals(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        modes: Vec<Box<dyn Mode>>,
        theme: Option<ThemeName>,
        face_overrides: Vec<FaceOverride>,
    ) -> io::Result<Self> {
        Self::build(
            path,
            width,
            height,
            frontend,
            modes,
            Vec::new(),
            theme.as_ref(),
            face_overrides,
        )
    }

    // ponytail: composition-root inputs stay explicit; add a builder only if this grows.
    #[allow(clippy::too_many_arguments)]
    pub fn with_modes_visuals_and_backgrounds(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        modes: Vec<Box<dyn Mode>>,
        backgrounds: Vec<Box<dyn ModeBackground>>,
        theme: Option<ThemeName>,
        face_overrides: Vec<FaceOverride>,
    ) -> io::Result<Self> {
        Self::build(
            path,
            width,
            height,
            frontend,
            modes,
            backgrounds,
            theme.as_ref(),
            face_overrides,
        )
    }

    // ponytail: this private funnel mirrors the public composition-root inputs.
    #[allow(clippy::too_many_arguments)]
    fn build(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        modes: Vec<Box<dyn Mode>>,
        backgrounds: Vec<Box<dyn ModeBackground>>,
        theme: Option<&ThemeName>,
        face_overrides: Vec<FaceOverride>,
    ) -> io::Result<Self> {
        let display_profile = frontend.display_profile();
        let opened_path = path
            .map(|path| normalize_path(std::path::Path::new(path)))
            .transpose()?;
        let (buffer, baseline) = if let Some((path, _)) = &opened_path {
            match std::fs::read_to_string(path) {
                Ok(text) => (
                    Buffer::from_file(path.clone(), text.clone()),
                    Some(FileBaseline::Materialized(text)),
                ),
                Err(source) if source.kind() == io::ErrorKind::NotFound => (
                    Buffer::for_new_file(path.clone()),
                    Some(FileBaseline::Missing),
                ),
                Err(source) => return Err(source),
            }
        } else {
            (Buffer::new(), None)
        };
        let mut bootstrap = match theme {
            Some(theme) => bootstrap_editor_with_theme(
                buffer,
                width,
                height,
                modes,
                Some(theme),
                face_overrides,
            )?,
            None if face_overrides.is_empty() => bootstrap_editor(buffer, width, height, modes)?,
            None => {
                bootstrap_editor_with_theme(buffer, width, height, modes, None, face_overrides)?
            }
        };
        for background in backgrounds {
            bootstrap.kernel.register_mode_background(background);
        }
        bootstrap
            .session
            .faces_mut()
            .set_display_profile(display_profile);
        let mut app = Self {
            kernel: bootstrap.kernel,
            session: bootstrap.session,
            frontend,
            runtime_diagnostics: Vec::new(),
            next_command_task: 0,
            command_tasks: HashMap::new(),
            pending_commands: Vec::new(),
            #[cfg(test)]
            behavior: BehaviorRecorder::default(),
        };
        if let Some(((path, identity), baseline)) = opened_path.zip(baseline) {
            app.kernel
                .register_buffer_path(ContentId(0), identity, path, baseline)
                .expect("bootstrap contains only one editor buffer");
        }
        Ok(app)
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Mode attachment is an app extension seam")
    )]
    pub(super) fn attach_mode_to_content(
        &mut self,
        content: ContentId,
        mode: &ModeName,
    ) -> Result<(), AttachmentPlanError> {
        let (contents, modes, classifier, mode_contents) = self.kernel.attachment_runtime_parts();
        self.session.attach_mode_to_content_views(
            content,
            mode,
            modes,
            classifier,
            mode_contents,
            contents,
        )?;
        self.session
            .sync_focused_input(Instant::now(), mode_contents, contents);
        self.kernel.schedule_mode_jobs();
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(())
    }
}
