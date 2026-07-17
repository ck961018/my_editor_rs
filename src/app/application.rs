use std::io;

use crate::app::kernel::Kernel;
use crate::app::session::ClientSession;
use crate::core::buffer::Buffer;
use crate::core::content::Content;
use crate::core::content_store::ContentStore;
use crate::core::mode::ModeRegistry;
use crate::core::status_bar::StatusBar;
use crate::frontend::Frontend;
use crate::protocol::ids::ContentId;

pub struct App<F: Frontend> {
    pub(super) kernel: Kernel,
    pub(super) session: ClientSession,
    pub(super) frontend: F,
}

impl<F: Frontend> App<F> {
    pub fn new(path: Option<&str>, width: usize, height: usize, frontend: F) -> io::Result<Self> {
        let editor_content = ContentId(0);
        let status_content = ContentId(1);
        let mut buffer = Buffer::new();
        if let Some(p) = path {
            buffer.open_path(p)?;
        }
        let status_bar = StatusBar::new(editor_content);
        let mut contents = ContentStore::default();
        contents
            .insert(editor_content, Content::Buffer(buffer))
            .expect("editor content id is unique");
        contents
            .insert(status_content, Content::StatusBar(status_bar))
            .expect("status content id is unique");
        let modes = ModeRegistry::builtin();
        let session = ClientSession::editor(&contents, &modes, width, height);
        let kernel = Kernel::new(contents, modes);
        Ok(Self {
            kernel,
            session,
            frontend,
        })
    }
}
