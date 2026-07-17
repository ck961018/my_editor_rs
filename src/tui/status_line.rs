use crate::protocol::status::StatusMessage;

pub(super) fn status_line(
    file_name: Option<&str>,
    modified: bool,
    message: &StatusMessage,
) -> String {
    let name = file_name.unwrap_or("[No Name]");
    let modified = if modified { "[+]" } else { "" };
    let msg = match message {
        StatusMessage::None => "",
        StatusMessage::Saved => "Saved",
        StatusMessage::SaveFailed => "SaveFailed",
        StatusMessage::NewFile => "NewFile",
        StatusMessage::OpenFailed => "OpenFailed",
    };
    format!("{name} {modified}  {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_formats_document_state() {
        assert_eq!(
            status_line(None, false, &StatusMessage::None),
            "[No Name]   "
        );
        assert_eq!(
            status_line(Some("file.rs"), true, &StatusMessage::Saved),
            "file.rs [+]  Saved"
        );
    }
}
