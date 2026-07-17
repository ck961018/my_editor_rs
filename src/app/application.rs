use std::io;

use crate::app::bootstrap::bootstrap_editor;
use crate::app::kernel::Kernel;
use crate::app::session::ClientSession;
use crate::core::buffer::Buffer;
use crate::frontend::Frontend;

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
}
