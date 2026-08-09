use std::collections::HashMap;

use crate::presentation::PresentationLayerStore;
use crate::theme::SessionFaces;
use crate::view::{BODY_PANE, STATUS_PANE, View};
use vell_core::content_store::ContentStore;
use vell_core::content_view_state::ContentViewState;
use vell_protocol::content_query::{
    BufferBackingState, ContentData, ContentQuery, ContentQueryKind, CursorStyle,
    DEFAULT_TAB_WIDTH, DirtyState, FaceName, FacePatch, MAX_TAB_WIDTH, RenderQuery,
    RenderQueryError, RowRange, SaveState, SelectionShape, StatusBarPresentation, StatusBarSegment,
    TextDecoration, TextPresentation, ViewData, ViewPresentation,
};
use vell_protocol::ids::{ContentId, SpaceId, ViewId};

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

    fn view(&self, id: ViewId, space: SpaceId) -> Result<ViewData, RenderQueryError> {
        let view = self
            .views
            .get(&id)
            .ok_or(RenderQueryError::MissingView(id))?;
        let content = view.content();
        let _content_kind = self
            .contents
            .kind(content)
            .ok_or(RenderQueryError::MissingContent(content))?;
        // view 根据来源 Pane 决定该 Space 的显示内容。
        match view.panes().key_for_space(space) {
            Some(BODY_PANE) => self.body_pane_view(id, content, view),
            Some(STATUS_PANE) => self.status_pane_view(id, content, view),
            _ => Err(RenderQueryError::UnmappedSpace { view: id, space }),
        }
    }

    fn decorations(
        &self,
        id: ViewId,
        space: SpaceId,
        visible_rows: RowRange,
    ) -> Result<Vec<TextDecoration>, RenderQueryError> {
        let view = self
            .views
            .get(&id)
            .ok_or(RenderQueryError::MissingView(id))?;
        // 文本 decoration 只属于正文 Pane。
        if view.panes().key_for_space(space) != Some(BODY_PANE) {
            return Ok(Vec::new());
        }
        let content = view.content();
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

impl AppQuery<'_> {
    fn body_pane_view(
        &self,
        id: ViewId,
        content: ContentId,
        view: &View,
    ) -> Result<ViewData, RenderQueryError> {
        let ContentViewState::Buffer(state) = view.state();
        let presentation = {
            let content_revision = self
                .contents
                .revision(content)
                .ok_or(RenderQueryError::MissingContent(content))?;
            let policy = self
                .presentation
                .policy(id, content_revision, view.revision());
            ViewPresentation::Text(TextPresentation {
                base_face: self
                    .faces
                    .resolve_root_for(&FaceName::new("ui.editor"), content, id),
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
                tab_width: policy
                    .tab_width
                    .unwrap_or(DEFAULT_TAB_WIDTH)
                    .clamp(1, MAX_TAB_WIDTH),
            })
        };
        Ok(ViewData {
            content,
            presentation,
        })
    }

    /// 状态栏 Pane 直接读取本 view 的数据、状态与 Mode policy，不再经由
    /// 独立状态栏 view 间接查询。
    fn status_pane_view(
        &self,
        id: ViewId,
        content: ContentId,
        view: &View,
    ) -> Result<ViewData, RenderQueryError> {
        let content_revision = self
            .contents
            .revision(content)
            .ok_or(RenderQueryError::MissingContent(content))?;
        let policy = self
            .presentation
            .policy(id, content_revision, view.revision());
        let mut presentation = policy.status_bar.as_ref().map_or_else(
            || default_status_bar_presentation(id, content, self.contents, self.views, self.faces),
            |presentation| StatusBarPresentation {
                base_face: self.faces.resolve_status_bar_root(id, content),
                left: resolve_status_segments(&presentation.left, self.faces, content, id),
                center: resolve_status_segments(&presentation.center, self.faces, content, id),
                right: resolve_status_segments(&presentation.right, self.faces, content, id),
            },
        );
        if let Some(message) = self.presentation.status_message() {
            presentation.center = vec![StatusBarSegment {
                text: message.to_owned(),
                face: FacePatch::default(),
            }];
        }
        Ok(ViewData {
            content,
            presentation: ViewPresentation::StatusBar(presentation),
        })
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
    view: ViewId,
    content: ContentId,
    contents: &ContentStore,
    views: &HashMap<ViewId, View>,
    faces: &SessionFaces,
) -> StatusBarPresentation {
    let name = match contents.query(content, ContentQuery::ResourceName) {
        ContentData::ResourceName(name) => name.unwrap_or_else(|| "[No Name]".to_owned()),
        _ => "[No Name]".to_owned(),
    };
    let dirty = matches!(
        contents.query(content, ContentQuery::DirtyState),
        ContentData::DirtyState(DirtyState::Modified)
    );
    let unmaterialized = matches!(
        contents.query(content, ContentQuery::BackingState),
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
        .get(&view)
        .and_then(|view| view.state().selections())
        .and_then(|selections| {
            match contents.query(
                content,
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

    let center = match contents.query(content, ContentQuery::SaveState) {
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
        base_face: faces.resolve_status_bar_root(view, content),
        left,
        center,
        right,
    }
}
