use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::time::Instant;

use crate::app::bootstrap::bootstrap_editor;
use crate::app::kernel::Kernel;
use crate::app::mode_name::ModeName;
use crate::app::script::{ScriptHost, load_user_config};
use crate::app::session::ClientSession;
use crate::core::buffer::Buffer;
use crate::frontend::Frontend;
use crate::protocol::ids::ContentId;

pub struct App<F: Frontend> {
    pub(super) kernel: Kernel,
    pub(super) session: ClientSession,
    pub(super) frontend: F,
    #[allow(dead_code, reason = "keeps script mode callbacks alive")]
    pub(super) script_host: Option<Rc<RefCell<ScriptHost>>>,
}

impl<F: Frontend> App<F> {
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "test-only unconfigured constructor")
    )]
    pub fn new(path: Option<&str>, width: usize, height: usize, frontend: F) -> io::Result<Self> {
        Self::build(path, width, height, frontend, None)
    }

    pub fn new_configured(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
    ) -> io::Result<Self> {
        let script_host = load_user_config().map_err(io::Error::other)?;
        Self::build(path, width, height, frontend, script_host)
    }

    fn build(
        path: Option<&str>,
        width: usize,
        height: usize,
        frontend: F,
        script_host: Option<Rc<RefCell<ScriptHost>>>,
    ) -> io::Result<Self> {
        let mut buffer = Buffer::new();
        if let Some(p) = path {
            buffer.open_path(p)?;
        }
        let script_modes = script_host
            .as_ref()
            .map(ScriptHost::script_modes)
            .unwrap_or_default();
        let bootstrap = bootstrap_editor(buffer, width, height, script_modes)?;
        Ok(Self {
            kernel: bootstrap.kernel,
            session: bootstrap.session,
            frontend,
            script_host,
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
