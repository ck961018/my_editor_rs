use std::collections::HashMap;

use crate::app::view::View;
use crate::core::content_store::ContentStore;
use crate::protocol::content_query::{
    ContentData, ContentQuery, RenderQuery, TextPresentation, ViewData, ViewPresentation,
};
use crate::protocol::ids::{ContentId, ViewId};

pub(super) struct AppQuery<'a> {
    pub(super) contents: &'a ContentStore,
    pub(super) views: &'a HashMap<ViewId, View>,
}

impl RenderQuery for AppQuery<'_> {
    fn content(&self, cid: ContentId, query: ContentQuery) -> ContentData {
        self.contents.query(cid, query)
    }

    fn view(&self, id: ViewId) -> ViewData {
        let view = self.views.get(&id).expect("scene references existing view");
        let presentation = match view.selections() {
            Some(selections) => ViewPresentation::Text(TextPresentation {
                selections: selections.clone(),
                cursor_style: view.cursor_style(),
                selection_shape: view.selection_shape(),
            }),
            None => ViewPresentation::StatusBar,
        };
        ViewData {
            content: view.content(),
            presentation,
        }
    }
}
