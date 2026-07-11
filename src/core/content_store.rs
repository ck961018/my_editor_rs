use std::collections::HashMap;

use crate::core::content::Content;
use crate::core::content_runtime::ContentRuntime;
use crate::core::keymap::Keymap;
use crate::protocol::content_query::{
    ContentData, ContentQuery, DocumentStatus, RowRange, StatusBarData,
};
use crate::protocol::ids::ContentId;
use crate::protocol::key_event::KeyEvent;
use crate::protocol::status::StatusMessage;

#[derive(Default)]
pub struct ContentStore {
    contents: HashMap<ContentId, Content>,
}

impl ContentStore {
    pub fn insert(&mut self, id: ContentId, content: Content) {
        self.contents.insert(id, content);
    }

    pub fn keymap(&self, id: ContentId) -> Option<&Keymap> {
        self.contents.get(&id).map(Content::keymap)
    }

    pub fn resolve_key(
        &self,
        id: ContentId,
        key: KeyEvent,
    ) -> Option<crate::core::command::Command> {
        self.contents
            .get(&id)
            .and_then(|content| content.resolve_key(key))
    }

    pub fn create_runtime(&self, id: ContentId) -> Option<ContentRuntime> {
        self.contents.get(&id).map(Content::create_runtime)
    }

    #[allow(dead_code)] // Task 3 switches Dispatcher to this runtime-aware key resolution path.
    pub fn resolve_key_with_runtime(
        &self,
        id: ContentId,
        runtime: &ContentRuntime,
        key: KeyEvent,
    ) -> Option<crate::core::command::Command> {
        self.contents
            .get(&id)
            .and_then(|content| content.resolve_key_with_runtime(runtime, key))
    }

    pub fn execute(
        &mut self,
        id: ContentId,
        input: crate::core::content::ContentInput<'_>,
    ) -> crate::core::content::ContentEffect {
        self.contents
            .get_mut(&id)
            .map(|content| content.execute(input))
            .unwrap_or(crate::core::content::ContentEffect::None)
    }

    pub fn query(&self, id: ContentId, query: ContentQuery) -> ContentData {
        match (self.contents.get(&id), query) {
            (Some(Content::Buffer(buffer)), ContentQuery::TextRows(range)) => {
                ContentData::TextRows(text_rows(buffer, range))
            }
            (Some(Content::Buffer(buffer)), ContentQuery::TextLineCount) => {
                ContentData::TextLineCount(buffer.len_lines())
            }
            (Some(Content::Buffer(buffer)), ContentQuery::DocumentStatus) => {
                ContentData::DocumentStatus(document_status(buffer))
            }
            (Some(Content::StatusBar(status_bar)), ContentQuery::StatusBarData) => {
                ContentData::StatusBarData(
                    match self.query(status_bar.target_content_id(), ContentQuery::DocumentStatus) {
                        ContentData::DocumentStatus(status) => status,
                        ContentData::TextRows(_)
                        | ContentData::TextLineCount(_)
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
        .map(|row| buffer.line(row).trim_end_matches('\n').to_string())
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
    use crate::core::content::{Content, ContentEffect, ContentInput};
    use crate::core::status_bar::StatusBar;
    use crate::protocol::content_query::{ContentData, ContentQuery, RowRange};
    use crate::protocol::ids::ContentId;
    use crate::protocol::selection::{CursorPos, Selection, Selections};

    #[test]
    fn edit_with_view_runtime_updates_buffer_and_selection() {
        let id = ContentId(0);
        let mut store = ContentStore::default();
        store.insert(id, Content::Buffer(Buffer::new()));
        let mut selections = Selections::single(Selection::collapsed(CursorPos::origin()));
        let mut runtime = store.create_runtime(id).expect("content exists");

        let effect = store.execute(
            id,
            ContentInput::View {
                command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
                selections: &mut selections,
                runtime: &mut runtime,
            },
        );

        assert_eq!(effect, ContentEffect::None);
        assert_eq!(
            store.query(id, ContentQuery::TextRows(RowRange { start: 0, end: 1 })),
            ContentData::TextRows(vec!["x".to_string()])
        );
        assert_eq!(selections.primary().head().char_index, 1);
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
}
