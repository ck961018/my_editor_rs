#![allow(dead_code)] // Semantic wire contract; transport integration is intentionally deferred.

use crate::protocol::content_query::{ContentData, ContentQuery, ViewData};
use crate::protocol::ids::{ContentId, ViewId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);

impl Revision {
    pub fn next(&mut self) {
        self.0 = self.0.checked_add(1).expect("revision overflow");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

pub const CURRENT_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    ViewQuery,
    ContentQuery,
    ChangeNotifications,
    Revisions,
}

pub const SERVER_CAPABILITIES: &[Capability] = &[
    Capability::ViewQuery,
    Capability::ContentQuery,
    Capability::ChangeNotifications,
    Capability::Revisions,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub version: ProtocolVersion,
    pub capabilities: Vec<Capability>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Welcome {
    pub version: ProtocolVersion,
    pub capabilities: Vec<Capability>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolErrorCode {
    IncompatibleVersion,
    MissingCapability,
    UnknownView,
    UnknownContent,
    UnsupportedQuery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolError {
    pub code: ProtocolErrorCode,
    pub message: String,
}

impl ProtocolError {
    pub fn new(code: ProtocolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub fn negotiate(hello: &Hello) -> Result<Welcome, ProtocolError> {
    if hello.version.major != CURRENT_VERSION.major {
        return Err(ProtocolError::new(
            ProtocolErrorCode::IncompatibleVersion,
            format!(
                "unsupported protocol major {}; server uses {}",
                hello.version.major, CURRENT_VERSION.major
            ),
        ));
    }

    Ok(Welcome {
        version: compatible_version(hello.version, CURRENT_VERSION),
        capabilities: SERVER_CAPABILITIES
            .iter()
            .copied()
            .filter(|capability| hello.capabilities.contains(capability))
            .collect(),
    })
}

fn compatible_version(client: ProtocolVersion, server: ProtocolVersion) -> ProtocolVersion {
    ProtocolVersion {
        major: server.major,
        minor: client.minor.min(server.minor),
    }
}

pub fn require_capability(welcome: &Welcome, capability: Capability) -> Result<(), ProtocolError> {
    if welcome.capabilities.contains(&capability) {
        Ok(())
    } else {
        Err(ProtocolError::new(
            ProtocolErrorCode::MissingCapability,
            format!("capability {capability:?} was not negotiated"),
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestData {
    View {
        view: ViewId,
    },
    Content {
        content: ContentId,
        query: ContentQuery,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: RequestId,
    pub data: RequestData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponseData {
    View {
        view: ViewId,
        revision: Revision,
        data: ViewData,
    },
    Content {
        content: ContentId,
        revision: Revision,
        data: ContentData,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub id: RequestId,
    pub result: Result<ResponseData, ProtocolError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notification {
    SceneChanged {
        revision: Revision,
    },
    ViewChanged {
        view: ViewId,
        revision: Revision,
    },
    ContentInvalidated {
        content: ContentId,
        revision: Revision,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    Hello(Hello),
    Request(Request),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerMessage {
    Welcome(Welcome),
    Response(Response),
    Notification(Notification),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_returns_only_the_capability_intersection() {
        let welcome = negotiate(&Hello {
            version: ProtocolVersion { major: 1, minor: 9 },
            capabilities: vec![Capability::ViewQuery, Capability::Revisions],
        })
        .unwrap();

        assert_eq!(welcome.version, CURRENT_VERSION);
        assert_eq!(
            welcome.capabilities,
            vec![Capability::ViewQuery, Capability::Revisions]
        );
    }

    #[test]
    fn negotiation_rejects_an_incompatible_major_version() {
        let error = negotiate(&Hello {
            version: ProtocolVersion { major: 2, minor: 0 },
            capabilities: vec![],
        })
        .unwrap_err();

        assert_eq!(error.code, ProtocolErrorCode::IncompatibleVersion);
    }

    #[test]
    fn unnegotiated_capability_is_an_explicit_error() {
        let welcome = Welcome {
            version: CURRENT_VERSION,
            capabilities: vec![Capability::ViewQuery],
        };

        let error = require_capability(&welcome, Capability::ContentQuery).unwrap_err();

        assert_eq!(error.code, ProtocolErrorCode::MissingCapability);
    }

    #[test]
    fn revision_is_monotonic() {
        let mut revision = Revision::default();
        revision.next();
        revision.next();
        assert_eq!(revision, Revision(2));
    }

    #[test]
    fn messages_own_their_payloads() {
        let message = ClientMessage::Request(Request {
            id: RequestId(7),
            data: RequestData::Content {
                content: ContentId(3),
                query: ContentQuery::StatusBarData,
            },
        });

        assert!(matches!(
            message,
            ClientMessage::Request(Request {
                id: RequestId(7),
                ..
            })
        ));
    }
}
