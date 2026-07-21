use std::io;
use std::time::Instant;

#[cfg(test)]
use crate::behavior::BehaviorRecorder;
use crate::bootstrap::bootstrap_editor;
use crate::kernel::Kernel;
use crate::mode::{Mode, ModeAttachmentError};
use crate::mode_name::ModeName;
use crate::session::ClientSession;
use vell_core::buffer::Buffer;
use vell_frontend::Frontend;
use vell_protocol::ids::ContentId;

pub struct App<F: Frontend> {
    pub(super) kernel: Kernel,
    pub(super) session: ClientSession,
    pub(super) frontend: F,
    #[cfg(test)]
    pub(super) behavior: BehaviorRecorder,
}

impl<F: Frontend> App<F> {
    #[allow(dead_code, reason = "unconfigured application constructor")]
    pub fn new(path: Option<&str>, width: usize, height: usize, frontend: F) -> io::Result<Self> {
        Self::build(path, width, height, frontend, Vec::new())
    }

    pub fn with_modes(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        modes: Vec<Box<dyn Mode>>,
    ) -> io::Result<Self> {
        Self::build(path, width, height, frontend, modes)
    }

    fn build(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        modes: Vec<Box<dyn Mode>>,
    ) -> io::Result<Self> {
        let mut buffer = Buffer::new();
        if let Some(p) = path {
            buffer.open_path(p)?;
        }
        let bootstrap = bootstrap_editor(buffer, width, height, modes)?;
        Ok(Self {
            kernel: bootstrap.kernel,
            session: bootstrap.session,
            frontend,
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
