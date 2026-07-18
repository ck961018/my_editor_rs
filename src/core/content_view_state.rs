use crate::protocol::selection::{Selection, Selections, TextOffset};

pub struct ContentViewState {
    selections: Option<Selections>,
}

impl ContentViewState {
    pub fn text() -> Self {
        Self {
            selections: Some(Selections::single(Selection::collapsed(
                TextOffset::origin(),
            ))),
        }
    }

    pub fn stateless() -> Self {
        Self { selections: None }
    }

    pub fn selections(&self) -> Option<&Selections> {
        self.selections.as_ref()
    }

    pub fn selections_mut(&mut self) -> Option<&mut Selections> {
        self.selections.as_mut()
    }

    pub fn replace_selections(&mut self, selections: Selections) -> Option<bool> {
        let current = self.selections.as_mut()?;
        let changed = current != &selections;
        *current = selections;
        Some(changed)
    }
}
