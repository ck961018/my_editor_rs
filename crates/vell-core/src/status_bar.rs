use crate::protocol::ids::ContentId;

/// Status-bar content stores only its target. ContentStore derives its display data by
/// querying the target document status.
#[derive(Clone)]
pub struct StatusBar {
    target_content_id: ContentId,
}

impl StatusBar {
    pub fn new(target_content_id: ContentId) -> Self {
        Self { target_content_id }
    }

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
