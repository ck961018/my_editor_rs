use crate::protocol::selection::{Selection, Selections, TextOffset};

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
}

pub enum ContentViewState {
    Buffer(BufferViewState),
    StatusBar,
}

impl ContentViewState {
    pub fn buffer() -> Self {
        Self::Buffer(BufferViewState::new())
    }

    pub fn selections(&self) -> Option<&Selections> {
        match self {
            Self::Buffer(state) => Some(state.selections()),
            Self::StatusBar => None,
        }
    }
}
