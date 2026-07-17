use std::collections::HashMap;

use crate::core::content::{Content, ContentChange, ContentResult};
use crate::core::content_view_state::ContentViewState;
use crate::core::mode_name::ModeName;
use crate::protocol::content_query::{
    ContentData, ContentQuery, DocumentStatus, RowRange, StatusBarData,
};
use crate::protocol::ids::ContentId;
use crate::protocol::revision::Revision;
use crate::protocol::status::StatusMessage;

#[derive(Default)]
pub struct ContentStore {
    contents: HashMap<ContentId, Content>,
    revisions: HashMap<ContentId, Revision>,
}

impl ContentStore {
    pub fn insert(&mut self, id: ContentId, content: Content) {
        self.contents.insert(id, content);
        self.revisions.insert(id, Revision::default());
    }

    pub fn contains(&self, id: ContentId) -> bool {
        self.contents.contains_key(&id)
    }

    pub fn create_view_state(&self, id: ContentId) -> Option<ContentViewState> {
        self.contents.get(&id).map(Content::create_view_state)
    }

    pub fn default_mode(&self, id: ContentId) -> Option<ModeName> {
        self.contents.get(&id).and_then(Content::default_mode)
    }

    pub fn transform_view_state(
        &self,
        id: ContentId,
        state: &mut ContentViewState,
        change: &ContentChange,
    ) -> Option<bool> {
        self.contents
            .get(&id)
            .map(|content| content.transform_view_state(state, change))
    }

    pub fn execute(
        &mut self,
        id: ContentId,
        input: crate::core::content::ContentInput<'_>,
    ) -> crate::core::content::ContentResult {
        let result = self
            .contents
            .get_mut(&id)
            .map(|content| content.execute(input))
            .unwrap_or(ContentResult::NotHandled);
        if matches!(&result, ContentResult::Handled(outcome) if outcome.content_changed) {
            self.revisions
                .get_mut(&id)
                .expect("inserted content has a revision")
                .next();
        }
        result
    }

    pub fn revision(&self, id: ContentId) -> Option<Revision> {
        let own = self.revisions.get(&id).copied()?;
        match self.contents.get(&id) {
            Some(Content::StatusBar(status_bar)) => Some(
                self.revisions
                    .get(&status_bar.target_content_id())
                    .copied()
                    .map_or(own, |target| own.max(target)),
            ),
            Some(Content::Buffer(_)) => Some(own),
            None => None,
        }
    }

    pub fn query(&self, id: ContentId, query: ContentQuery) -> ContentData {
        match (self.contents.get(&id), query) {
            (Some(Content::Buffer(buffer)), ContentQuery::TextRows(range)) => {
                ContentData::TextRows(text_rows(buffer, range))
            }
            (Some(Content::Buffer(buffer)), ContentQuery::TextPoints(offsets)) => {
                ContentData::TextPoints(
                    offsets
                        .into_iter()
                        .map(|offset| buffer.text_point(offset))
                        .collect(),
                )
            }
            (Some(Content::Buffer(buffer)), ContentQuery::DocumentStatus) => {
                ContentData::DocumentStatus(document_status(buffer))
            }
            (Some(Content::StatusBar(status_bar)), ContentQuery::StatusBarData) => {
                ContentData::StatusBarData(
                    match self.query(status_bar.target_content_id(), ContentQuery::DocumentStatus) {
                        ContentData::DocumentStatus(status) => status,
                        ContentData::TextRows(_)
                        | ContentData::TextPoints(_)
                        | ContentData::StatusBarData(_)
                        | ContentData::Unsupported => default_status_bar_data(),
                    },
                )
            }
            _ => ContentData::Unsupported,
        }
    }
}

fn text_rows(buffer: &crate::core::buffer::Buffer, range: RowRange) -> Vec<String> {
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

fn document_status(buffer: &crate::core::buffer::Buffer) -> DocumentStatus {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::command::{ContentCommand, EditCommand};
    use crate::core::content::{Content, ContentEffect, ContentInput, ContentResult};
    use crate::core::status_bar::StatusBar;
    use crate::protocol::content_query::{ContentData, ContentQuery, RowRange};
    use crate::protocol::ids::ContentId;

    #[test]
    fn edit_with_view_state_updates_buffer_and_selection() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        store.insert(id, Content::Buffer(Buffer::new()));
        let mut state = store.create_view_state(id).expect("content exists");

        let effect = store.execute(
            id,
            ContentInput::View {
                command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
                state: &mut state,
            },
        );

        assert!(matches!(
            effect,
            ContentResult::Handled(ref outcome)
                if outcome.effect == ContentEffect::None
                    && outcome.content_changed
                    && outcome.view_changed
        ));
        assert_eq!(
            store.query(id, ContentQuery::TextRows(RowRange { start: 0, end: 1 })),
            ContentData::TextRows(vec!["x".to_string()])
        );
        assert_eq!(state.selections().unwrap().primary().head().char_index, 1);
    }

    #[test]
    fn status_bar_queries_document_status_without_a_type_probe() {
        let buffer_id = ContentId(0);
        let status_bar_id = ContentId(1);
        let mut store = ContentStore::default();
        store.insert(buffer_id, Content::Buffer(Buffer::new()));
        store.insert(status_bar_id, Content::StatusBar(StatusBar::new(buffer_id)));

        assert!(matches!(
            store.query(status_bar_id, ContentQuery::StatusBarData),
            ContentData::StatusBarData(_)
        ));
    }

    #[test]
    fn contains_reports_inserted_content_ids() {
        let mut store = ContentStore::default();
        store.insert(ContentId(4), Content::Buffer(Buffer::new()));

        assert!(store.contains(ContentId(4)));
        assert!(!store.contains(ContentId(5)));
    }

    #[test]
    fn handled_inputs_advance_content_revision() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        store.insert(id, Content::Buffer(Buffer::new()));
        let mut state = store.create_view_state(id).unwrap();

        assert_eq!(store.revision(id), Some(Revision(0)));
        store.execute(
            id,
            ContentInput::View {
                command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
                state: &mut state,
            },
        );

        assert_eq!(store.revision(id), Some(Revision(1)));
    }

    #[test]
    fn status_bar_revision_tracks_its_target_document() {
        let buffer = ContentId(0);
        let status = ContentId(1);
        let mut store = ContentStore::default();
        store.insert(buffer, Content::Buffer(Buffer::new()));
        store.insert(status, Content::StatusBar(StatusBar::new(buffer)));
        let mut state = store.create_view_state(buffer).unwrap();

        store.execute(
            buffer,
            ContentInput::View {
                command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
                state: &mut state,
            },
        );

        assert_eq!(store.revision(status), Some(Revision(1)));
    }

    #[test]
    fn movement_and_no_op_edit_do_not_advance_content_revision() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        store.insert(id, Content::Buffer(Buffer::new()));
        let mut state = store.create_view_state(id).unwrap();

        store.execute(
            id,
            ContentInput::View {
                command: ContentCommand::Edit(EditCommand::MoveLeftBy(1)),
                state: &mut state,
            },
        );
        store.execute(
            id,
            ContentInput::View {
                command: ContentCommand::Edit(EditCommand::Delete(-1)),
                state: &mut state,
            },
        );

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
        store.insert(id, Content::Buffer(buffer));

        assert_eq!(
            store.query(id, ContentQuery::TextRows(RowRange { start: 0, end: 2 })),
            ContentData::TextRows(vec!["a".to_string(), "b".to_string()])
        );
    }
}
