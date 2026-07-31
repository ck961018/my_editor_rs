use std::io;
use std::path::PathBuf;

use crate::kernel::FileBaseline;
use crate::mode::{ModeJobKey, ModeJobResult};
use vell_core::content::Content;
use vell_core::transaction::TextStateId;
use vell_protocol::ids::ContentId;

pub(crate) struct OpenedBuffer {
    pub content: Content,
    pub baseline: FileBaseline,
}

pub(crate) struct OpenedPath {
    pub path: PathBuf,
    pub identity: PathBuf,
    pub buffer: OpenedBuffer,
}

pub(crate) enum AppMessage {
    OpenCompleted {
        content: ContentId,
        result: io::Result<OpenedPath>,
    },
    SaveCompleted {
        content: ContentId,
        revision: u64,
        state: TextStateId,
        result: io::Result<()>,
    },
    ModeJobFinished {
        key: ModeJobKey,
        version: u64,
        result: ModeJobResult,
    },
}
