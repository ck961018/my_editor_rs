//! ContentLookup for contents map。替代旧 document.rs。

use std::collections::HashMap;

use crate::core::content::{ContentHandler, ContentLookup};
use crate::protocol::ids::ContentId;

impl ContentLookup for HashMap<ContentId, Box<dyn ContentHandler>> {
    fn get(&self, id: ContentId) -> Option<&dyn ContentHandler> {
        HashMap::get(self, &id).map(|c| c.as_ref())
    }
}
