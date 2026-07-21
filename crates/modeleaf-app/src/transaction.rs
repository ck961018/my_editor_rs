use std::collections::HashMap;

use modeleaf_core::content::{ContentTransaction, ContentTransactionError};
use modeleaf_protocol::ids::{ContentId, ViewId};
use modeleaf_protocol::selection::Selections;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionRecord {
    pub target: ContentId,
    pub data: TransactionData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionData {
    pub content: ContentTransaction,
    pub view: ViewTransactionData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewTransactionData {
    Source {
        view: ViewId,
        before: Selections,
        after: Selections,
    },
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionManagerError {
    OwnerMismatch {
        expected: Option<ViewId>,
        actual: Option<ViewId>,
    },
    ViewParticipantMismatch,
    Content(ContentTransactionError),
}

#[derive(Clone, Default)]
pub struct TransactionManager {
    flows: HashMap<ContentId, TransactionFlow>,
}

#[derive(Clone, Default)]
struct TransactionFlow {
    active: Option<ActiveTransaction>,
    history: Vec<TransactionRecord>,
    cursor: usize,
}

#[derive(Clone)]
struct ActiveTransaction {
    owner: Option<ViewId>,
    data: Option<TransactionData>,
}

impl TransactionData {
    fn owner(&self) -> Option<ViewId> {
        self.view.owner()
    }

    fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    fn compose(&self, next: &Self) -> Result<Self, TransactionManagerError> {
        Ok(Self {
            content: self
                .content
                .compose(&next.content)
                .map_err(TransactionManagerError::Content)?,
            view: self.view.compose(&next.view)?,
        })
    }
}

impl ViewTransactionData {
    fn owner(&self) -> Option<ViewId> {
        match self {
            Self::Source { view, .. } => Some(*view),
            Self::None => None,
        }
    }

    fn compose(&self, next: &Self) -> Result<Self, TransactionManagerError> {
        match (self, next) {
            (
                Self::Source {
                    view: first_view,
                    before,
                    ..
                },
                Self::Source {
                    view: next_view,
                    after,
                    ..
                },
            ) if first_view == next_view => Ok(Self::Source {
                view: *first_view,
                before: before.clone(),
                after: after.clone(),
            }),
            (Self::None, Self::None) => Ok(Self::None),
            _ => Err(TransactionManagerError::ViewParticipantMismatch),
        }
    }
}

pub struct TransactionSnapshot {
    target: ContentId,
    flow: Option<TransactionFlowSnapshot>,
}

struct TransactionFlowSnapshot {
    active: Option<ActiveTransaction>,
    cursor: usize,
    history_len: usize,
    preserved_from: usize,
    preserved_history: Vec<TransactionRecord>,
}

impl TransactionManager {
    pub fn snapshot(&self, target: ContentId) -> TransactionSnapshot {
        let flow = self.flows.get(&target).map(|flow| TransactionFlowSnapshot {
            active: flow.active.clone(),
            cursor: flow.cursor,
            history_len: flow.history.len(),
            preserved_from: flow.history.len(),
            preserved_history: Vec::new(),
        });
        TransactionSnapshot { target, flow }
    }

    pub fn preserve_truncated_history(&self, snapshot: &mut TransactionSnapshot) {
        let Some(saved) = snapshot.flow.as_mut() else {
            return;
        };
        let flow = self
            .flows
            .get(&snapshot.target)
            .expect("snapshotted transaction flow still exists");
        if flow
            .active
            .as_ref()
            .and_then(|active| active.data.as_ref())
            .is_none_or(TransactionData::is_empty)
        {
            return;
        }
        let from = flow.cursor.min(saved.history_len);
        if from >= saved.preserved_from {
            return;
        }
        let mut history = flow.history[from..saved.preserved_from].to_vec();
        history.append(&mut saved.preserved_history);
        saved.preserved_from = from;
        saved.preserved_history = history;
    }

    pub fn restore(&mut self, snapshot: TransactionSnapshot) {
        let TransactionSnapshot { target, flow } = snapshot;
        let Some(snapshot) = flow else {
            self.flows.remove(&target);
            return;
        };
        let flow = self.flows.entry(target).or_default();
        flow.active = snapshot.active;
        flow.history.truncate(snapshot.preserved_from);
        flow.history.extend(snapshot.preserved_history);
        flow.history.truncate(snapshot.history_len);
        flow.cursor = snapshot.cursor;
    }

    pub fn begin(&mut self, target: ContentId, owner: Option<ViewId>) -> Option<TransactionRecord> {
        let same_owner = self
            .flows
            .get(&target)
            .and_then(|flow| flow.active.as_ref())
            .is_some_and(|active| active.owner == owner);
        if same_owner {
            return None;
        }
        let committed = self.commit(target);
        self.flows.entry(target).or_default().active =
            Some(ActiveTransaction { owner, data: None });
        committed
    }

    pub fn record(&mut self, record: TransactionRecord) -> Result<(), TransactionManagerError> {
        let owner = record.data.owner();
        let flow = self.flows.entry(record.target).or_default();
        let active = flow
            .active
            .get_or_insert(ActiveTransaction { owner, data: None });
        if active.owner != owner {
            return Err(TransactionManagerError::OwnerMismatch {
                expected: active.owner,
                actual: owner,
            });
        }
        active.data = Some(match active.data.take() {
            Some(current) => current.compose(&record.data)?,
            None => record.data,
        });
        Ok(())
    }

    pub fn commit(&mut self, target: ContentId) -> Option<TransactionRecord> {
        let flow = self.flows.get_mut(&target)?;
        let active = flow.active.take()?;
        let data = active.data.filter(|data| !data.is_empty())?;
        let record = TransactionRecord { target, data };
        flow.history.truncate(flow.cursor);
        flow.history.push(record.clone());
        flow.cursor = flow.history.len();
        Some(record)
    }

    pub fn rollback(&mut self, target: ContentId) -> Option<TransactionRecord> {
        let flow = self.flows.get_mut(&target)?;
        let active = flow.active.take()?;
        active.data.map(|data| TransactionRecord { target, data })
    }

    pub fn undo(&mut self, target: ContentId) -> Option<TransactionRecord> {
        let flow = self.flows.get_mut(&target)?;
        if flow.active.is_some() || flow.cursor == 0 {
            return None;
        }
        flow.cursor -= 1;
        Some(flow.history[flow.cursor].clone())
    }

    pub fn redo(&mut self, target: ContentId) -> Option<TransactionRecord> {
        let flow = self.flows.get_mut(&target)?;
        if flow.active.is_some() || flow.cursor >= flow.history.len() {
            return None;
        }
        let record = flow.history[flow.cursor].clone();
        flow.cursor += 1;
        Some(record)
    }

    pub fn active_owner(&self, target: ContentId) -> Option<Option<ViewId>> {
        self.flows
            .get(&target)
            .and_then(|flow| flow.active.as_ref())
            .map(|active| active.owner)
    }

    #[cfg(test)]
    pub(crate) fn behavior_for_test(
        &self,
        target: ContentId,
    ) -> (bool, Option<ViewId>, usize, usize) {
        let Some(flow) = self.flows.get(&target) else {
            return (false, None, 0, 0);
        };
        (
            flow.active.is_some(),
            flow.active.as_ref().and_then(|active| active.owner),
            flow.cursor,
            flow.history.len() - flow.cursor,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modeleaf_core::buffer::BufferTransactionData;
    use modeleaf_core::content::ContentTransaction;
    use modeleaf_core::transaction::{TextChangeSet, TextEdit, TextStateId, TextTransactionData};
    use modeleaf_protocol::selection::{Selection, TextOffset};

    fn selections(offset: usize) -> Selections {
        Selections::single(Selection::collapsed(TextOffset { char_index: offset }))
    }

    fn record(
        target: ContentId,
        view: Option<ViewId>,
        before_offset: usize,
        inserted: &str,
        before_state: u64,
    ) -> TransactionRecord {
        let forward = TextChangeSet::from_edits(
            before_offset,
            vec![TextEdit::new(before_offset..before_offset, inserted)],
        )
        .unwrap();
        let inverse = TextChangeSet::from_edits(
            before_offset + inserted.chars().count(),
            vec![TextEdit::new(
                before_offset..before_offset + inserted.chars().count(),
                "",
            )],
        )
        .unwrap();
        TransactionRecord {
            target,
            data: TransactionData {
                content: ContentTransaction::Text(BufferTransactionData {
                    text: TextTransactionData {
                        forward,
                        inverse,
                        before_state: TextStateId(before_state),
                        after_state: TextStateId(before_state + 1),
                    },
                }),
                view: match view {
                    Some(view) => ViewTransactionData::Source {
                        view,
                        before: selections(before_offset),
                        after: selections(before_offset + inserted.chars().count()),
                    },
                    None => ViewTransactionData::None,
                },
            },
        }
    }

    #[test]
    fn record_pairs_opaque_content_transaction_with_view_data() {
        let target = ContentId(1);
        let view = ViewId(2);
        let mut manager = TransactionManager::default();
        manager.begin(target, Some(view));
        manager
            .record(record(target, Some(view), 0, "a", 0))
            .unwrap();
        manager
            .record(record(target, Some(view), 1, "b", 1))
            .unwrap();

        let committed = manager.commit(target).unwrap();

        let TransactionData { content, view } = committed.data;
        let ContentTransaction::Text(content) = content;
        assert_eq!(content.text.before_state, TextStateId(0));
        assert_eq!(content.text.after_state, TextStateId(2));
        assert!(matches!(
            view,
            ViewTransactionData::Source {
                before,
                after,
                ..
            } if before.primary().head().char_index == 0
                && after.primary().head().char_index == 2
        ));
    }

    #[test]
    fn transaction_without_view_participant_needs_no_fake_view_id() {
        let target = ContentId(1);
        let mut manager = TransactionManager::default();
        manager.begin(target, None);
        manager.record(record(target, None, 0, "a", 0)).unwrap();

        assert!(matches!(
            manager.commit(target).unwrap().data.view,
            ViewTransactionData::None
        ));
    }

    #[test]
    fn changing_owner_checkpoints_the_previous_transaction() {
        let target = ContentId(1);
        let first = ViewId(2);
        let second = ViewId(3);
        let mut manager = TransactionManager::default();
        manager.begin(target, Some(first));
        manager
            .record(record(target, Some(first), 0, "a", 0))
            .unwrap();

        let committed = manager.begin(target, Some(second)).unwrap();

        assert_eq!(committed.data.owner(), Some(first));
        assert_eq!(manager.active_owner(target), Some(Some(second)));
    }

    #[test]
    fn undo_and_redo_traverse_committed_outer_records() {
        let target = ContentId(1);
        let mut manager = TransactionManager::default();
        manager.record(record(target, None, 0, "a", 0)).unwrap();
        manager.commit(target);

        assert!(manager.undo(target).is_some());
        assert!(manager.undo(target).is_none());
        assert!(manager.redo(target).is_some());
        assert!(manager.redo(target).is_none());
    }

    #[test]
    fn histories_are_isolated_by_content() {
        let first = ContentId(1);
        let second = ContentId(2);
        let mut manager = TransactionManager::default();
        manager.record(record(first, None, 0, "a", 0)).unwrap();
        manager.commit(first);
        manager.record(record(second, None, 0, "b", 0)).unwrap();
        manager.commit(second);

        assert_eq!(manager.undo(first).unwrap().target, first);
        assert!(manager.undo(first).is_none());
        assert_eq!(manager.undo(second).unwrap().target, second);
    }

    #[test]
    fn snapshot_restores_active_transaction_without_copying_committed_prefix() {
        let target = ContentId(1);
        let mut manager = TransactionManager::default();
        manager.begin(target, None);
        manager.record(record(target, None, 0, "a", 0)).unwrap();
        let mut snapshot = manager.snapshot(target);

        manager.preserve_truncated_history(&mut snapshot);
        manager.commit(target);
        manager.begin(target, None);
        manager.record(record(target, None, 1, "b", 1)).unwrap();
        manager.restore(snapshot);

        assert_eq!(manager.active_owner(target), Some(None));
        let restored = manager.commit(target).unwrap();
        let ContentTransaction::Text(content) = restored.data.content;
        assert_eq!(content.text.before_state, TextStateId(0));
        assert_eq!(content.text.after_state, TextStateId(1));
    }

    #[test]
    fn snapshot_restores_redo_tail_discarded_by_a_new_edit() {
        let target = ContentId(1);
        let mut manager = TransactionManager::default();
        for (offset, text, state) in [(0, "a", 0), (1, "b", 1)] {
            manager
                .record(record(target, None, offset, text, state))
                .unwrap();
            manager.commit(target);
        }
        manager.undo(target);
        let mut snapshot = manager.snapshot(target);

        manager.record(record(target, None, 1, "c", 1)).unwrap();
        manager.preserve_truncated_history(&mut snapshot);
        manager.commit(target);
        manager.restore(snapshot);

        let restored = manager.redo(target).unwrap();
        let ContentTransaction::Text(content) = restored.data.content;
        assert_eq!(content.text.after_state, TextStateId(2));
    }

    #[test]
    fn snapshot_removes_a_flow_created_by_the_failed_command() {
        let target = ContentId(1);
        let mut manager = TransactionManager::default();
        let snapshot = manager.snapshot(target);

        manager.begin(target, None);
        manager.record(record(target, None, 0, "a", 0)).unwrap();
        manager.restore(snapshot);

        assert_eq!(manager.active_owner(target), None);
        assert!(manager.undo(target).is_none());
    }
}
