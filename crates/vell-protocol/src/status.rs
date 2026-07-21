#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusMessage {
    None,
    Saved,
    SaveFailed,
    NewFile,
    OpenFailed,
}
