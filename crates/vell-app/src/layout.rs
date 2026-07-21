use crate::application::App;
use crate::scene_model::{CloseResult, SceneError, SplitResult};
use crate::view::View;
use vell_core::content_store::ContentStore;
use vell_frontend::Frontend;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::scene::Scene;
use vell_protocol::space::{Sizing, SpaceKind, SplitDirection};

impl<F: Frontend> App<F> {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "layout mutation is exposed as an application backend operation"
        )
    )]
    pub(super) fn split_space(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
        direction: SplitDirection,
        focus_new: bool,
    ) -> Result<SplitResult, LayoutError> {
        let mode_names = self.session.mode_chain_for_new_view(content);
        let view = create_view(content, self.kernel.contents(), &mode_names)
            .ok_or(LayoutError::MissingContent(content))?;
        let (contents, modes, content_modes) = self.kernel.mode_attachment_parts();
        let result = self.session.split_space(
            target,
            view,
            focusable,
            direction,
            focus_new,
            modes,
            content_modes,
            contents,
        )?;
        self.kernel.schedule_mode_jobs();
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(result)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "layout mutation is exposed as an application backend operation"
        )
    )]
    pub(super) fn close_space(&mut self, target: SpaceId) -> Result<CloseResult, LayoutError> {
        let removed = self
            .session
            .view_for_space(target)
            .and_then(|view| self.session.view(view).map(|data| (view, data.content())));
        let (contents, content_modes) = self.kernel.mode_runtime_parts();
        let result = self.session.close_space(target, content_modes, contents)?;
        if let Some((view, content)) = removed
            && self.kernel.active_transaction_owner(content) == Some(Some(view))
        {
            self.kernel.commit_transaction(content);
        }
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(result)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "layout mutation is exposed as an application backend operation"
        )
    )]
    pub(super) fn replace_space_content(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
    ) -> Result<(), LayoutError> {
        let mode_names = self.session.mode_chain_for_new_view(content);
        let view = create_view(content, self.kernel.contents(), &mode_names)
            .ok_or(LayoutError::MissingContent(content))?;
        let removed = self
            .session
            .view_for_space(target)
            .and_then(|view| self.session.view(view).map(|data| (view, data.content())));
        let (contents, modes, content_modes) = self.kernel.mode_attachment_parts();
        self.session.replace_space_content(
            target,
            view,
            focusable,
            modes,
            content_modes,
            contents,
        )?;
        self.kernel.schedule_mode_jobs();
        if let Some((view, content)) = removed
            && self.kernel.active_transaction_owner(content) == Some(Some(view))
        {
            self.kernel.commit_transaction(content);
        }
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(())
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
    WouldRemoveLastFocusable(SpaceId),
    NoFocusableSpace,
    Scene(SceneError),
}

impl From<SceneError> for LayoutError {
    fn from(error: SceneError) -> Self {
        Self::Scene(error)
    }
}

pub(super) fn create_view(
    content: ContentId,
    contents: &ContentStore,
    mode_names: &[crate::mode_name::ModeName],
) -> Option<NewView> {
    if !contents.contains(content) {
        return None;
    }
    let state = contents
        .create_view_state(content)
        .expect("existing content creates view state");
    Some(NewView {
        view: View::new(content, state),
        mode_names: mode_names.to_vec(),
    })
}

pub(super) struct NewView {
    pub(super) view: View,
    pub(super) mode_names: Vec<crate::mode_name::ModeName>,
}

fn collect_view_spaces(scene: &Scene, sid: SpaceId, out: &mut Vec<(SpaceId, ViewId)>) {
    let node = scene.node(sid);
    match &node.space.kind {
        SpaceKind::Content { view, .. } => {
            out.push((sid, *view));
        }
        SpaceKind::Container { .. } => {
            for child in &node.children {
                collect_view_spaces(scene, *child, out);
            }
        }
    }
}

pub(super) fn scene_views(scene: &Scene) -> Vec<(SpaceId, ViewId)> {
    let mut views = Vec::new();
    collect_view_spaces(scene, scene.root(), &mut views);
    views
}

pub(super) fn view_for_space(scene: &Scene, space: SpaceId) -> Option<ViewId> {
    if !scene.contains(space) {
        return None;
    }
    match &scene.node(space).space.kind {
        SpaceKind::Content { view, .. } => Some(*view),
        SpaceKind::Container { .. } => None,
    }
}

pub(super) fn view_space_focusable(scene: &Scene, space: SpaceId) -> Option<bool> {
    if !scene.contains(space) {
        return None;
    }
    match &scene.node(space).space.kind {
        SpaceKind::Content { focusable, .. } => Some(*focusable),
        SpaceKind::Container { .. } => None,
    }
}

pub(super) fn focusable_view_count(scene: &Scene) -> usize {
    scene_views(scene)
        .into_iter()
        .filter(|(space, _)| view_space_focusable(scene, *space) == Some(true))
        .count()
}

pub(super) fn resolve_focus(
    scene: &Scene,
    previous: SpaceId,
    preferred: Option<SpaceId>,
) -> Option<SpaceId> {
    preferred
        .filter(|space| view_space_focusable(scene, *space) == Some(true))
        .or_else(|| (view_space_focusable(scene, previous) == Some(true)).then_some(previous))
        .or_else(|| {
            scene_views(scene)
                .into_iter()
                .map(|(space, _)| space)
                .find(|space| view_space_focusable(scene, *space) == Some(true))
        })
}
