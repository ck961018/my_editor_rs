//! App-owned adapter from a View's document binding to the Mode contract.

use crate::mode::{ModeContextError, ModeViewContext};
use crate::view::View;
use vell_core::content_store::ContentStore;
use vell_protocol::ids::ViewId;

pub(crate) fn require_mode_view_context<'a>(
    view_id: ViewId,
    view: &'a View,
    contents: &'a ContentStore,
) -> Result<ModeViewContext<'a>, ModeContextError> {
    let (content, state) = view
        .document()
        .ok_or(ModeContextError::MissingDocument { view: view_id })?;
    ModeViewContext::new(view_id, content, state, contents)
}
