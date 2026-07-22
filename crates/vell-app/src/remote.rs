#![allow(dead_code)] // The local adapter is exercised by tests until a remote frontend is wired.

use vell_protocol::content_query::{RenderQuery, RenderQueryError};
use vell_protocol::remote::{
    ProtocolError, ProtocolErrorCode, Request, RequestData, Response, ResponseData,
};

use super::query::AppQuery;

pub(super) fn respond(query: &AppQuery<'_>, request: Request) -> Response {
    let result = match request.data {
        RequestData::View { view } => match query.views.get(&view) {
            Some(local_view) => query
                .view(view)
                .map(|data| ResponseData::View {
                    view,
                    revision: local_view.revision(),
                    data,
                })
                .map_err(|error| {
                    ProtocolError::new(ProtocolErrorCode::InvalidViewState, error.to_string())
                }),
            None => Err(ProtocolError::new(
                ProtocolErrorCode::UnknownView,
                format!("unknown view {}", view.0),
            )),
        },
        RequestData::Content {
            content,
            query: content_query,
        } => match query.contents.revision(content) {
            Some(revision) => query
                .content(content, content_query)
                .map(|data| ResponseData::Content {
                    content,
                    revision,
                    data,
                })
                .map_err(content_query_protocol_error),
            None => Err(ProtocolError::new(
                ProtocolErrorCode::UnknownContent,
                format!("unknown content {}", content.0),
            )),
        },
    };

    Response {
        id: request.id,
        result,
    }
}

fn content_query_protocol_error(error: RenderQueryError) -> ProtocolError {
    let code = match error {
        RenderQueryError::MissingContent(_) => ProtocolErrorCode::UnknownContent,
        RenderQueryError::UnsupportedContentQuery { .. } => ProtocolErrorCode::UnsupportedQuery,
        RenderQueryError::InvalidContentData { .. }
        | RenderQueryError::MissingView(_)
        | RenderQueryError::IncompatibleContentViewState { .. } => {
            ProtocolErrorCode::InvalidContentData
        }
    };
    ProtocolError::new(code, error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::view::View;
    use vell_core::buffer::Buffer;
    use vell_core::content::Content;
    use vell_core::content_store::ContentStore;
    use vell_core::status_bar::StatusBar;
    use vell_protocol::content_query::{ContentData, ContentQuery, RowRange, ViewPresentation};
    use vell_protocol::ids::{ContentId, ViewId};
    use vell_protocol::remote::RequestId;
    use vell_protocol::revision::Revision;

    struct Fixture {
        contents: ContentStore,
        views: HashMap<ViewId, View>,
        presentation: crate::presentation::PresentationLayerStore,
        faces: crate::theme::SessionFaces,
    }

    impl Fixture {
        fn new() -> Self {
            let content = ContentId(0);
            let mut contents = ContentStore::default();
            contents
                .insert(content, Content::Buffer(Buffer::new()))
                .unwrap();
            contents
                .insert(ContentId(1), Content::StatusBar(StatusBar::new()))
                .unwrap();
            let view = View::new(content, contents.create_view_state(content).unwrap());
            Self {
                contents,
                views: HashMap::from([(ViewId(0), view)]),
                presentation: crate::presentation::PresentationLayerStore::default(),
                faces: crate::theme::SessionFaces::default(),
            }
        }

        fn query(&self) -> AppQuery<'_> {
            AppQuery {
                contents: &self.contents,
                views: &self.views,
                presentation: &self.presentation,
                faces: &self.faces,
            }
        }
    }

    #[test]
    fn view_response_preserves_request_id_and_revision() {
        let fixture = Fixture::new();
        let response = respond(
            &fixture.query(),
            Request {
                id: RequestId(9),
                data: RequestData::View { view: ViewId(0) },
            },
        );

        assert_eq!(response.id, RequestId(9));
        assert!(matches!(
            response.result,
            Ok(ResponseData::View {
                view: ViewId(0),
                revision: Revision(0),
                data: vell_protocol::content_query::ViewData {
                    presentation: ViewPresentation::Text(_),
                    ..
                },
            })
        ));
    }

    #[test]
    fn content_response_is_owned_and_revisioned() {
        let fixture = Fixture::new();
        let response = respond(
            &fixture.query(),
            Request {
                id: RequestId(2),
                data: RequestData::Content {
                    content: ContentId(0),
                    query: ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
                },
            },
        );

        assert!(matches!(
            response.result,
            Ok(ResponseData::Content {
                content: ContentId(0),
                revision: Revision(0),
                data: ContentData::TextRows(_),
            })
        ));
    }

    #[test]
    fn unknown_ids_and_unsupported_queries_are_explicit_errors() {
        let fixture = Fixture::new();
        let unknown = respond(
            &fixture.query(),
            Request {
                id: RequestId(1),
                data: RequestData::View { view: ViewId(99) },
            },
        );
        let unsupported = respond(
            &fixture.query(),
            Request {
                id: RequestId(2),
                data: RequestData::Content {
                    content: ContentId(1),
                    query: ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
                },
            },
        );

        assert_eq!(
            unknown.result.unwrap_err().code,
            ProtocolErrorCode::UnknownView
        );
        assert_eq!(
            unsupported.result.unwrap_err().code,
            ProtocolErrorCode::UnsupportedQuery
        );
    }
}
