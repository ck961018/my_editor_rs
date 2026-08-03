use std::fmt;

use crate::core::content::ContentKind;
use crate::protocol::ids::ContentId;
use crate::protocol::selection::{Selection, Selections, TextOffset};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferViewState {
    selections: Selections,
}

impl BufferViewState {
    fn new() -> Self {
        Self {
            selections: Selections::single(Selection::collapsed(TextOffset::origin())),
        }
    }

    pub fn selections(&self) -> &Selections {
        &self.selections
    }

    pub fn selections_mut(&mut self) -> &mut Selections {
        &mut self.selections
    }

    fn replace_selections(&mut self, selections: Selections) -> bool {
        let changed = self.selections != selections;
        self.selections = selections;
        changed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentViewState {
    Buffer(BufferViewState),
}

impl ContentViewState {
    pub fn buffer() -> Self {
        Self::Buffer(BufferViewState::new())
    }

    pub fn kind(&self) -> ContentKind {
        match self {
            Self::Buffer(_) => ContentKind::Buffer,
        }
    }

    pub fn selections(&self) -> Option<&Selections> {
        match self {
            Self::Buffer(state) => Some(state.selections()),
        }
    }

    #[cfg(test)]
    pub fn selections_mut(&mut self) -> Option<&mut Selections> {
        match self {
            Self::Buffer(state) => Some(state.selections_mut()),
        }
    }

    pub fn replace_selections(&mut self, selections: Selections) -> Option<bool> {
        match self {
            Self::Buffer(state) => Some(state.replace_selections(selections)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentViewStateError {
    MissingContent(ContentId),
    KindMismatch {
        content: ContentKind,
        state: ContentKind,
    },
}

impl fmt::Display for ContentViewStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContent(content) => {
                write!(formatter, "content {} does not exist", content.0)
            }
            Self::KindMismatch { content, state } => write!(
                formatter,
                "content kind {content:?} cannot transform {state:?} view state"
            ),
        }
    }
}

impl std::error::Error for ContentViewStateError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_state_always_has_selections() {
        let state = ContentViewState::buffer();

        assert_eq!(state.kind(), ContentKind::Buffer);
        assert_eq!(
            state.selections().unwrap().primary().head(),
            TextOffset::origin()
        );
    }
}
