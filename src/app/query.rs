use std::collections::HashMap;

use crate::app::mode::{FaceRegistry, ModeContentStore, ModeViewStore};
use crate::app::view::View;
use crate::core::content_store::ContentStore;
use crate::protocol::content_query::{
    ContentData, ContentPresentation, ContentQuery, CursorStyle, RenderQuery, RowRange,
    SelectionShape, TextDecoration, TextPresentation, ViewData, ViewPresentation,
};
use crate::protocol::ids::{ContentId, ViewId};

pub(super) struct AppQuery<'a> {
    pub(super) contents: &'a ContentStore,
    pub(super) views: &'a HashMap<ViewId, View>,
    pub(super) view_modes: &'a ModeViewStore,
    pub(super) mode_contents: &'a ModeContentStore,
    pub(super) faces: &'a FaceRegistry,
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
                let context = crate::app::mode::ModeViewContext::new(id, view, self.contents);
                let selections = view
                    .selections()
                    .expect("text content creates selection-backed view state");
                let policy = self
                    .view_modes
                    .view_policy(id, &context, self.mode_contents);
                ViewPresentation::Text(TextPresentation {
                    selections: selections.clone(),
                    cursor_style: policy.cursor_style.unwrap_or(CursorStyle::Default),
                    selection_shape: policy.selection_shape.unwrap_or(SelectionShape::Character),
                    selection_face: policy
                        .selection_face
                        .as_ref()
                        .map(|face| self.faces.resolve(face))
                        .unwrap_or_default(),
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

    fn decorations(&self, id: ViewId, visible_rows: RowRange) -> Vec<TextDecoration> {
        let Some(view) = self.views.get(&id) else {
            return Vec::new();
        };
        let context = crate::app::mode::ModeViewContext::new(id, view, self.contents);
        self.view_modes
            .decorations(id, &context, self.mode_contents, self.faces, visible_rows)
    }
}
