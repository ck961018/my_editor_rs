use crate::application::App;
use crate::mode::{FaceConflict, ModeViewPolicy};
use crate::mode_name::ModeName;
use crate::presentation::PolicySources;
use vell_frontend::Frontend;
use vell_protocol::content_query::FaceName;
use vell_protocol::ids::{ContentId, ViewId};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamedPolicySources {
    pub cursor_style: Option<ModeName>,
    pub cursor_domain: Option<ModeName>,
    pub selection_shape: Option<ModeName>,
    pub selection_face: Option<ModeName>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeDecorationDiagnostics {
    pub mode: ModeName,
    pub content_count: usize,
    pub view_count: usize,
    pub faulted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewModeDiagnostics {
    pub view: ViewId,
    pub content: ContentId,
    pub modes: Vec<ModeName>,
    pub policy: ModeViewPolicy,
    pub policy_sources: NamedPolicySources,
    pub decorations: Vec<ModeDecorationDiagnostics>,
}

impl<F: Frontend> App<F> {
    pub fn mode_diagnostics(&self) -> Vec<ViewModeDiagnostics> {
        let registry = self.kernel.modes();
        let content_modes = self.kernel.content_modes();
        let view_modes = self.session.view_modes();
        let mut diagnostics = self
            .session
            .views()
            .iter()
            .filter_map(|(&view, view_data)| {
                let content = view_data.content();
                let content_revision = self.kernel.contents().revision(content)?;
                let raw = self.session.presentation().diagnostics(
                    view,
                    content_revision,
                    view_data.revision(),
                )?;
                let mode_name = |mode| {
                    registry
                        .mode_name(mode)
                        .expect("presentation references a registered mode")
                        .clone()
                };
                let policy_sources = named_policy_sources(&raw.policy_sources, &mode_name);
                let modes = raw.modes.into_iter().map(&mode_name).collect();
                let decorations = raw
                    .decorations
                    .into_iter()
                    .map(|layer| ModeDecorationDiagnostics {
                        mode: mode_name(layer.mode),
                        content_count: layer.content_count,
                        view_count: layer.view_count,
                        faulted: content_modes.is_faulted(layer.mode, content)
                            || view_modes.is_faulted(layer.mode, view),
                    })
                    .collect();
                Some(ViewModeDiagnostics {
                    view,
                    content,
                    modes,
                    policy: raw.policy,
                    policy_sources,
                    decorations,
                })
            })
            .collect::<Vec<_>>();
        diagnostics.sort_by_key(|entry| entry.view.0);
        diagnostics
    }

    pub fn face_provider(&self, face: &FaceName) -> Option<&ModeName> {
        self.session.faces().provider(face)
    }

    pub fn face_conflicts(&self) -> &[FaceConflict] {
        self.session.faces().conflicts()
    }
}

fn named_policy_sources(
    sources: &PolicySources,
    name: &impl Fn(crate::mode::ModeId) -> ModeName,
) -> NamedPolicySources {
    NamedPolicySources {
        cursor_style: sources.cursor_style.map(name),
        cursor_domain: sources.cursor_domain.map(name),
        selection_shape: sources.selection_shape.map(name),
        selection_face: sources.selection_face.map(name),
    }
}
