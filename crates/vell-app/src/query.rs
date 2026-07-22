use std::collections::HashMap;

use crate::mode::FaceRegistry;
use crate::presentation::PresentationLayerStore;
use crate::view::View;
use vell_core::content::ContentKind;
use vell_core::content_store::ContentStore;
use vell_core::content_view_state::ContentViewState;
use vell_protocol::content_query::{
    ContentData, ContentQuery, ContentQueryKind, CursorStyle, RenderQuery, RenderQueryError,
    RowRange, SelectionShape, TextDecoration, TextPresentation, ViewData, ViewPresentation,
};
use vell_protocol::ids::{ContentId, ViewId};

pub(super) struct AppQuery<'a> {
    pub(super) contents: &'a ContentStore,
    pub(super) views: &'a HashMap<ViewId, View>,
    pub(super) presentation: &'a PresentationLayerStore,
    pub(super) faces: &'a FaceRegistry,
}

impl RenderQuery for AppQuery<'_> {
    fn content(
        &self,
        cid: ContentId,
        query: ContentQuery,
    ) -> Result<ContentData, RenderQueryError> {
        if !self.contents.contains(cid) {
            return Err(RenderQueryError::MissingContent(cid));
        }
        let query_kind = query.kind();
        match self.contents.query(cid, query) {
            ContentData::Unsupported => Err(RenderQueryError::UnsupportedContentQuery {
                content: cid,
                query: query_kind,
            }),
            data => Ok(data),
        }
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

    fn decorations(
        &self,
        id: ViewId,
        visible_rows: RowRange,
    ) -> Result<Vec<TextDecoration>, RenderQueryError> {
        let view = self
            .views
            .get(&id)
            .ok_or(RenderQueryError::MissingView(id))?;
        let content = view.content();
        let content_kind = self
            .contents
            .kind(content)
            .ok_or(RenderQueryError::MissingContent(content))?;
        match (content_kind, view.state()) {
            (ContentKind::StatusBar, ContentViewState::StatusBar(_)) => return Ok(Vec::new()),
            (ContentKind::Buffer, ContentViewState::Buffer(_)) => {}
            (ContentKind::Buffer, ContentViewState::StatusBar(_))
            | (ContentKind::StatusBar, ContentViewState::Buffer(_)) => {
                return Err(RenderQueryError::IncompatibleContentViewState { view: id, content });
            }
        }
        let content_revision = self
            .contents
            .revision(content)
            .ok_or(RenderQueryError::MissingContent(content))?;
        let snapshot =
            self.contents
                .text_snapshot(content)
                .ok_or(RenderQueryError::InvalidContentData {
                    content,
                    query: ContentQueryKind::TextRows,
                })?;
        Ok(self
            .presentation
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
            .collect())
    }
}
