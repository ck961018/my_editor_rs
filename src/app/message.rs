use std::io;

use crate::protocol::ids::ContentId;

#[derive(Debug)]
pub(crate) enum AppMessage {
    SaveCompleted {
        content: ContentId,
        result: io::Result<()>,
    },
}
