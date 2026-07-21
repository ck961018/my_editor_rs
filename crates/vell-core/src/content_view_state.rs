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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusBarViewState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentViewState {
    Buffer(BufferViewState),
    StatusBar(StatusBarViewState),
}

impl ContentViewState {
    pub fn buffer() -> Self {
        Self::Buffer(BufferViewState::new())
    }

    pub fn status_bar() -> Self {
        Self::StatusBar(StatusBarViewState)
    }

    pub fn kind(&self) -> ContentKind {
        match self {
            Self::Buffer(_) => ContentKind::Buffer,
            Self::StatusBar(_) => ContentKind::StatusBar,
        }
    }

    pub fn selections(&self) -> Option<&Selections> {
        match self {
            Self::Buffer(state) => Some(state.selections()),
            Self::StatusBar(_) => None,
        }
    }

    #[cfg(test)]
    pub fn selections_mut(&mut self) -> Option<&mut Selections> {
        match self {
            Self::Buffer(state) => Some(state.selections_mut()),
            Self::StatusBar(_) => None,
        }
    }

    pub fn replace_selections(&mut self, selections: Selections) -> Option<bool> {
        match self {
            Self::Buffer(state) => Some(state.replace_selections(selections)),
            Self::StatusBar(_) => None,
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

    #[test]
    fn status_bar_state_cannot_accept_selections() {
        let mut state = ContentViewState::status_bar();

        assert_eq!(state.kind(), ContentKind::StatusBar);
        assert!(state.selections().is_none());
        assert!(state.selections_mut().is_none());
        assert_eq!(
            state.replace_selections(Selections::single(Selection::collapsed(
                TextOffset::origin(),
            ))),
            None
        );
    }
}
