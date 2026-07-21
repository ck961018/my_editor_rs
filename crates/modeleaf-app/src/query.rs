use std::collections::HashMap;

use crate::mode::FaceRegistry;
use crate::presentation::PresentationLayerStore;
use crate::view::View;
use modeleaf_core::content::ContentKind;
use modeleaf_core::content_store::ContentStore;
use modeleaf_core::content_view_state::ContentViewState;
use modeleaf_protocol::content_query::{
    ContentData, ContentQuery, CursorStyle, RenderQuery, RenderQueryError, RowRange,
    SelectionShape, TextDecoration, TextPresentation, ViewData, ViewPresentation,
};
use modeleaf_protocol::ids::{ContentId, ViewId};

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

    fn view(&self, id: ViewId) -> Result<ViewData, RenderQueryError> {
        let view = self
            .views
            .get(&id)
            .ok_or(RenderQueryError::MissingView(id))?;
        let content = view.content();
        let content_kind = self
            .contents
            .kind(content)
            .ok_or(RenderQueryError::MissingContent(content))?;
        let presentation = match (content_kind, view.state()) {
            (ContentKind::Buffer, ContentViewState::Buffer(state)) => {
                let content_revision = self
                    .contents
                    .revision(content)
                    .ok_or(RenderQueryError::MissingContent(content))?;
                let policy = self
                    .presentation
                    .policy(id, content_revision, view.revision());
                ViewPresentation::Text(TextPresentation {
                    selections: state.selections().clone(),
                    cursor_style: policy.cursor_style.unwrap_or(CursorStyle::Default),
                    selection_shape: policy.selection_shape.unwrap_or(SelectionShape::Character),
                    selection_face: policy
                        .selection_face
                        .as_ref()
                        .map(|face| self.faces.resolve(face))
                        .unwrap_or_default(),
                })
            }
            (ContentKind::StatusBar, ContentViewState::StatusBar(_)) => ViewPresentation::StatusBar,
            (ContentKind::Buffer, ContentViewState::StatusBar(_))
            | (ContentKind::StatusBar, ContentViewState::Buffer(_)) => {
                return Err(RenderQueryError::IncompatibleContentViewState { view: id, content });
            }
        };
        Ok(ViewData {
            content,
            presentation,
        })
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
