use std::io;

use crate::core::transaction::TextStateId;
use crate::protocol::ids::ContentId;

#[derive(Debug)]
pub(crate) enum AppMessage {
    SaveCompleted {
        content: ContentId,
        revision: u64,
        state: TextStateId,
        result: io::Result<()>,
    },
}
