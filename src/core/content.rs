use std::path::PathBuf;

use crate::core::action::{ContentAction, ContentEditPlan};
use crate::core::buffer::{Buffer, BufferTransactionData};
use crate::core::command::EditCommand;
use crate::core::content_view_state::ContentViewState;
use crate::core::status_bar::StatusBar;
use crate::core::text_snapshot::TextSnapshot;
use crate::core::transaction::{TextChangeSet, TextStateId, TextTransactionError};
use crate::protocol::content_query::{
    ContentData, ContentPresentation, ContentQuery, DocumentStatus, RowRange, StatusBarData,
};
use crate::protocol::ids::ContentId;
use crate::protocol::revision::Revision;
use crate::protocol::selection::Selections;
use crate::protocol::status::StatusMessage;

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

pub(crate) struct ContentDependencyQuery {
    pub id: ContentId,
    pub query: ContentQuery,
}

impl Content {
    pub fn text_snapshot(&self) -> Option<TextSnapshot> {
        match self {
            Self::Buffer(buffer) => Some(TextSnapshot::new(buffer.slice())),
            Self::StatusBar(_) => None,
        }
    }

    pub(crate) fn revision_dependency(&self) -> Option<ContentId> {
        match self {
            Self::Buffer(_) => None,
            Self::StatusBar(status_bar) => Some(status_bar.target_content_id()),
        }
    }

    pub(crate) fn effective_revision(
        &self,
        own: Revision,
        dependency: Option<Revision>,
    ) -> Revision {
        match self {
            Self::Buffer(_) => own,
            Self::StatusBar(_) => dependency.map_or(own, |revision| own.max(revision)),
        }
    }

    pub(crate) fn dependency_query(&self, query: &ContentQuery) -> Option<ContentDependencyQuery> {
        match (self, query) {
            (Self::StatusBar(status_bar), ContentQuery::StatusBarData) => {
                Some(ContentDependencyQuery {
                    id: status_bar.target_content_id(),
                    query: ContentQuery::DocumentStatus,
                })
            }
            _ => None,
        }
    }

    pub(crate) fn query(
        &self,
        query: ContentQuery,
        dependency: Option<ContentData>,
    ) -> ContentData {
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
            (Self::Buffer(buffer), ContentQuery::DocumentStatus) => {
                ContentData::DocumentStatus(document_status(buffer))
            }
            (Self::StatusBar(_), ContentQuery::StatusBarData) => {
                ContentData::StatusBarData(match dependency {
                    Some(ContentData::DocumentStatus(status)) => status,
                    _ => default_status_bar_data(),
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

    pub fn presentation(&self) -> ContentPresentation {
        match self {
            Self::Buffer(_) => ContentPresentation::Text,
            Self::StatusBar(_) => ContentPresentation::StatusBar,
        }
    }

    pub fn create_view_state(&self) -> ContentViewState {
        match self {
            Self::Buffer(_) => ContentViewState::text(),
            Self::StatusBar(_) => ContentViewState::stateless(),
        }
    }

    pub fn transform_view_state(
        &self,
        state: &mut ContentViewState,
        change: &ContentChange,
    ) -> bool {
        match (self, change) {
            (Self::Buffer(buffer), ContentChange::Text(change)) => buffer.transform_selections(
                state
                    .selections_mut()
                    .expect("text content requires selection-backed view state"),
                change,
            ),
            (Self::StatusBar(_), _) => {
                assert!(state.selections().is_none(), "content/view state mismatch");
                false
            }
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
                    let changed = buffer.status() != StatusMessage::SaveFailed;
                    buffer.set_status(StatusMessage::SaveFailed);
                    ContentResult::Handled(ContentOutcome::new(ContentEffect::None, changed, false))
                }
            },
            (
                Self::Buffer(buffer),
                ContentInput::Event(ContentEvent::SaveFinished { state, result }),
            ) => {
                let before_modified = buffer.modified();
                let before_status = buffer.status();
                match result {
                    Ok(()) => {
                        if buffer.mark_saved(state) {
                            buffer.set_status(StatusMessage::Saved);
                        }
                    }
                    Err(_) => buffer.set_status(StatusMessage::SaveFailed),
                }
                ContentResult::Handled(ContentOutcome::new(
                    ContentEffect::None,
                    buffer.modified() != before_modified || buffer.status() != before_status,
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

fn document_status(buffer: &Buffer) -> DocumentStatus {
    DocumentStatus {
        file_name: buffer.file_name().map(str::to_string),
        modified: buffer.modified(),
        message: buffer.status(),
    }
}

fn default_status_bar_data() -> StatusBarData {
    StatusBarData {
        file_name: None,
        modified: false,
        message: StatusMessage::None,
    }
}

impl From<ContentEffect> for ContentOutcome {
    fn from(effect: ContentEffect) -> Self {
        Self::new(effect, false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ids::ContentId;

    #[test]
    fn buffer_creates_text_view_state() {
        let content = Content::Buffer(Buffer::new());
        assert_eq!(content.presentation(), ContentPresentation::Text);
        assert!(content.create_view_state().selections().is_some());
    }

    #[test]
    fn status_bar_creates_stateless_view() {
        let content = Content::StatusBar(StatusBar::new(ContentId(0)));
        assert_eq!(content.presentation(), ContentPresentation::StatusBar);
        assert!(content.create_view_state().selections().is_none());
    }

    #[test]
    fn contents_explicitly_report_save_support() {
        let mut buffer = Content::Buffer(Buffer::new());
        assert!(matches!(
            buffer.execute(ContentInput::Save),
            ContentResult::Handled(_)
        ));

        let mut status = Content::StatusBar(StatusBar::new(ContentId(0)));
        assert_eq!(
            status.execute(ContentInput::Save),
            ContentResult::NotHandled
        );
    }
}
