#![allow(dead_code)] // The local adapter is exercised by tests until a remote frontend is wired.

use crate::protocol::content_query::{ContentData, RenderQuery};
use crate::protocol::remote::{
    ProtocolError, ProtocolErrorCode, Request, RequestData, Response, ResponseData,
};

use super::query::AppQuery;

pub(super) fn respond(query: &AppQuery<'_>, request: Request) -> Response {
    let result = match request.data {
        RequestData::View { view } => match query.views.get(&view) {
            Some(local_view) => Ok(ResponseData::View {
                view,
                revision: local_view.revision(),
                data: query.view(view),
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
            Some(revision) => match query.content(content, content_query) {
                ContentData::Unsupported => Err(ProtocolError::new(
                    ProtocolErrorCode::UnsupportedQuery,
                    format!("content {} does not support the query", content.0),
                )),
                data => Ok(ResponseData::Content {
                    content,
                    revision,
                    data,
                }),
            },
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::app::view::View;
    use crate::core::buffer::Buffer;
    use crate::core::content::Content;
    use crate::core::content_store::ContentStore;
    use crate::protocol::content_query::{ContentQuery, RowRange, ViewPresentation};
    use crate::protocol::ids::{ContentId, ViewId};
    use crate::protocol::remote::RequestId;
    use crate::protocol::revision::Revision;

    struct Fixture {
        contents: ContentStore,
        views: HashMap<ViewId, View>,
        view_modes: crate::app::mode::ModeViewStore,
        mode_contents: crate::app::mode::ModeContentStore,
        faces: crate::app::mode::FaceRegistry,
    }

    impl Fixture {
        fn new() -> Self {
            let content = ContentId(0);
            let mut contents = ContentStore::default();
            contents
                .insert(content, Content::Buffer(Buffer::new()))
                .unwrap();
            let view = View::new(content, contents.create_view_state(content).unwrap());
            Self {
                contents,
                views: HashMap::from([(ViewId(0), view)]),
                view_modes: crate::app::mode::ModeViewStore::default(),
                mode_contents: crate::app::mode::ModeContentStore::default(),
                faces: crate::app::mode::FaceRegistry::default(),
            }
        }

        fn query(&self) -> AppQuery<'_> {
            AppQuery {
                contents: &self.contents,
                views: &self.views,
                view_modes: &self.view_modes,
                mode_contents: &self.mode_contents,
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
                data: crate::protocol::content_query::ViewData {
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
                    content: ContentId(0),
                    query: ContentQuery::StatusBarData,
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
