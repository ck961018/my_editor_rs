#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardKind {
    CharacterWise,
    LineWise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PastePlacement {
    Before,
    After,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardPayload {
    pub kind: ClipboardKind,
    pub fragments: Vec<String>,
}

impl ClipboardPayload {
    pub fn character(text: impl Into<String>) -> Self {
        Self {
            kind: ClipboardKind::CharacterWise,
            fragments: vec![text.into()],
        }
    }

    pub fn system_text(&self) -> String {
        self.fragments.concat()
    }
}
