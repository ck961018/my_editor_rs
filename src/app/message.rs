use std::io;

use crate::protocol::ids::ContentId;

#[derive(Debug)]
pub(crate) enum AppMessage {
    SaveCompleted {
        content: ContentId,
        revision: u64,
        result: io::Result<()>,
    },
}
