use std::io;
use std::path::PathBuf;

use crate::kernel::FileBaseline;
use crate::mode::{CompletionSourceError, ModeJobKey, ModeJobResult};
#[cfg(test)]
use vell_completion::CompletionBatch;
use vell_completion::SourceRequestKey;
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
    CompletionBatchReady(SourceRequestKey),
    #[cfg(test)]
    CompletionBatchForTest(CompletionBatch),
    CompletionSourceFinished {
        key: SourceRequestKey,
        outcome: CompletionSourceTaskOutcome,
    },
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

#[derive(Debug)]
pub(crate) enum CompletionSourceTaskOutcome {
    Completed,
    Cancelled,
    TimedOut,
    Failed(CompletionSourceError),
}
