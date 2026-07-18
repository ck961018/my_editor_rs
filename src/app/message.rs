use std::io;

use crate::app::mode::{ModeJobKey, ModeJobResult};
use crate::core::transaction::TextStateId;
use crate::protocol::ids::ContentId;

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
