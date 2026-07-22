use std::io;
use std::time::Instant;

#[cfg(test)]
use crate::behavior::BehaviorRecorder;
use crate::bootstrap::{bootstrap_editor, bootstrap_editor_with_theme};
use crate::diagnostics::RuntimeDiagnostic;
use crate::kernel::Kernel;
use crate::mode::{Mode, ModeAttachmentError};
use crate::mode_name::ModeName;
use crate::session::ClientSession;
use vell_core::buffer::Buffer;
use vell_frontend::Frontend;
use vell_protocol::ids::ContentId;
use vell_protocol::content_query::{FaceOverride, ThemeName};

pub struct App<F: Frontend> {
    pub(super) kernel: Kernel,
    pub(super) session: ClientSession,
    pub(super) frontend: F,
    pub(super) runtime_diagnostics: Vec<RuntimeDiagnostic>,
    #[cfg(test)]
    pub(super) behavior: BehaviorRecorder,
}

impl<F: Frontend> App<F> {
    #[allow(dead_code, reason = "unconfigured application constructor")]
    pub fn new(path: Option<&str>, width: usize, height: usize, frontend: F) -> io::Result<Self> {
        Self::build(path, width, height, frontend, Vec::new(), None, Vec::new())
    }

    pub fn with_modes(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        modes: Vec<Box<dyn Mode>>,
    ) -> io::Result<Self> {
        Self::build(path, width, height, frontend, modes, None, Vec::new())
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
            theme.as_ref(),
            face_overrides,
        )
    }

    fn build(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        modes: Vec<Box<dyn Mode>>,
        theme: Option<&ThemeName>,
        face_overrides: Vec<FaceOverride>,
    ) -> io::Result<Self> {
        let display_profile = frontend.display_profile();
        let mut buffer = Buffer::new();
        if let Some(p) = path {
            buffer.open_path(p)?;
        }
        let mut bootstrap = match theme {
            Some(theme) => {
                bootstrap_editor_with_theme(
                    buffer,
                    width,
                    height,
                    modes,
                    Some(theme),
                    face_overrides,
                )?
            }
            None if face_overrides.is_empty() => bootstrap_editor(buffer, width, height, modes)?,
            None => bootstrap_editor_with_theme(
                buffer,
                width,
                height,
                modes,
                None,
                face_overrides,
            )?,
        };
        bootstrap
            .session
            .faces_mut()
            .set_display_profile(display_profile);
        Ok(Self {
            kernel: bootstrap.kernel,
            session: bootstrap.session,
            frontend,
            runtime_diagnostics: Vec::new(),
            #[cfg(test)]
            behavior: BehaviorRecorder::default(),
        })
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Mode attachment is an app extension seam")
    )]
    pub(super) fn attach_mode_to_content(
        &mut self,
        content: ContentId,
        mode: &ModeName,
    ) -> Result<(), ModeAttachmentError> {
        let (contents, modes, mode_contents) = self.kernel.mode_attachment_parts();
        self.session
            .attach_mode_to_content_views(content, mode, modes, mode_contents, contents)?;
        self.session
            .sync_focused_input(Instant::now(), mode_contents, contents);
        self.kernel.schedule_mode_jobs();
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(())
    }
}
