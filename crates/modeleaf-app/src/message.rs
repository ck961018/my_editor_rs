use std::io;

use crate::mode::{ModeJobKey, ModeJobResult};
use modeleaf_core::transaction::TextStateId;
use modeleaf_protocol::ids::ContentId;

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
