use crate::core::keymap::Keymap;
use crate::protocol::ids::ContentId;

/// Status-bar content stores only its target and local keymap. ContentStore derives its
/// display data by querying the target document status.
pub struct StatusBar {
    target_content_id: ContentId,
    keymap: Keymap,
}

impl StatusBar {
    pub fn new(target_content_id: ContentId) -> Self {
        Self {
            target_content_id,
            keymap: Keymap::new(),
        }
    }

    pub(crate) fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub(crate) fn keymap_mut(&mut self) -> &mut Keymap {
        &mut self.keymap
    }

    #[allow(dead_code)] // Test helper.
    pub fn target_content_id(&self) -> ContentId {
        self.target_content_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_content_id_stored() {
        let sb = StatusBar::new(ContentId(7));
        assert_eq!(sb.target_content_id(), ContentId(7));
    }
}
