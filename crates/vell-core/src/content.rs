use std::path::PathBuf;

use crate::core::action::{ContentAction, ContentEditPlan};
use crate::core::buffer::{Buffer, BufferTransactionData};
use crate::core::command::EditCommand;
use crate::core::content_view_state::ContentViewState;
use crate::core::status_bar::StatusBar;
use crate::core::text_snapshot::TextSnapshot;
use crate::core::transaction::{TextChangeSet, TextStateId, TextTransactionError};
use crate::protocol::content_query::{
    ContentData, ContentQuery, DirtyState, RowRange, SaveState, TextMetrics,
};
use crate::protocol::selection::Selections;

pub enum ContentInput {
    Save,
    Event(ContentEvent),
}

pub enum ContentEvent {
    SaveFinished {
        state: TextStateId,
        result: std::io::Result<()>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct SaveSnapshot {
    pub path: PathBuf,
    pub bytes: String,
    pub revision: u64,
    pub state: TextStateId,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContentEffect {
    None,
    Save(SaveSnapshot),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentChange {
    Text(TextChangeSet),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ContentOutcome {
    pub effect: ContentEffect,
    pub content_changed: bool,
    pub view_changed: bool,
    pub change: Option<ContentChange>,
}

impl ContentOutcome {
    fn new(effect: ContentEffect, content_changed: bool, view_changed: bool) -> Self {
        Self {
            effect,
            content_changed,
            view_changed,
            change: None,
        }
    }

    fn with_change(mut self, change: Option<TextChangeSet>) -> Self {
        self.change = change.map(ContentChange::Text);
        self
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContentResult {
    Handled(ContentOutcome),
    NotHandled,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContentActionResult {
    Handled {
        outcome: ContentOutcome,
        transaction: Option<ContentTransaction>,
    },
    Rejected(ContentTransactionError),
    NotHandled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentTransaction {
    Text(BufferTransactionData),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentTransactionError {
    Text(TextTransactionError),
}

impl ContentTransaction {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(data) => data.text.is_empty(),
        }
    }

    pub fn compose(&self, next: &Self) -> Result<Self, ContentTransactionError> {
        match (self, next) {
            (Self::Text(first), Self::Text(next)) => Ok(Self::Text(BufferTransactionData {
                text: first
                    .text
                    .compose(&next.text)
                    .map_err(ContentTransactionError::Text)?,
            })),
        }
    }
}

#[derive(Clone)]
pub enum Content {
    Buffer(Buffer),
    StatusBar(StatusBar),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContentKind {
    Buffer,
    StatusBar,
}

impl Content {
    pub fn kind(&self) -> ContentKind {
        match self {
            Self::Buffer(_) => ContentKind::Buffer,
            Self::StatusBar(_) => ContentKind::StatusBar,
        }
    }

    pub fn text_snapshot(&self) -> Option<TextSnapshot> {
        match self {
            Self::Buffer(buffer) => Some(TextSnapshot::new(buffer.slice())),
            Self::StatusBar(_) => None,
        }
    }

    pub(crate) fn query(&self, query: ContentQuery) -> ContentData {
        match (self, query) {
            (Self::Buffer(buffer), ContentQuery::TextRows(range)) => {
                ContentData::TextRows(text_rows(buffer, range))
            }
            (Self::Buffer(buffer), ContentQuery::TextPoints(offsets)) => ContentData::TextPoints(
                offsets
                    .into_iter()
                    .map(|offset| buffer.text_point(offset))
                    .collect(),
            ),
            (Self::Buffer(buffer), ContentQuery::ResourceName) => {
                ContentData::ResourceName(buffer.file_name().map(str::to_owned))
            }
            (Self::Buffer(buffer), ContentQuery::ResourcePath) => ContentData::ResourcePath(
                buffer
                    .path()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
            (Self::Buffer(buffer), ContentQuery::BackingState) => {
                ContentData::BackingState(buffer.backing_state())
            }
            (Self::Buffer(buffer), ContentQuery::DirtyState) => {
                ContentData::DirtyState(if buffer.modified() {
                    DirtyState::Modified
                } else {
                    DirtyState::Clean
                })
            }
            (Self::Buffer(buffer), ContentQuery::SaveState) => {
                ContentData::SaveState(buffer.save_state())
            }
            (Self::Buffer(buffer), ContentQuery::TextMetrics) => {
                ContentData::TextMetrics(TextMetrics {
                    line_count: buffer.len_lines(),
                    char_count: buffer.slice().len_chars(),
                })
            }
            _ => ContentData::Unsupported,
        }
    }

    pub fn plan_edit(
        &self,
        command: EditCommand,
        selections: &Selections,
    ) -> Option<ContentEditPlan> {
        match self {
            Self::Buffer(buffer) => Some(buffer.plan_edit(command, selections)),
            Self::StatusBar(_) => None,
        }
    }

    pub fn apply(&mut self, action: ContentAction) -> ContentActionResult {
        match (self, action) {
            (Self::Buffer(buffer), ContentAction::Text(change)) => {
                let transaction = match buffer.apply_content_change(change.clone()) {
                    Ok(transaction) => transaction.map(ContentTransaction::Text),
                    Err(error) => {
                        return ContentActionResult::Rejected(ContentTransactionError::Text(error));
                    }
                };
                let changed = transaction.is_some();
                ContentActionResult::Handled {
                    outcome: ContentOutcome::new(ContentEffect::None, changed, false)
                        .with_change(changed.then_some(change)),
                    transaction,
                }
            }
            (Self::StatusBar(_), ContentAction::Text(_)) => ContentActionResult::NotHandled,
        }
    }

    pub fn create_view_state(&self) -> ContentViewState {
        match self {
            Self::Buffer(_) => ContentViewState::buffer(),
            Self::StatusBar(_) => ContentViewState::unbound_status_bar(),
        }
    }

    pub fn transform_view_state(
        &self,
        state: &mut ContentViewState,
        change: &ContentChange,
    ) -> Result<bool, crate::core::content_view_state::ContentViewStateError> {
        let state_kind = state.kind();
        match (self, state, change) {
            (
                Self::Buffer(buffer),
                ContentViewState::Buffer(state),
                ContentChange::Text(change),
            ) => Ok(buffer.transform_selections(state.selections_mut(), change)),
            (Self::StatusBar(_), ContentViewState::StatusBar(_), ContentChange::Text(_)) => {
                Ok(false)
            }
            (Self::Buffer(_), ContentViewState::StatusBar(_), ContentChange::Text(_))
            | (Self::StatusBar(_), ContentViewState::Buffer(_), ContentChange::Text(_)) => Err(
                crate::core::content_view_state::ContentViewStateError::KindMismatch {
                    content: self.kind(),
                    state: state_kind,
                },
            ),
        }
    }

    pub fn selections_are_valid(&self, selections: &Selections) -> bool {
        match self {
            Self::Buffer(buffer) => {
                let mut reconciled = selections.clone();
                !buffer.reconcile_selections(&mut reconciled)
            }
            Self::StatusBar(_) => false,
        }
    }

    pub fn apply_transaction(
        &mut self,
        data: &ContentTransaction,
        direction: crate::core::transaction::TransactionDirection,
    ) -> Result<Option<ContentChange>, ContentTransactionError> {
        match (self, data) {
            (Self::Buffer(buffer), ContentTransaction::Text(data)) => buffer
                .apply_transaction_data(data, direction)
                .map(|change| Some(ContentChange::Text(change)))
                .map_err(ContentTransactionError::Text),
            (Self::StatusBar(_), _) => Ok(None),
        }
    }

    pub fn execute(&mut self, input: ContentInput) -> ContentResult {
        match (self, input) {
            (Self::Buffer(buffer), ContentInput::Save) => match buffer.path().cloned() {
                Some(path) => ContentResult::Handled(
                    ContentEffect::Save(SaveSnapshot {
                        path,
                        bytes: buffer.slice().to_string(),
                        revision: buffer.revision(),
                        state: buffer.state_id(),
                    })
                    .into(),
                ),
                None => {
                    let changed = buffer.save_state() != SaveState::Failed;
                    buffer.set_save_state(SaveState::Failed);
                    ContentResult::Handled(ContentOutcome::new(ContentEffect::None, changed, false))
                }
            },
            (
                Self::Buffer(buffer),
                ContentInput::Event(ContentEvent::SaveFinished { state, result }),
            ) => {
                let before_modified = buffer.modified();
                let before_save_state = buffer.save_state();
                let before_backing_state = buffer.backing_state();
                let save_state = match result {
                    Ok(()) if buffer.mark_saved(state) => SaveState::Saved,
                    Ok(()) => SaveState::Idle,
                    Err(_) => SaveState::Failed,
                };
                buffer.set_save_state(save_state);
                ContentResult::Handled(ContentOutcome::new(
                    ContentEffect::None,
                    buffer.modified() != before_modified
                        || buffer.save_state() != before_save_state
                        || buffer.backing_state() != before_backing_state,
                    false,
                ))
            }
            (Self::StatusBar(_), _) => ContentResult::NotHandled,
        }
    }
}

fn text_rows(buffer: &Buffer, range: RowRange) -> Vec<String> {
    let total = buffer.len_lines();
    let start = range.start.min(total);
    let end = range.end.min(total).max(start);
    (start..end)
        .map(|row| {
            buffer
                .line(row)
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string()
        })
        .collect()
}

impl From<ContentEffect> for ContentOutcome {
    fn from(effect: ContentEffect) -> Self {
        Self::new(effect, false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_creates_text_view_state() {
        let content = Content::Buffer(Buffer::new());
        assert_eq!(content.kind(), ContentKind::Buffer);
        assert!(matches!(
            content.create_view_state(),
            ContentViewState::Buffer(_)
        ));
    }

    #[test]
    fn status_bar_creates_an_explicitly_unbound_view() {
        let content = Content::StatusBar(StatusBar::new());
        assert_eq!(content.kind(), ContentKind::StatusBar);
        let ContentViewState::StatusBar(state) = content.create_view_state() else {
            panic!("status-bar content must create status-bar view state");
        };
        assert_eq!(state.target(), None);
    }

    #[test]
    fn contents_explicitly_report_save_support() {
        let mut buffer = Content::Buffer(Buffer::new());
        assert!(matches!(
            buffer.execute(ContentInput::Save),
            ContentResult::Handled(_)
        ));

        let mut status = Content::StatusBar(StatusBar::new());
        assert_eq!(
            status.execute(ContentInput::Save),
            ContentResult::NotHandled
        );
    }
}
