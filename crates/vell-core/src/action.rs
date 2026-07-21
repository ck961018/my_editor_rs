use crate::core::transaction::TextChangeSet;
use crate::protocol::selection::Selections;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentAction {
    Text(TextChangeSet),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentEditPlan {
    pub action: Option<ContentAction>,
    pub selections: Selections,
}
