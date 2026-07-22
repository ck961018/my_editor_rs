use std::collections::HashMap;

use crate::presentation::PresentationLayerStore;
use crate::theme::SessionFaces;
use crate::view::View;
use vell_core::content::ContentKind;
use vell_core::content_store::ContentStore;
use vell_core::content_view_state::ContentViewState;
use vell_protocol::content_query::{
    BufferBackingState, ContentData, ContentQuery, ContentQueryKind, CursorStyle, DirtyState,
    FaceName, FacePatch, RenderQuery, RenderQueryError, RowRange, SaveState, SelectionShape,
    StatusBarPresentation, StatusBarSegment, TextDecoration, TextPresentation, ViewData,
    ViewPresentation,
};
use vell_protocol::ids::{ContentId, ViewId};

pub(super) struct AppQuery<'a> {
    pub(super) contents: &'a ContentStore,
    pub(super) views: &'a HashMap<ViewId, View>,
    pub(super) presentation: &'a PresentationLayerStore,
    pub(super) faces: &'a SessionFaces,
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
                    base_face: self.faces.resolve_root_for(
                        &FaceName::new("ui.editor"),
                        content,
                        id,
                    ),
                    selections: state.selections().clone(),
                    cursor_style: policy.cursor_style.unwrap_or(CursorStyle::Default),
                    selection_shape: policy.selection_shape.unwrap_or(SelectionShape::Character),
                    selection_face: policy
                        .selection_face
                        .as_ref()
                        .map(|face| self.faces.resolve_for(face, content, id))
                        .unwrap_or_else(|| {
                            self.faces
                                .resolve_for(&FaceName::new("ui.selection"), content, id)
                        }),
                })
            }
            (ContentKind::StatusBar, ContentViewState::StatusBar(state)) => {
                let Some((target_view, target_content)) = state.target() else {
                    return Err(RenderQueryError::IncompatibleContentViewState {
                        view: id,
                        content,
                    });
                };
                let content_revision = self
                    .contents
                    .revision(content)
                    .ok_or(RenderQueryError::MissingContent(content))?;
                let policy = self
                    .presentation
                    .policy(id, content_revision, view.revision());
                let presentation = policy.status_bar.as_ref().map_or_else(
                    || {
                        default_status_bar_presentation(
                            target_view,
                            target_content,
                            self.contents,
                            self.views,
                            self.faces,
                            content,
                            id,
                        )
                    },
                    |presentation| StatusBarPresentation {
                        base_face: self.faces.resolve_status_bar_root(target_view, content, id),
                        left: resolve_status_segments(&presentation.left, self.faces, content, id),
                        center: resolve_status_segments(
                            &presentation.center,
                            self.faces,
                            content,
                            id,
                        ),
                        right: resolve_status_segments(
                            &presentation.right,
                            self.faces,
                            content,
                            id,
                        ),
                    },
                );
                ViewPresentation::StatusBar(presentation)
            }
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
                face: self.faces.resolve_for(&decoration.face, content, id),
            })
            .collect())
    }
}

fn resolve_status_segments(
    segments: &[crate::mode::NamedStatusBarSegment],
    faces: &SessionFaces,
    content: ContentId,
    view: ViewId,
) -> Vec<StatusBarSegment> {
    segments
        .iter()
        .map(|segment| StatusBarSegment {
            text: segment.text.clone(),
            face: segment
                .face
                .as_ref()
                .map(|face| faces.resolve_for(face, content, view))
                .unwrap_or_default(),
        })
        .collect()
}

fn default_status_bar_presentation(
    target_view: ViewId,
    target: ContentId,
    contents: &ContentStore,
    views: &HashMap<ViewId, View>,
    faces: &SessionFaces,
    status_content: ContentId,
    status_view: ViewId,
) -> StatusBarPresentation {
    let name = match contents.query(target, ContentQuery::ResourceName) {
        ContentData::ResourceName(name) => name.unwrap_or_else(|| "[No Name]".to_owned()),
        _ => "[No Name]".to_owned(),
    };
    let dirty = matches!(
        contents.query(target, ContentQuery::DirtyState),
        ContentData::DirtyState(DirtyState::Modified)
    );
    let unmaterialized = matches!(
        contents.query(target, ContentQuery::BackingState),
        ContentData::BackingState(BufferBackingState::Unmaterialized)
    );
    let mut left = vec![StatusBarSegment {
        text: name,
        face: FacePatch::default(),
    }];
    if dirty {
        left.push(StatusBarSegment {
            text: " [+]".to_owned(),
            face: FacePatch::default(),
        });
    }
    if unmaterialized {
        left.push(StatusBarSegment {
            text: " [New]".to_owned(),
            face: FacePatch::default(),
        });
    }

    let right = views
        .get(&target_view)
        .and_then(|view| view.state().selections())
        .and_then(|selections| {
            match contents.query(
                target,
                ContentQuery::TextPoints(vec![selections.primary().head()]),
            ) {
                ContentData::TextPoints(points) => points.first().copied(),
                _ => None,
            }
        })
        .map(|point| {
            vec![StatusBarSegment {
                text: format!("{}:{}", point.row + 1, point.col + 1),
                face: FacePatch::default(),
            }]
        })
        .unwrap_or_default();

    let center = match contents.query(target, ContentQuery::SaveState) {
        ContentData::SaveState(SaveState::Saved) => vec![StatusBarSegment {
            text: "Saved".to_owned(),
            face: FacePatch::default(),
        }],
        ContentData::SaveState(SaveState::Failed) => vec![StatusBarSegment {
            text: "Save failed".to_owned(),
            face: FacePatch::default(),
        }],
        _ => Vec::new(),
    };

    StatusBarPresentation {
        base_face: faces.resolve_status_bar_root(target_view, status_content, status_view),
        left,
        center,
        right,
    }
}
