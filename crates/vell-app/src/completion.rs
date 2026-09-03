use std::collections::HashMap;
use std::sync::Arc;

use crate::application::App;
use crate::execution::PreparedCompletionAction;
use crate::message::{AppMessage, CompletionSourceTaskOutcome};
use crate::mode::{CompletionSourceId, ModeId};
use vell_completion::{
    CompletionBatch, CompletionEffect, CompletionEvent, CompletionRequest,
    CompletionRequestContext, CompletionRequestSeed, CompletionSourceKey, CompletionTaskKey,
    CompletionTextRange, CompletionTrigger, SelectionMove, SourceRequestKey,
};
use vell_core::content::ContentKind;
use vell_frontend::Frontend;
use vell_protocol::content_query::{
    CompletionCandidateIdentity, CompletionPresentation, CompletionRow, CompletionSelection,
    CompletionStatus, ContentData, ContentQuery,
};
use vell_protocol::ids::ViewId;
use vell_protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
use vell_protocol::selection::TextOffset;

const MAX_COMPLETION_DIAGNOSTICS: usize = 256;
const MAX_COMPLETION_DIAGNOSTIC_BYTES: usize = 4 * 1024;
// Four UTF-8 bytes per scalar keeps the query within the engine's 16 KiB
// default while bounding synchronous backward scanning on the input path.
const MAX_COMPLETION_QUERY_CHARS: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionDiagnosticKind {
    Scheduled,
    BatchInstalled,
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    StaleRejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKeyAction {
    Next,
    Previous,
    Accept,
    Cancel,
}

impl CompletionKeyAction {
    pub(super) fn operation(self) -> crate::operation::CompletionOperation {
        match self {
            Self::Next => crate::operation::CompletionOperation::Next,
            Self::Previous => crate::operation::CompletionOperation::Previous,
            Self::Accept => crate::operation::CompletionOperation::Accept,
            Self::Cancel => crate::operation::CompletionOperation::Cancel,
        }
    }
}

pub(super) fn default_completion_keymap() -> HashMap<KeyEvent, CompletionKeyAction> {
    HashMap::from([
        (KeyEvent::ctrl('n'), CompletionKeyAction::Next),
        (KeyEvent::arrow(ArrowKey::Down), CompletionKeyAction::Next),
        (KeyEvent::ctrl('p'), CompletionKeyAction::Previous),
        (KeyEvent::arrow(ArrowKey::Up), CompletionKeyAction::Previous),
        (KeyEvent::plain(KeyCode::Enter), CompletionKeyAction::Accept),
        (KeyEvent::plain(KeyCode::Tab), CompletionKeyAction::Accept),
    ])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionDiagnostic {
    pub view: ViewId,
    pub task: CompletionTaskKey,
    pub source: CompletionSourceKey,
    pub kind: CompletionDiagnosticKind,
    pub detail: Option<String>,
}

#[derive(Clone)]
struct ActiveCompletionSource {
    key: CompletionSourceKey,
    mode: ModeId,
    local_id: CompletionSourceId,
}

impl<F: Frontend> App<F> {
    pub fn bind_completion_key(
        &mut self,
        key: KeyEvent,
        action: Option<CompletionKeyAction>,
    ) -> bool {
        self.session.bind_completion_key(key, action)
    }

    pub fn completion_diagnostics(&self) -> impl Iterator<Item = &CompletionDiagnostic> {
        self.completion_diagnostics.iter()
    }

    #[cfg(test)]
    pub(crate) fn completion_source_label_count(&self, view: ViewId) -> usize {
        self.session.completion_source_label_count(view)
    }

    pub(super) fn trigger_completion(&mut self, request: CompletionRequestSeed) -> bool {
        let view = request.view();
        let Some(sources) = self.active_completion_sources(view) else {
            return false;
        };
        let keys = sources
            .iter()
            .map(|source| source.key.clone())
            .collect::<Vec<_>>();
        if keys.is_empty() || !self.completion_seed_target_is_current(&request) {
            return false;
        }
        let effects = self
            .session
            .completion_mut()
            .transition(CompletionEvent::Trigger {
                request,
                sources: keys,
            });
        let active_sources = effects
            .iter()
            .filter_map(|effect| match effect {
                CompletionEffect::RequestSource { key, .. } => sources
                    .iter()
                    .find(|source| &source.key == key.source())
                    .cloned()
                    .map(|source| (source.key.clone(), source)),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        if !active_sources.is_empty() {
            self.session.set_completion_source_labels(
                view,
                active_sources
                    .iter()
                    .map(|(key, source)| (key.clone(), source.local_id.as_str().to_owned())),
            );
        }
        self.apply_completion_effects(effects, &active_sources)
    }

    pub(super) fn trigger_completion_for_view(
        &mut self,
        view: ViewId,
        trigger: CompletionTrigger,
    ) -> bool {
        let Some(seed) = self.completion_request_for_view(view, trigger) else {
            return self.cancel_completion_view(view);
        };
        self.trigger_completion(seed)
    }

    pub(super) fn publish_completion_action(
        &mut self,
        view: ViewId,
        action: PreparedCompletionAction,
    ) -> bool {
        match action {
            PreparedCompletionAction::ManualTrigger => {
                self.trigger_completion_for_view(view, CompletionTrigger::Manual)
            }
            PreparedCompletionAction::Next => {
                let effects =
                    self.session
                        .completion_mut()
                        .transition(CompletionEvent::MoveSelection {
                            view,
                            movement: SelectionMove::Next,
                        });
                self.apply_completion_effects(effects, &HashMap::new())
            }
            PreparedCompletionAction::Previous => {
                let effects =
                    self.session
                        .completion_mut()
                        .transition(CompletionEvent::MoveSelection {
                            view,
                            movement: SelectionMove::Previous,
                        });
                self.apply_completion_effects(effects, &HashMap::new())
            }
            PreparedCompletionAction::Cancel => self.cancel_completion_view(view),
            PreparedCompletionAction::AcceptanceCommitted { task, candidate } => {
                let effects = self.session.completion_mut().transition(
                    CompletionEvent::AcceptanceCommitted {
                        view,
                        task,
                        candidate,
                    },
                );
                self.apply_completion_effects(effects, &HashMap::new())
            }
        }
    }

    pub(super) fn cancel_completion_view(&mut self, view: ViewId) -> bool {
        let effects = self
            .session
            .completion_mut()
            .transition(CompletionEvent::CancelView { view });
        self.apply_completion_effects(effects, &HashMap::new())
    }

    fn completion_request_for_view(
        &self,
        view: ViewId,
        trigger: CompletionTrigger,
    ) -> Option<CompletionRequestSeed> {
        const CONTEXT_CHARS: usize = 256;
        let view_data = self.session.view(view)?;
        let content = view_data.document_content()?;
        let selections = view_data.selections()?;
        if selections.all().count() != 1 || !selections.primary().is_empty() {
            return None;
        }
        let selection = *selections.primary();
        let cursor = selection.head.char_index;
        let snapshot = self.kernel.contents().text_snapshot(content)?;
        if cursor > snapshot.len_chars() {
            return None;
        }
        let mut start = cursor;
        let query_floor = cursor.saturating_sub(MAX_COMPLETION_QUERY_CHARS);
        while start > query_floor && snapshot.char_at(start - 1).is_some_and(is_identifier_char) {
            start -= 1;
        }
        if start == query_floor
            && start > 0
            && snapshot.char_at(start - 1).is_some_and(is_identifier_char)
        {
            return None;
        }
        let query = snapshot.char_range_to_string(start..cursor)?;
        if matches!(
            trigger,
            CompletionTrigger::Identifier | CompletionTrigger::Delete
        ) && query.is_empty()
        {
            return None;
        }
        let before_start = cursor.saturating_sub(CONTEXT_CHARS);
        let after_end = cursor
            .saturating_add(CONTEXT_CHARS)
            .min(snapshot.len_chars());
        let before = snapshot.char_range_to_string(before_start..cursor)?;
        let after = snapshot.char_range_to_string(cursor..after_end)?;
        let resource_name = match self
            .kernel
            .contents()
            .query(content, ContentQuery::ResourceName)
        {
            ContentData::ResourceName(Some(name)) => name,
            _ => "untitled".to_owned(),
        };
        let resource_path = match self
            .kernel
            .contents()
            .query(content, ContentQuery::ResourcePath)
        {
            ContentData::ResourcePath(path) => path,
            _ => None,
        };
        let language = self
            .kernel
            .classify_content(content)
            .and_then(|classification| classification.language)
            .map(|language| language.as_str().to_owned());
        CompletionRequestSeed::new(
            view,
            content,
            self.kernel.contents().revision(content)?,
            view_data.revision(),
            selection,
            CompletionTextRange::new(
                TextOffset { char_index: start },
                TextOffset { char_index: cursor },
            )?,
            query,
            trigger,
            CompletionRequestContext::new(language, resource_name, resource_path, before, after),
        )
    }

    pub(super) fn reconcile_completion_state(&mut self) -> bool {
        // M1 treats every external target change as cancellation. Explicit
        // trigger and incomplete refresh are the only retrigger transitions;
        // M2 adds automatic trigger policy after the originating frame commits.
        let views = self.session.completion().active_views();
        let mut changed = false;
        for view in views {
            let current = self
                .session
                .completion()
                .request_and_sources(view)
                .is_some_and(|(request, sources)| {
                    self.completion_request_is_current(request, sources)
                });
            if current {
                continue;
            }
            let effects = self
                .session
                .completion_mut()
                .transition(CompletionEvent::CancelView { view });
            changed |= self.apply_completion_effects(effects, &HashMap::new());
        }
        changed
    }

    pub(super) fn handle_completion_message(&mut self, message: AppMessage) -> bool {
        match message {
            AppMessage::CompletionBatchReady(key) => {
                let Some(batch) = self.kernel.take_completion_batch(&key) else {
                    return false;
                };
                self.handle_completion_batch(batch)
            }
            #[cfg(test)]
            AppMessage::CompletionBatchForTest(batch) => self.handle_completion_batch(batch),
            AppMessage::CompletionSourceFinished { key, outcome } => {
                self.handle_completion_source_finished(key, outcome)
            }
            _ => unreachable!("caller routes only completion messages"),
        }
    }

    fn handle_completion_batch(&mut self, batch: CompletionBatch) -> bool {
        let key = batch.key.clone();
        let current = self.kernel.completion_source_is_running(&key)
            && self
                .session
                .completion()
                .request_and_sources(key.view())
                .is_some_and(|(request, sources)| {
                    request.task_key() == key.task()
                        && self.completion_request_is_current(request, sources)
                });
        if !current {
            self.record_completion_diagnostic(
                &key,
                CompletionDiagnosticKind::StaleRejected,
                Some("completion target changed before batch installation".to_owned()),
            );
            self.kernel.cancel_completion_source(&key);
            return self.reconcile_completion_state();
        }

        let effects = self
            .session
            .completion_mut()
            .transition(CompletionEvent::InstallBatch(batch));
        let installed = effects.iter().any(
            |effect| matches!(effect, CompletionEffect::ViewChanged(view) if *view == key.view()),
        ) && !effects.iter().any(|effect| {
            matches!(
                effect,
                CompletionEffect::Ignored { .. }
                    | CompletionEffect::Rejected { .. }
                    | CompletionEffect::SourceFailed { .. }
            )
        });
        if installed {
            self.record_completion_diagnostic(&key, CompletionDiagnosticKind::BatchInstalled, None);
        }
        self.apply_completion_effects(effects, &HashMap::new())
    }

    fn handle_completion_source_finished(
        &mut self,
        key: SourceRequestKey,
        outcome: CompletionSourceTaskOutcome,
    ) -> bool {
        if !self.kernel.finish_completion_source(&key) {
            return false;
        }
        let (kind, detail, event) = match outcome {
            CompletionSourceTaskOutcome::Completed => (
                CompletionDiagnosticKind::Completed,
                None,
                CompletionEvent::SourceCompleted(key.clone()),
            ),
            CompletionSourceTaskOutcome::Cancelled => (
                CompletionDiagnosticKind::Cancelled,
                None,
                CompletionEvent::SourceFailed(key.clone()),
            ),
            CompletionSourceTaskOutcome::TimedOut => (
                CompletionDiagnosticKind::TimedOut,
                None,
                CompletionEvent::SourceTimedOut(key.clone()),
            ),
            CompletionSourceTaskOutcome::Failed(error) => (
                CompletionDiagnosticKind::Failed,
                Some(error.to_string()),
                CompletionEvent::SourceFailed(key.clone()),
            ),
        };
        self.record_completion_diagnostic(&key, kind, detail);
        let effects = self.session.completion_mut().transition(event);
        self.apply_completion_effects(effects, &HashMap::new())
    }

    fn active_completion_sources(&self, view: ViewId) -> Option<Vec<ActiveCompletionSource>> {
        let Some(content) = self
            .session
            .view(view)
            .and_then(|view| view.document_content())
        else {
            return Some(Vec::new());
        };
        let Some(kind) = self.kernel.contents().kind(content) else {
            return Some(Vec::new());
        };
        if kind != ContentKind::Buffer {
            return Some(Vec::new());
        }
        let limit = self.session.completion().max_sources();
        let mut sources = Vec::with_capacity(limit);
        for mode in self.session.view_modes().mode_ids(view) {
            if self.session.view_modes().is_faulted(*mode, view) {
                continue;
            }
            let Some(definitions) = self.kernel.modes().completion_sources(*mode, kind) else {
                continue;
            };
            for definition in definitions {
                if sources.len() == limit {
                    return None;
                }
                sources.push(ActiveCompletionSource {
                    key: completion_source_key(*mode, definition.id()),
                    mode: *mode,
                    local_id: definition.id().clone(),
                });
            }
        }
        Some(sources)
    }

    fn completion_request_is_current(
        &self,
        request: &CompletionRequest,
        expected_sources: &[CompletionSourceKey],
    ) -> bool {
        let focused_view = self.session.view_for_space(self.session.focused());
        let Some(view) = self.session.view(request.view()) else {
            return false;
        };
        let Some(sources) = self.active_completion_sources(request.view()) else {
            return false;
        };
        focused_view == Some(request.view())
            && view.document_content() == Some(request.content())
            && view.revision() == request.view_revision()
            && view
                .selections()
                .is_some_and(|selections| *selections.primary() == request.selection())
            && self.kernel.contents().revision(request.content())
                == Some(request.content_revision())
            && sources
                .iter()
                .map(|source| &source.key)
                .eq(expected_sources.iter())
    }

    fn completion_seed_target_is_current(&self, request: &CompletionRequestSeed) -> bool {
        let focused_view = self.session.view_for_space(self.session.focused());
        let Some(view) = self.session.view(request.view()) else {
            return false;
        };
        focused_view == Some(request.view())
            && view.document_content() == Some(request.content())
            && view.revision() == request.view_revision()
            && view
                .selections()
                .is_some_and(|selections| *selections.primary() == request.selection())
            && self.kernel.contents().revision(request.content())
                == Some(request.content_revision())
    }

    fn apply_completion_effects(
        &mut self,
        effects: Vec<CompletionEffect>,
        active_sources: &HashMap<CompletionSourceKey, ActiveCompletionSource>,
    ) -> bool {
        let mut changed = false;
        let mut changed_views = Vec::new();
        for effect in effects {
            match effect {
                CompletionEffect::RequestSource { key, request } => {
                    let Some(source) = active_sources.get(key.source()) else {
                        let follow_up = self
                            .session
                            .completion_mut()
                            .transition(CompletionEvent::SourceFailed(key.clone()));
                        self.record_completion_diagnostic(
                            &key,
                            CompletionDiagnosticKind::Failed,
                            Some("active completion source definition disappeared".to_owned()),
                        );
                        changed |= self.apply_completion_effects(follow_up, active_sources);
                        continue;
                    };
                    let task = self.session.prepare_completion_source(
                        source.mode,
                        &source.local_id,
                        &request,
                        self.kernel.content_modes(),
                        self.kernel.contents(),
                    );
                    match task {
                        Ok(task) => {
                            if !self
                                .kernel
                                .queue_completion_source(key.clone(), request, task)
                            {
                                let follow_up = self
                                    .session
                                    .completion_mut()
                                    .transition(CompletionEvent::SourceFailed(key.clone()));
                                self.record_completion_diagnostic(
                                    &key,
                                    CompletionDiagnosticKind::Failed,
                                    Some("completion task limit reached".to_owned()),
                                );
                                changed |= self.apply_completion_effects(follow_up, active_sources);
                                continue;
                            }
                            self.record_completion_diagnostic(
                                &key,
                                CompletionDiagnosticKind::Scheduled,
                                None,
                            );
                        }
                        Err(error) => {
                            let follow_up = self
                                .session
                                .completion_mut()
                                .transition(CompletionEvent::SourceFailed(key.clone()));
                            self.record_completion_diagnostic(
                                &key,
                                CompletionDiagnosticKind::Failed,
                                Some(error.to_string()),
                            );
                            changed |= self.apply_completion_effects(follow_up, active_sources);
                        }
                    }
                }
                CompletionEffect::CancelSource(key) => {
                    if self.kernel.cancel_completion_source(&key) {
                        self.record_completion_diagnostic(
                            &key,
                            CompletionDiagnosticKind::Cancelled,
                            None,
                        );
                    }
                }
                CompletionEffect::SourceFailed { key, reason } => {
                    self.kernel.cancel_completion_source(&key);
                    self.record_completion_diagnostic(
                        &key,
                        CompletionDiagnosticKind::Failed,
                        Some(reason.to_owned()),
                    );
                }
                CompletionEffect::ViewChanged(view) => {
                    changed = true;
                    changed_views.push(view);
                }
                CompletionEffect::SelectionChanged(selection) => {
                    let selected = selection
                        .candidate
                        .as_ref()
                        .zip(selection.visible_index)
                        .map(|(candidate, visible_index)| CompletionSelection {
                            candidate: completion_candidate_identity(candidate),
                            visible_index,
                        });
                    self.session
                        .update_completion_presentation_selection(selection.view, selected);
                    changed = true;
                }
                CompletionEffect::RequestResolve(_)
                | CompletionEffect::Accept(_)
                | CompletionEffect::Ignored { .. }
                | CompletionEffect::Rejected { .. } => {}
            }
        }
        changed_views.sort_unstable_by_key(|view| view.0);
        changed_views.dedup();
        for view in changed_views {
            self.refresh_completion_presentation(view);
        }
        changed
    }

    fn refresh_completion_presentation(&mut self, view: ViewId) {
        let space = self.session.body_space_for_view(view);
        let presentation =
            self.session
                .completion()
                .snapshot(view)
                .zip(space)
                .map(|(snapshot, space)| {
                    let selected = snapshot.selected.as_ref().and_then(|selected| {
                        snapshot
                            .candidates
                            .iter()
                            .position(|candidate| &candidate.id == selected)
                            .map(|visible_index| CompletionSelection {
                                candidate: completion_candidate_identity(selected),
                                visible_index,
                            })
                    });
                    let documentation = snapshot.resolved.as_ref().and_then(|(candidate, data)| {
                        (snapshot.selected.as_ref() == Some(candidate))
                            .then(|| data.documentation.clone())
                            .flatten()
                    });
                    CompletionPresentation {
                        view,
                        space,
                        anchor: snapshot.request.range().end(),
                        rows: snapshot
                            .candidates
                            .into_iter()
                            .map(|candidate| {
                                let source = self
                                    .session
                                    .completion_source_label(view, &candidate.id.source)
                                    .map_or_else(|| Arc::from("unknown"), Arc::from);
                                CompletionRow {
                                    candidate: completion_candidate_identity(&candidate.id),
                                    label: candidate.label,
                                    kind: candidate.kind,
                                    detail: candidate.detail,
                                    source,
                                    group: candidate.group,
                                    deprecated: candidate.deprecated,
                                    match_positions: candidate.match_positions,
                                }
                            })
                            .collect::<Vec<_>>()
                            .into(),
                        selected,
                        status: CompletionStatus {
                            collecting: snapshot.collecting,
                            incomplete: snapshot.incomplete,
                            source_faults: snapshot.source_faults,
                        },
                        documentation,
                    }
                });
        self.session.set_completion_presentation(view, presentation);
    }

    fn record_completion_diagnostic(
        &mut self,
        key: &SourceRequestKey,
        kind: CompletionDiagnosticKind,
        mut detail: Option<String>,
    ) {
        if let Some(detail) = &mut detail
            && detail.len() > MAX_COMPLETION_DIAGNOSTIC_BYTES
        {
            let mut end = MAX_COMPLETION_DIAGNOSTIC_BYTES;
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            detail.truncate(end);
        }
        if self.completion_diagnostics.len() == MAX_COMPLETION_DIAGNOSTICS {
            self.completion_diagnostics.pop_front();
        }
        self.completion_diagnostics.push_back(CompletionDiagnostic {
            view: key.view(),
            task: key.task(),
            source: key.source().clone(),
            kind,
            detail,
        });
    }
}

fn completion_candidate_identity(
    candidate: &vell_completion::CandidateId,
) -> CompletionCandidateIdentity {
    CompletionCandidateIdentity::new(format!(
        "{}:{}:{}",
        candidate.source.as_str(),
        candidate.batch.0,
        candidate.ordinal
    ))
    .expect("engine-owned candidate identity stays within protocol bounds")
}

pub(super) fn is_identifier_char(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

pub(super) fn is_single_identifier_text(text: &str) -> bool {
    let mut characters = text.chars();
    characters.next().is_some_and(is_identifier_char) && characters.next().is_none()
}

fn completion_source_key(mode: ModeId, source: &CompletionSourceId) -> CompletionSourceKey {
    CompletionSourceKey::new(format!(
        "{}:{}:{}",
        mode.get(),
        source.as_str().len(),
        source.as_str()
    ))
}
