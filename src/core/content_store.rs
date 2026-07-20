use std::collections::HashMap;
use std::fmt;

use crate::core::action::{ContentAction, ContentEditPlan};
use crate::core::command::EditCommand;
use crate::core::content::{
    Content, ContentActionResult, ContentChange, ContentResult, ContentTransaction,
    ContentTransactionError,
};
use crate::core::content_view_state::ContentViewState;
use crate::core::transaction::TransactionDirection;
use crate::protocol::content_query::{ContentData, ContentPresentation, ContentQuery};
use crate::protocol::ids::ContentId;
use crate::protocol::revision::Revision;
use crate::protocol::selection::Selections;

#[derive(Default)]
pub struct ContentStore {
    entries: HashMap<ContentId, ContentEntry>,
}

#[derive(Clone)]
struct ContentEntry {
    content: Content,
    revision: Revision,
}

pub(crate) struct ContentSnapshot {
    id: ContentId,
    entry: ContentEntry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DuplicateContentId {
    id: ContentId,
}

impl ContentStore {
    pub fn insert(&mut self, id: ContentId, content: Content) -> Result<(), DuplicateContentId> {
        if self.entries.contains_key(&id) {
            return Err(DuplicateContentId { id });
        }
        self.entries.insert(
            id,
            ContentEntry {
                content,
                revision: Revision::default(),
            },
        );
        Ok(())
    }

    pub fn contains(&self, id: ContentId) -> bool {
        self.entries.contains_key(&id)
    }

    #[cfg(test)]
    pub(crate) fn ids_for_test(&self) -> Vec<ContentId> {
        self.entries.keys().copied().collect()
    }

    pub(crate) fn snapshot(&self, id: ContentId) -> Option<ContentSnapshot> {
        self.entries
            .get(&id)
            .cloned()
            .map(|entry| ContentSnapshot { id, entry })
    }

    pub(crate) fn restore(&mut self, snapshot: ContentSnapshot) {
        self.entries.insert(snapshot.id, snapshot.entry);
    }

    pub fn create_view_state(&self, id: ContentId) -> Option<ContentViewState> {
        self.entries
            .get(&id)
            .map(|entry| entry.content.create_view_state())
    }

    pub fn presentation(&self, id: ContentId) -> Option<ContentPresentation> {
        self.entries
            .get(&id)
            .map(|entry| entry.content.presentation())
    }

    pub fn transform_view_state(
        &self,
        id: ContentId,
        state: &mut ContentViewState,
        change: &ContentChange,
    ) -> Option<bool> {
        self.entries
            .get(&id)
            .map(|entry| entry.content.transform_view_state(state, change))
    }

    pub fn selections_are_valid(&self, id: ContentId, selections: &Selections) -> Option<bool> {
        self.entries
            .get(&id)
            .map(|entry| entry.content.selections_are_valid(selections))
    }

    pub fn plan_edit(
        &self,
        id: ContentId,
        command: EditCommand,
        selections: &Selections,
    ) -> Option<ContentEditPlan> {
        self.entries
            .get(&id)
            .and_then(|entry| entry.content.plan_edit(command, selections))
    }

    pub fn apply(&mut self, id: ContentId, action: ContentAction) -> ContentActionResult {
        let Some(entry) = self.entries.get_mut(&id) else {
            return ContentActionResult::NotHandled;
        };
        let result = entry.content.apply(action);
        if matches!(
            &result,
            ContentActionResult::Handled { outcome, .. } if outcome.content_changed
        ) {
            entry.revision.next();
        }
        result
    }

    pub fn apply_transaction(
        &mut self,
        id: ContentId,
        data: &ContentTransaction,
        direction: TransactionDirection,
    ) -> Result<Option<ContentChange>, ContentTransactionError> {
        let Some(entry) = self.entries.get_mut(&id) else {
            return Ok(None);
        };
        let change = entry.content.apply_transaction(data, direction)?;
        if change.is_some() {
            entry.revision.next();
        }
        Ok(change)
    }

    pub fn execute(
        &mut self,
        id: ContentId,
        input: crate::core::content::ContentInput,
    ) -> crate::core::content::ContentResult {
        let Some(entry) = self.entries.get_mut(&id) else {
            return ContentResult::NotHandled;
        };
        let result = entry.content.execute(input);
        if matches!(&result, ContentResult::Handled(outcome) if outcome.content_changed) {
            entry.revision.next();
        }
        result
    }

    pub fn revision(&self, id: ContentId) -> Option<Revision> {
        let entry = self.entries.get(&id)?;
        let dependency = entry
            .content
            .revision_dependency()
            .and_then(|id| self.entries.get(&id))
            .map(|entry| entry.revision);
        Some(entry.content.effective_revision(entry.revision, dependency))
    }

    pub fn text_snapshot(&self, id: ContentId) -> Option<crate::core::text_snapshot::TextSnapshot> {
        self.entries.get(&id)?.content.text_snapshot()
    }

    pub fn query(&self, id: ContentId, query: ContentQuery) -> ContentData {
        let Some(entry) = self.entries.get(&id) else {
            return ContentData::Unsupported;
        };
        let dependency = entry
            .content
            .dependency_query(&query)
            .map(|dependency| self.query(dependency.id, dependency.query));
        entry.content.query(query, dependency)
    }
}

impl fmt::Display for DuplicateContentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "content id {:?} is already registered", self.id)
    }
}

impl std::error::Error for DuplicateContentId {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::action::ContentAction;
    use crate::core::buffer::Buffer;
    use crate::core::command::EditCommand;
    use crate::core::content::Content;
    use crate::core::status_bar::StatusBar;
    use crate::core::transaction::{TextChangeSet, TextEdit};
    use crate::protocol::content_query::{
        ContentData, ContentPresentation, ContentQuery, RowRange, StatusBarData,
    };
    use crate::protocol::ids::ContentId;
    use crate::protocol::status::StatusMessage;

    fn apply_planned_edit(store: &mut ContentStore, id: ContentId, command: EditCommand) {
        let selections = store
            .create_view_state(id)
            .unwrap()
            .selections()
            .unwrap()
            .clone();
        let plan = store.plan_edit(id, command, &selections).unwrap();
        if let Some(action) = plan.action {
            assert!(matches!(
                store.apply(id, action),
                ContentActionResult::Handled { .. }
            ));
        }
    }

    #[test]
    fn planned_edit_keeps_view_state_out_of_content_application() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        store.insert(id, Content::Buffer(Buffer::new())).unwrap();
        let selections = store
            .create_view_state(id)
            .unwrap()
            .selections()
            .unwrap()
            .clone();

        let plan = store
            .plan_edit(id, EditCommand::InsertText("x".to_string()), &selections)
            .unwrap();

        assert_eq!(selections.primary().head().char_index, 0);
        assert_eq!(plan.selections.primary().head().char_index, 1);
        assert!(matches!(plan.action, Some(ContentAction::Text(_))));
        assert!(matches!(
            store.query(id, ContentQuery::TextRows(RowRange { start: 0, end: 1 })),
            ContentData::TextRows(ref rows) if rows == &[String::new()]
        ));

        let outcome = store.apply(id, plan.action.unwrap());

        assert!(matches!(
            outcome,
            ContentActionResult::Handled { ref outcome, .. }
                if outcome.content_changed && !outcome.view_changed
        ));
        assert_eq!(
            store.query(id, ContentQuery::TextRows(RowRange { start: 0, end: 1 })),
            ContentData::TextRows(vec!["x".to_string()])
        );
    }

    #[test]
    fn invalid_content_action_is_rejected_without_mutation() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        store.insert(id, Content::Buffer(Buffer::new())).unwrap();
        let revision = store.revision(id);
        let change = TextChangeSet::from_edits(1, vec![TextEdit::new(1..1, "x")]).unwrap();

        let outcome = store.apply(id, ContentAction::Text(change));

        assert!(matches!(outcome, ContentActionResult::Rejected(_)));
        assert_eq!(store.revision(id), revision);
        assert_eq!(
            store.query(id, ContentQuery::TextRows(RowRange { start: 0, end: 1 })),
            ContentData::TextRows(vec![String::new()])
        );
    }

    #[test]
    fn planned_movement_is_only_a_view_change() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        let mut buffer = Buffer::new();
        buffer.insert_char(0, 'x');
        store.insert(id, Content::Buffer(buffer)).unwrap();
        let selections = Selections::single(crate::protocol::selection::Selection::collapsed(
            crate::protocol::selection::TextOffset::origin(),
        ));

        let plan = store
            .plan_edit(id, EditCommand::MoveRightBy(1), &selections)
            .unwrap();

        assert_eq!(plan.action, None);
        assert_eq!(plan.selections.primary().head().char_index, 1);
    }

    #[test]
    fn status_bar_queries_document_status_without_a_type_probe() {
        let buffer_id = ContentId(0);
        let status_bar_id = ContentId(1);
        let mut store = ContentStore::default();
        store
            .insert(buffer_id, Content::Buffer(Buffer::new()))
            .unwrap();
        store
            .insert(status_bar_id, Content::StatusBar(StatusBar::new(buffer_id)))
            .unwrap();

        assert!(matches!(
            store.query(status_bar_id, ContentQuery::StatusBarData),
            ContentData::StatusBarData(_)
        ));
    }

    #[test]
    fn status_bar_uses_default_data_when_its_target_is_missing() {
        let status_bar_id = ContentId(1);
        let missing = ContentId(99);
        let mut store = ContentStore::default();
        store
            .insert(status_bar_id, Content::StatusBar(StatusBar::new(missing)))
            .unwrap();

        assert_eq!(
            store.query(status_bar_id, ContentQuery::StatusBarData),
            ContentData::StatusBarData(StatusBarData {
                file_name: None,
                modified: false,
                message: StatusMessage::None,
            })
        );
        assert_eq!(store.revision(status_bar_id), Some(Revision(0)));
    }

    #[test]
    fn status_bar_uses_default_data_for_a_non_document_target() {
        let target = ContentId(1);
        let status_bar_id = ContentId(2);
        let mut store = ContentStore::default();
        store
            .insert(target, Content::StatusBar(StatusBar::new(ContentId(0))))
            .unwrap();
        store
            .insert(status_bar_id, Content::StatusBar(StatusBar::new(target)))
            .unwrap();

        assert_eq!(
            store.query(status_bar_id, ContentQuery::StatusBarData),
            ContentData::StatusBarData(StatusBarData {
                file_name: None,
                modified: false,
                message: StatusMessage::None,
            })
        );
    }

    #[test]
    fn contains_reports_inserted_content_ids() {
        let mut store = ContentStore::default();
        store
            .insert(ContentId(4), Content::Buffer(Buffer::new()))
            .unwrap();

        assert!(store.contains(ContentId(4)));
        assert!(!store.contains(ContentId(5)));
    }

    #[test]
    fn presentation_is_dispatched_by_content() {
        let buffer_id = ContentId(4);
        let status_bar_id = ContentId(5);
        let mut store = ContentStore::default();
        store
            .insert(buffer_id, Content::Buffer(Buffer::new()))
            .unwrap();
        store
            .insert(status_bar_id, Content::StatusBar(StatusBar::new(buffer_id)))
            .unwrap();

        assert_eq!(
            store.presentation(buffer_id),
            Some(ContentPresentation::Text)
        );
        assert_eq!(
            store.presentation(status_bar_id),
            Some(ContentPresentation::StatusBar)
        );
        assert_eq!(store.presentation(ContentId(99)), None);
    }

    #[test]
    fn duplicate_id_is_rejected_without_replacing_content_or_revision() {
        let id = ContentId(4);
        let mut store = ContentStore::default();
        store.insert(id, Content::Buffer(Buffer::new())).unwrap();
        apply_planned_edit(&mut store, id, EditCommand::InsertText("x".to_string()));

        assert_eq!(
            store.insert(id, Content::StatusBar(StatusBar::new(id))),
            Err(DuplicateContentId { id })
        );
        assert_eq!(
            store.query(id, ContentQuery::TextRows(RowRange { start: 0, end: 1 })),
            ContentData::TextRows(vec!["x".to_string()])
        );
        assert_eq!(store.revision(id), Some(Revision(1)));
    }

    #[test]
    fn restoring_content_snapshot_does_not_replace_other_entries() {
        let target = ContentId(4);
        let other = ContentId(5);
        let mut store = ContentStore::default();
        store
            .insert(target, Content::Buffer(Buffer::new()))
            .unwrap();
        store.insert(other, Content::Buffer(Buffer::new())).unwrap();
        let snapshot = store.snapshot(target).unwrap();

        apply_planned_edit(
            &mut store,
            target,
            EditCommand::InsertText("target".to_string()),
        );
        apply_planned_edit(
            &mut store,
            other,
            EditCommand::InsertText("other".to_string()),
        );
        store.restore(snapshot);

        assert_eq!(
            store.query(
                target,
                ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
            ),
            ContentData::TextRows(vec![String::new()])
        );
        assert_eq!(
            store.query(other, ContentQuery::TextRows(RowRange { start: 0, end: 1 }),),
            ContentData::TextRows(vec!["other".to_string()])
        );
        assert_eq!(store.revision(target), Some(Revision(0)));
        assert_eq!(store.revision(other), Some(Revision(1)));
    }

    #[test]
    fn handled_inputs_advance_content_revision() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        store.insert(id, Content::Buffer(Buffer::new())).unwrap();
        assert_eq!(store.revision(id), Some(Revision(0)));
        apply_planned_edit(&mut store, id, EditCommand::InsertText("x".to_string()));

        assert_eq!(store.revision(id), Some(Revision(1)));
    }

    #[test]
    fn status_bar_revision_tracks_its_target_document() {
        let buffer = ContentId(0);
        let status = ContentId(1);
        let mut store = ContentStore::default();
        store
            .insert(buffer, Content::Buffer(Buffer::new()))
            .unwrap();
        store
            .insert(status, Content::StatusBar(StatusBar::new(buffer)))
            .unwrap();
        apply_planned_edit(&mut store, buffer, EditCommand::InsertText("x".to_string()));

        assert_eq!(store.revision(status), Some(Revision(1)));
    }

    #[test]
    fn movement_and_no_op_edit_do_not_advance_content_revision() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        store.insert(id, Content::Buffer(Buffer::new())).unwrap();
        apply_planned_edit(&mut store, id, EditCommand::MoveLeftBy(1));
        apply_planned_edit(&mut store, id, EditCommand::Delete(-1));

        assert_eq!(store.revision(id), Some(Revision(0)));
    }

    #[test]
    fn text_rows_hide_both_characters_of_crlf() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(
            &mut crate::protocol::selection::Selections::single(
                crate::protocol::selection::Selection::collapsed(
                    crate::protocol::selection::TextOffset::origin(),
                ),
            ),
            "a\r\nb",
        );
        store.insert(id, Content::Buffer(buffer)).unwrap();

        assert_eq!(
            store.query(id, ContentQuery::TextRows(RowRange { start: 0, end: 2 })),
            ContentData::TextRows(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(store.text_snapshot(id).unwrap().to_owned_string(), "a\r\nb");
    }
}
