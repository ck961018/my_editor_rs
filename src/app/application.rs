use std::io;
use std::time::Instant;

use crate::app::bootstrap::bootstrap_editor;
use crate::app::kernel::Kernel;
use crate::app::session::ClientSession;
use crate::core::buffer::Buffer;
use crate::core::mode_name::ModeName;
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
        allow(dead_code, reason = "ContentMode binding is an app extension seam")
    )]
    pub(super) fn bind_content_mode(&mut self, content: ContentId, mode: &ModeName) -> bool {
        if !self.kernel.bind_content_mode(content, mode) {
            return false;
        }
        self.session
            .remove_view_modes_for_content(content, self.kernel.contents());
        self.session.sync_focused_input(
            Instant::now(),
            self.kernel.content_modes(),
            self.kernel.contents(),
        );
        true
    }
}
