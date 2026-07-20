use std::collections::HashMap;

use crate::app::mode::FaceRegistry;
use crate::app::presentation::PresentationLayerStore;
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
    pub(super) presentation: &'a PresentationLayerStore,
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
                let selections = view
                    .selections()
                    .expect("text content creates selection-backed view state");
                let content_revision = self
                    .contents
                    .revision(content)
                    .expect("view references existing content");
                let policy = self
                    .presentation
                    .policy(id, content_revision, view.revision());
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
        let content = view.content();
        let Some(content_revision) = self.contents.revision(content) else {
            return Vec::new();
        };
        let Some(snapshot) = self.contents.text_snapshot(content) else {
            return Vec::new();
        };
        self.presentation
            .decorations(
                id,
                content_revision,
                view.revision(),
                &snapshot,
                visible_rows,
            )
            .into_iter()
            .map(|decoration| TextDecoration {
                start: decoration.start,
                end: decoration.end,
                face: self.faces.resolve(&decoration.face),
            })
            .collect()
    }
}
