use std::fmt;

use crate::application::App;
use crate::scene_model::{CloseResult, SceneError, SplitResult};
use crate::session::PreparedDiffReplacement;
use crate::view::View;
use vell_core::content_store::ContentStore;
use vell_frontend::Frontend;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::space::{Sizing, SplitDirection};
use vell_protocol::view::ViewDefinition;
use vell_protocol::view::{
    BindingKey, DIFF_VIEW_DEFINITION, DOCUMENT_BINDING, LEFT_BINDING, RIGHT_BINDING,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusBarPlacement {
    #[default]
    Global,
    PerPane,
}

/// 状态栏 Pane 的定位信息：状态栏不再是独立 view，而是某个 editor view
/// 的 STATUS_PANE 直属 Pane。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusBarHandle {
    /// 状态栏 Pane 所在的 Space。
    pub space: SpaceId,
    /// 当前拥有该 Pane 的 editor view。
    pub target_view: ViewId,
    /// target_view 绑定的 content。
    pub content: ContentId,
}

impl<F: Frontend> App<F> {
    pub fn status_bar_placement(&self) -> StatusBarPlacement {
        self.session.status_bar_placement()
    }

    pub fn status_bar_for_view(&self, editor: ViewId) -> Option<StatusBarHandle> {
        self.session.status_bar_for_view(editor)
    }

    pub fn status_bars_for_content(&self, content: ContentId) -> Vec<StatusBarHandle> {
        self.session.status_bars_for_content(content)
    }

    /// 焦点 Pane 所属 view 的通用切换目标（最近 switchable 祖先）。
    pub fn switch_target(&self) -> Option<ViewId> {
        self.session.switch_target(self.session.focused())
    }

    pub fn set_status_bar_placement(
        &mut self,
        placement: StatusBarPlacement,
    ) -> std::io::Result<()> {
        self.session
            .set_status_bar_placement(placement)
            .map_err(std::io::Error::other)?;
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(())
    }

    pub fn set_status_bar_visible(
        &mut self,
        editor: Option<ViewId>,
        visible: bool,
    ) -> std::io::Result<()> {
        self.session
            .set_status_bar_visible(editor, visible)
            .map_err(std::io::Error::other)
    }

    pub(super) fn split_space(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
        direction: SplitDirection,
        focus_new: bool,
    ) -> Result<SplitResult, LayoutError> {
        let inherited_state = self
            .session
            .view_for_space(target)
            .and_then(|view| self.session.view(view))
            .and_then(|view| view.document())
            .filter(|(document, _)| *document == content)
            .map(|(_, state)| state.clone());
        let mut view = create_view(
            content,
            self.kernel.contents(),
            self.kernel.buffer_view_definition(),
        )
        .ok_or(LayoutError::MissingContent(content))?;
        if let Some(state) = inherited_state {
            *view.view.require_document_state_mut() = state;
        }
        let (contents, modes, classifier, content_modes) = self.kernel.attachment_runtime_parts();
        let result = self.session.split_space(
            target,
            view,
            focusable,
            direction,
            focus_new,
            modes,
            classifier,
            content_modes,
            contents,
        )?;
        self.kernel.schedule_mode_jobs();
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(result)
    }

    pub(super) fn close_space(&mut self, target: SpaceId) -> Result<CloseResult, LayoutError> {
        let (contents, content_modes) = self.kernel.mode_runtime_parts();
        let mutation = self.session.close_space(target, content_modes, contents)?;
        for removed in mutation.removed {
            if let Some(content) = removed.document
                && self.kernel.active_transaction_owner(content) == Some(Some(removed.view))
            {
                self.kernel.commit_transaction(content);
            }
            self.cancel_pending_commands_for_view(removed.view);
        }
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(mutation.output)
    }

    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
    ) -> Result<ViewId, LayoutError> {
        let view = create_view(
            content,
            self.kernel.contents(),
            self.kernel.buffer_view_definition(),
        )
        .ok_or(LayoutError::MissingContent(content))?;
        let (contents, modes, classifier, content_modes) = self.kernel.attachment_runtime_parts();
        let mutation = self.session.replace_space_content(
            target,
            view,
            focusable,
            modes,
            classifier,
            content_modes,
            contents,
        )?;
        for removed in mutation.removed {
            self.cancel_pending_commands_for_view(removed.view);
            if let Some(content) = removed.document
                && self.kernel.active_transaction_owner(content) == Some(Some(removed.view))
            {
                self.kernel.commit_transaction(content);
            }
        }
        self.kernel.schedule_mode_jobs();
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(mutation.output)
    }

    pub(super) fn prepare_diff_replacement(
        &mut self,
        target: SpaceId,
        left: ContentId,
        right: ContentId,
    ) -> Result<PreparedDiffReplacement, LayoutError> {
        let left_view = create_view(
            left,
            self.kernel.contents(),
            self.kernel.buffer_view_definition(),
        )
        .ok_or(LayoutError::MissingContent(left))?;
        let right_view = create_view(
            right,
            self.kernel.contents(),
            self.kernel.buffer_view_definition(),
        )
        .ok_or(LayoutError::MissingContent(right))?;
        let parent = View::with_definition(
            self.kernel.diff_view_definition(),
            [
                (BindingKey::new(LEFT_BINDING), left),
                (BindingKey::new(RIGHT_BINDING), right),
            ],
            None,
        )
        .expect("built-in DiffView definition and bindings are valid");
        let (contents, modes, classifier, _) = self.kernel.attachment_runtime_parts();
        self.session.prepare_diff_replacement(
            target, parent, left_view, right_view, modes, classifier, contents,
        )
    }

    pub(super) fn publish_diff_replacement(
        &mut self,
        prepared: PreparedDiffReplacement,
    ) -> crate::view_workspace::DiffViewResult {
        let (contents, modes, _, content_modes) = self.kernel.attachment_runtime_parts();
        let mutation =
            self.session
                .publish_diff_replacement(prepared, modes, content_modes, contents);
        for removed in &mutation.removed {
            self.cancel_pending_commands_for_view(removed.view);
            if let Some(content) = removed.document
                && self.kernel.active_transaction_owner(content) == Some(Some(removed.view))
            {
                self.kernel.commit_transaction(content);
            }
        }
        self.kernel.schedule_mode_jobs();
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        mutation.output
    }

    pub(super) fn switch_view_at(
        &mut self,
        target: ViewId,
        content: ContentId,
    ) -> Result<ViewId, LayoutError> {
        if self.session.view(target).is_some_and(|view| {
            view.document_content() == Some(content) && view.children().is_empty()
        }) {
            return Ok(target);
        }
        let space = self
            .session
            .replacement_space_for_view(target)
            .ok_or(LayoutError::MissingView(target))?;
        self.replace_space_content(space, content, true)
    }

    pub(super) fn rebind_view_content(
        &mut self,
        view: ViewId,
        binding: &BindingKey,
        content: ContentId,
    ) -> Result<ContentId, LayoutError> {
        if self
            .session
            .view(view)
            .is_some_and(|view| view.definition().as_str() == DIFF_VIEW_DEFINITION)
            && binding.as_str() == RIGHT_BINDING
        {
            let (contents, modes, classifier, content_modes) =
                self.kernel.attachment_runtime_parts();
            let (previous, right) = self.session.rebind_diff_right(
                view,
                content,
                modes,
                classifier,
                content_modes,
                contents,
            )?;
            if previous != content {
                if self.kernel.active_transaction_owner(previous) == Some(Some(right)) {
                    self.kernel.commit_transaction(previous);
                }
                self.cancel_pending_commands_for_view(right);
                self.kernel.schedule_mode_jobs();
            }
            self.session
                .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
            return Ok(previous);
        }
        let (contents, modes, classifier, content_modes) = self.kernel.attachment_runtime_parts();
        let previous = self.session.rebind_view_content(
            view,
            binding,
            content,
            modes,
            classifier,
            content_modes,
            contents,
        )?;
        if previous != content && binding.as_str() == DOCUMENT_BINDING {
            if self.kernel.active_transaction_owner(previous) == Some(Some(view)) {
                self.kernel.commit_transaction(previous);
            }
            self.cancel_pending_commands_for_view(view);
            self.kernel.schedule_mode_jobs();
        }
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(previous)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "layout mutation is exposed as an application backend operation"
        )
    )]
    pub(super) fn set_space_sizing(
        &mut self,
        target: SpaceId,
        sizing: Sizing,
    ) -> Result<(), LayoutError> {
        self.session.set_space_sizing(target, sizing)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LayoutError {
    MissingContent(ContentId),
    MissingView(ViewId),
    MissingBinding { view: ViewId, binding: BindingKey },
    ModeAttachment(String),
    WouldRemoveLastFocusable(SpaceId),
    NoFocusableSpace,
    NoStatusBar,
    StatusBarSpace(SpaceId),
    InvalidWorkspace(String),
    Scene(SceneError),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContent(content) => {
                write!(formatter, "content {} does not exist", content.0)
            }
            Self::MissingView(view) => write!(formatter, "view {} does not exist", view.0),
            Self::MissingBinding { view, binding } => {
                write!(formatter, "view {} has no {binding} binding", view.0)
            }
            Self::ModeAttachment(message) => formatter.write_str(message),
            Self::WouldRemoveLastFocusable(space) => {
                write!(formatter, "space {} is the last focusable space", space.0)
            }
            Self::NoFocusableSpace => write!(formatter, "scene has no focusable space"),
            Self::NoStatusBar => write!(formatter, "status bar does not exist"),
            Self::StatusBarSpace(space) => {
                write!(formatter, "space {} is managed by the status bar", space.0)
            }
            Self::InvalidWorkspace(message) => {
                write!(formatter, "ViewWorkspace invariant failed: {message}")
            }
            Self::Scene(error) => write!(formatter, "scene mutation failed: {error:?}"),
        }
    }
}

impl std::error::Error for LayoutError {}

impl From<SceneError> for LayoutError {
    fn from(error: SceneError) -> Self {
        Self::Scene(error)
    }
}

pub(super) fn create_view(
    content: ContentId,
    contents: &ContentStore,
    definition: &ViewDefinition,
) -> Option<NewView> {
    if !contents.contains(content) {
        return None;
    }
    let state = contents
        .create_view_state(content)
        .expect("existing content creates view state");
    Some(NewView {
        view: View::with_definition(
            definition,
            [(BindingKey::new(DOCUMENT_BINDING), content)],
            Some(state),
        )
        .expect("BufferView definition and bindings are valid"),
    })
}

pub(super) struct NewView {
    pub(super) view: View,
}
