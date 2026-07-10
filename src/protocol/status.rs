#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusMessage {
    None,
    Saved,
    SaveFailed,
    NewFile,
    OpenFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_message_eq() {
        assert_eq!(StatusMessage::Saved, StatusMessage::Saved);
        assert_ne!(StatusMessage::Saved, StatusMessage::None);
    }
}
