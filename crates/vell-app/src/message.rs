use std::io;

use crate::mode::{ModeJobKey, ModeJobResult};
use vell_core::transaction::TextStateId;
use vell_protocol::ids::ContentId;

pub(crate) enum AppMessage {
    SaveCompleted {
        content: ContentId,
        revision: u64,
        state: TextStateId,
        result: io::Result<()>,
    },
    ModeJobCompleted {
        key: ModeJobKey,
        version: u64,
        result: ModeJobResult,
    },
}
