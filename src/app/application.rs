use std::io;
use std::time::Instant;

use crate::app::bootstrap::bootstrap_editor;
use crate::app::kernel::Kernel;
use crate::app::mode_name::ModeName;
use crate::app::session::ClientSession;
use crate::core::buffer::Buffer;
use crate::frontend::Frontend;
use crate::protocol::ids::ContentId;

pub struct App<F: Frontend> {
    pub(super) kernel: Kernel,
    pub(super) session: ClientSession,
    pub(super) frontend: F,
}

impl<F: Frontend> App<F> {
    pub fn new(path: Option<&str>, width: usize, height: usize, frontend: F) -> io::Result<Self> {
        let mut buffer = Buffer::new();
        if let Some(p) = path {
            buffer.open_path(p)?;
        }
        let bootstrap = bootstrap_editor(buffer, width, height);
        Ok(Self {
            kernel: bootstrap.kernel,
            session: bootstrap.session,
            frontend,
        })
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Mode attachment is an app extension seam")
    )]
    pub(super) fn attach_mode_to_content(&mut self, content: ContentId, mode: &ModeName) -> bool {
        let (contents, modes, mode_contents) = self.kernel.mode_attachment_parts();
        if !self
            .session
            .attach_mode_to_content_views(content, mode, modes, mode_contents, contents)
        {
            return false;
        }
        self.session
            .sync_focused_input(Instant::now(), mode_contents, contents);
        self.kernel.schedule_mode_jobs();
        true
    }
}
