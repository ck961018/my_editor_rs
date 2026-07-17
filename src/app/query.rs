use std::collections::HashMap;

use crate::app::view::View;
use crate::core::content_store::ContentStore;
use crate::protocol::content_query::{
    ContentData, ContentPresentation, ContentQuery, RenderQuery, TextPresentation, ViewData,
    ViewPresentation,
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
        let content = view.content();
        let presentation = match self
            .contents
            .presentation(content)
            .expect("view references existing content")
        {
            ContentPresentation::Text => {
                let selections = view
                    .selections()
                    .expect("text content creates selection-backed view state");
                ViewPresentation::Text(TextPresentation {
                    selections: selections.clone(),
                    cursor_style: view.cursor_style(),
                    selection_shape: view.selection_shape(),
                })
            }
            ContentPresentation::StatusBar => {
                debug_assert!(view.selections().is_none());
                ViewPresentation::StatusBar
            }
        };
        ViewData {
            content,
            presentation,
        }
    }
}
