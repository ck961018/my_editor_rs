use crate::application::App;
use vell_core::transaction::TextStateId;
use vell_frontend::Frontend;
use vell_protocol::content_query::{ContentData, ContentQuery, DocumentStatus};
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::Selections;
use vell_protocol::viewport::ResolvedViewportCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BehaviorSnapshot {
    pub contents: Vec<ContentBehavior>,
    pub views: Vec<ViewBehavior>,
    pub history: Vec<HistoryBehavior>,
    pub mode_probes: Vec<ModeProbeBehavior>,
    pub prepared_effects: Vec<EffectBehavior>,
    pub published_effects: Vec<EffectBehavior>,
    pub faults: Vec<ModeFaultBehavior>,
    pub outcome: ExecutionOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ContentBehavior {
    pub content: ContentId,
    pub revision: Revision,
    pub text: Option<String>,
    pub document_status: Option<DocumentStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ViewBehavior {
    pub view: ViewId,
    pub content: ContentId,
    pub revision: Revision,
    pub selections: Option<Selections>,
    pub modes: Vec<String>,
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HistoryBehavior {
    pub content: ContentId,
    pub active: bool,
    pub owner: Option<ViewId>,
    pub undo_depth: usize,
    pub redo_depth: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ModeProbeBehavior {
    pub name: String,
    pub value: String,
}

impl ModeProbeBehavior {
    pub(super) fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EffectBehavior {
    HistoryCommit {
        content: ContentId,
    },
    Save {
        content: ContentId,
        bytes: String,
        revision: u64,
        state: TextStateId,
    },
    Viewport {
        view: ViewId,
        command: ResolvedViewportCommand,
    },
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ModeFaultBehavior {
    pub mode: String,
    pub scope: ModeFaultScope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ModeFaultScope {
    Content(ContentId),
    View(ViewId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ExecutionOutcome {
    Succeeded,
    Failed(String),
}

impl ExecutionOutcome {
    pub(super) fn from_result<T>(result: &std::io::Result<T>) -> Self {
        match result {
            Ok(_) => Self::Succeeded,
            Err(error) => Self::Failed(error.to_string()),
        }
    }
}

#[derive(Default)]
pub(super) struct BehaviorRecorder {
    prepared_effects: Vec<EffectBehavior>,
    published_effects: Vec<EffectBehavior>,
}

impl BehaviorRecorder {
    pub(super) fn reset(&mut self) {
        self.prepared_effects.clear();
        self.published_effects.clear();
    }

    pub(super) fn record_prepared(&mut self, effect: EffectBehavior) {
        self.prepared_effects.push(effect);
    }

    pub(super) fn record_published(&mut self, effect: EffectBehavior) {
        self.published_effects.push(effect);
    }
}

impl BehaviorSnapshot {
    pub(super) fn capture<F: Frontend>(
        app: &App<F>,
        outcome: ExecutionOutcome,
        mut mode_probes: Vec<ModeProbeBehavior>,
    ) -> Self {
        let mut content_ids: Vec<_> = app.kernel.contents().ids().collect();
        content_ids.sort_by_key(|content| content.0);

        let contents = content_ids
            .iter()
            .copied()
            .map(|content| {
                let text = app
                    .kernel
                    .contents()
                    .text_snapshot(content)
                    .map(|snapshot| snapshot.to_owned_string());
                let document_status = match app
                    .kernel
                    .contents()
                    .query(content, ContentQuery::DocumentStatus)
                {
                    ContentData::DocumentStatus(status) => Some(status),
                    _ => None,
                };
                ContentBehavior {
                    content,
                    revision: app
                        .kernel
                        .contents()
                        .revision(content)
                        .expect("captured content exists"),
                    text,
                    document_status,
                }
            })
            .collect();

        let focused_view = app.session.view_for_space(app.session.focused());
        let mut views: Vec<_> = app
            .session
            .views()
            .iter()
            .map(|(view, state)| ViewBehavior {
                view: *view,
                content: state.content(),
                revision: state.revision(),
                selections: state.selections().cloned(),
                modes: app
                    .session
                    .view_modes()
                    .mode_names(*view)
                    .into_iter()
                    .map(|mode| mode.as_str().to_owned())
                    .collect(),
                focused: Some(*view) == focused_view,
            })
            .collect();
        views.sort_by_key(|view| view.view.0);

        let history = content_ids
            .iter()
            .copied()
            .map(|content| {
                let (active, owner, undo_depth, redo_depth) =
                    app.kernel.history_behavior_for_test(content);
                HistoryBehavior {
                    content,
                    active,
                    owner,
                    undo_depth,
                    redo_depth,
                }
            })
            .collect();

        mode_probes.sort_by(|left, right| left.name.cmp(&right.name));
        let mut faults: Vec<_> =
            app.kernel
                .content_modes()
                .faults_for_test()
                .into_iter()
                .map(|(mode, content)| ModeFaultBehavior {
                    mode,
                    scope: ModeFaultScope::Content(content),
                })
                .chain(app.session.view_modes().faults_for_test().into_iter().map(
                    |(mode, view)| ModeFaultBehavior {
                        mode,
                        scope: ModeFaultScope::View(view),
                    },
                ))
                .collect();
        faults.sort_by(|left, right| {
            left.mode
                .cmp(&right.mode)
                .then_with(|| fault_scope_key(&left.scope).cmp(&fault_scope_key(&right.scope)))
        });

        Self {
            contents,
            views,
            history,
            mode_probes,
            prepared_effects: app.behavior.prepared_effects.clone(),
            published_effects: app.behavior.published_effects.clone(),
            faults,
            outcome,
        }
    }
}

fn fault_scope_key(scope: &ModeFaultScope) -> (u8, u64) {
    match scope {
        ModeFaultScope::Content(content) => (0, content.0),
        ModeFaultScope::View(view) => (1, view.0),
    }
}
