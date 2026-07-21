use vell_protocol::selection::Selections;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewAction {
    SetSelections(Selections),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionIntent {
    Begin,
    Commit,
    Rollback,
    Undo,
    Redo,
}
