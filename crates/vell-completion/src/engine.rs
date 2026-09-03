use std::collections::{HashMap, HashSet};
use std::mem;

use vell_protocol::ids::ViewId;

use crate::matcher::{MatchInput, Matched, match_top_k};
use crate::model::{
    CandidateId, CompletionAcceptance, CompletionBatch, CompletionBatchKind, CompletionCandidate,
    CompletionConfigError, CompletionEffect, CompletionEvent, CompletionInteractionState,
    CompletionItem, CompletionLimits, CompletionRequest, CompletionRequestSeed,
    CompletionSelectionChange, CompletionSessionId, CompletionSnapshot, CompletionSourceKey,
    IncompleteDirections, RequestEpoch, ResolveData, ResolveRequestKey, SourceBatchVersion,
    SourceRequestKey,
};

pub struct CompletionEngine {
    limits: CompletionLimits,
    sessions: HashMap<ViewId, Session>,
    next_session: u64,
}

impl Default for CompletionEngine {
    fn default() -> Self {
        Self::new(CompletionLimits::default()).expect("default completion limits are valid")
    }
}

impl CompletionEngine {
    pub fn new(limits: CompletionLimits) -> Result<Self, CompletionConfigError> {
        Ok(Self {
            limits: limits.validate()?,
            sessions: HashMap::new(),
            next_session: 1,
        })
    }

    pub fn transition(&mut self, event: CompletionEvent) -> Vec<CompletionEffect> {
        match event {
            CompletionEvent::Trigger { request, sources } => self.trigger(request, sources),
            CompletionEvent::InstallBatch(batch) => self.install_batch(batch),
            CompletionEvent::SourceCompleted(key) => self.source_completed(key),
            CompletionEvent::SourceFailed(key) => self.source_failed(key),
            CompletionEvent::SourceTimedOut(key) => self.source_timed_out(key),
            CompletionEvent::MoveSelection { view, movement } => {
                let Some(session) = self.sessions.get_mut(&view) else {
                    return ignored(Some(view), "missing session");
                };
                session.move_selection(movement);
                let visible_index = session.selected.as_ref().and_then(|selected| {
                    session
                        .matched
                        .iter()
                        .position(|matched| &matched.id == selected)
                });
                vec![CompletionEffect::SelectionChanged(
                    CompletionSelectionChange {
                        view,
                        candidate: session.selected.clone(),
                        visible_index,
                    },
                )]
            }
            CompletionEvent::ResolveSelected { view } => self.resolve_selected(view),
            CompletionEvent::InstallResolved { key, data } => self.install_resolved(key, data),
            CompletionEvent::AcceptSelected { view } => self.accept_selected(view),
            CompletionEvent::AcceptanceCommitted {
                view,
                task,
                candidate,
            } => self.acceptance_committed(view, task, candidate),
            CompletionEvent::CancelView { view } => {
                let Some(session) = self.sessions.remove(&view) else {
                    return ignored(Some(view), "missing session");
                };
                let mut effects = session.cancel_effects();
                effects.push(CompletionEffect::ViewChanged(view));
                effects
            }
        }
    }

    pub fn snapshot(&self, view: ViewId) -> Option<CompletionSnapshot> {
        let session = self.sessions.get(&view)?;
        let candidates = session
            .matched
            .iter()
            .filter_map(|matched| {
                let item = session.item(&matched.id)?;
                Some(CompletionCandidate {
                    id: matched.id.clone(),
                    label: item.label.clone(),
                    detail: item.detail.clone(),
                    kind: item.kind.clone(),
                    group: item.group.clone(),
                    deprecated: item.deprecated,
                    match_positions: if item
                        .filter_text
                        .as_deref()
                        .is_none_or(|filter| filter == item.label.as_ref())
                    {
                        matched.positions.clone()
                    } else {
                        Default::default()
                    },
                })
            })
            .collect();
        Some(CompletionSnapshot {
            request: session.request.clone(),
            sources: session.source_order.clone(),
            candidates,
            selected: session.selected.clone(),
            collecting: session.sources.values().any(|source| source.pending),
            incomplete: session
                .sources
                .values()
                .any(|source| source.incomplete.forward || source.incomplete.backward),
            source_faults: session
                .sources
                .values()
                .filter(|source| source.failed || source.timed_out)
                .count(),
            resolved: session.resolved.clone(),
            source_count: session.sources.len(),
            item_count: session.item_count,
            estimated_heap_bytes: session.estimated_heap_bytes(),
        })
    }

    pub fn active_views(&self) -> Vec<ViewId> {
        self.sessions.keys().copied().collect()
    }

    pub fn max_sources(&self) -> usize {
        self.limits.max_sources
    }

    pub fn interaction_state(&self, view: ViewId) -> Option<CompletionInteractionState> {
        let session = self.sessions.get(&view)?;
        Some(CompletionInteractionState {
            content: session.request.content(),
            has_selection: session.selected.is_some(),
        })
    }

    pub fn request_and_sources(
        &self,
        view: ViewId,
    ) -> Option<(&CompletionRequest, &[CompletionSourceKey])> {
        let session = self.sessions.get(&view)?;
        Some((&session.request, &session.source_order))
    }

    fn trigger(
        &mut self,
        seed: CompletionRequestSeed,
        sources: Vec<CompletionSourceKey>,
    ) -> Vec<CompletionEffect> {
        let view = seed.view();
        let sources = match self.validate_request(&seed, sources) {
            Ok(sources) => sources,
            Err(reason) => return rejected(Some(view), reason),
        };
        let (session_id, epoch, next_session) = match self.sessions.get(&view) {
            Some(current) => {
                let Some(epoch) = current.request.epoch().0.checked_add(1).map(RequestEpoch) else {
                    return rejected(Some(view), "request epoch exhausted");
                };
                (current.request.session(), epoch, None)
            }
            None => {
                let Some(next) = self.next_session.checked_add(1) else {
                    return rejected(Some(view), "session identity exhausted");
                };
                (
                    CompletionSessionId(self.next_session),
                    RequestEpoch(1),
                    Some(next),
                )
            }
        };
        let request = CompletionRequest::from_seed(session_id, epoch, seed);
        let session = Session::new(request.clone(), &sources);
        if session.estimated_heap_upper_bound(0, self.limits.visible_items)
            > self.limits.max_session_bytes
        {
            return rejected(Some(view), "session limit exceeded");
        }
        if let Some(next) = next_session {
            self.next_session = next;
        }
        let mut effects = self
            .sessions
            .remove(&view)
            .map_or_else(Vec::new, Session::cancel_effects);
        for source in sources {
            effects.push(CompletionEffect::RequestSource {
                key: request.source_key(source),
                request: request.clone(),
            });
        }
        self.sessions.insert(view, session);
        effects.push(CompletionEffect::ViewChanged(view));
        effects
    }

    fn validate_request(
        &self,
        seed: &CompletionRequestSeed,
        sources: Vec<CompletionSourceKey>,
    ) -> Result<Vec<CompletionSourceKey>, &'static str> {
        if seed.query().len() > self.limits.max_query_bytes {
            return Err("query byte limit exceeded");
        }
        if seed.context().estimated_heap_bytes() > self.limits.max_context_bytes
            || seed
                .context()
                .strings()
                .any(|value| value.len() > self.limits.max_string_bytes)
        {
            return Err("request context limit exceeded");
        }
        if sources.len() > self.limits.max_sources
            || sources
                .iter()
                .any(|source| source.as_str().len() > self.limits.max_source_key_bytes)
        {
            return Err("source limit exceeded");
        }
        let mut seen = HashSet::new();
        Ok(sources
            .into_iter()
            .filter(|source| seen.insert(source.clone()))
            .collect())
    }

    fn install_batch(&mut self, batch: CompletionBatch) -> Vec<CompletionEffect> {
        let view = batch.key.view;
        let source_key = batch.key.clone();
        let Some(session) = self.sessions.get_mut(&view) else {
            return ignored(Some(view), "missing session");
        };
        if session.request.task_key() != batch.key.task {
            return ignored(Some(view), "stale request identity");
        }
        let session_non_item_upper =
            session.estimated_heap_upper_bound(0, self.limits.visible_items);
        let Some(source) = session.sources.get_mut(&batch.key.source) else {
            return ignored(Some(view), "inactive source");
        };
        if source.timed_out {
            return ignored(Some(view), "source timed out");
        }
        if source.final_batch || source.failed {
            return ignored(Some(view), "source already completed");
        }

        let accepted = match batch.kind {
            CompletionBatchKind::Replace => {
                source.version.is_none_or(|version| batch.version > version)
            }
            CompletionBatchKind::Append { sequence } => {
                source.version == Some(batch.version) && sequence == source.next_sequence
            }
        };
        if !accepted {
            return ignored(Some(view), "out-of-order source batch");
        }

        let batch_bytes = batch
            .items
            .iter()
            .map(CompletionItem::estimated_heap_bytes)
            .fold(0_usize, usize::saturating_add);
        let invalid_string = batch
            .items
            .iter()
            .flat_map(CompletionItem::strings)
            .any(|value| value.len() > self.limits.max_string_bytes);
        if batch.items.len() > self.limits.max_source_items
            || batch_bytes > self.limits.max_batch_bytes
            || invalid_string
        {
            source.pending = false;
            source.failed = true;
            return source_failed(source_key, "source batch limit exceeded");
        }

        let old_count = source.item_count;
        let old_bytes = source.item_heap_bytes;
        let old_chunk_bytes = source.chunk_heap_bytes();
        let (next_source_count, next_source_bytes, next_source_batches, next_chunk_bytes) =
            match batch.kind {
                CompletionBatchKind::Replace => (
                    batch.items.len(),
                    batch_bytes,
                    1,
                    mem::size_of::<Box<[CompletionItem]>>(),
                ),
                CompletionBatchKind::Append { .. } => (
                    old_count.saturating_add(batch.items.len()),
                    old_bytes.saturating_add(batch_bytes),
                    source.chunks.len().saturating_add(1),
                    source.predicted_append_chunk_heap_bytes(),
                ),
            };
        let next_count = session
            .item_count
            .saturating_sub(old_count)
            .saturating_add(next_source_count);
        let next_item_bytes = session
            .item_heap_bytes
            .saturating_sub(old_bytes)
            .saturating_add(next_source_bytes);
        if next_source_count > self.limits.max_source_items
            || next_source_batches > self.limits.max_source_batches
            || next_count > self.limits.max_session_items
            || session_non_item_upper
                .saturating_sub(old_chunk_bytes)
                .saturating_add(next_chunk_bytes)
                .saturating_add(next_item_bytes)
                > self.limits.max_session_bytes
        {
            source.pending = false;
            source.failed = true;
            return source_failed(source_key, "session limit exceeded");
        }

        match batch.kind {
            CompletionBatchKind::Replace => {
                source.version = Some(batch.version);
                source.next_sequence = 1;
                source.chunks = vec![batch.items.into_boxed_slice()];
                source.item_count = next_source_count;
                source.item_heap_bytes = batch_bytes;
            }
            CompletionBatchKind::Append { .. } => {
                source.next_sequence += 1;
                source.chunks.push(batch.items.into_boxed_slice());
                source.item_count = next_source_count;
                source.item_heap_bytes += batch_bytes;
            }
        }
        source.pending = !batch.is_final;
        source.final_batch = batch.is_final;
        source.incomplete = batch.incomplete;
        session.item_count = next_count;
        session.item_heap_bytes = next_item_bytes;
        session.recompute(self.limits.visible_items);
        vec![CompletionEffect::ViewChanged(view)]
    }

    fn source_timed_out(&mut self, key: SourceRequestKey) -> Vec<CompletionEffect> {
        let Some(session) = self.sessions.get_mut(&key.view) else {
            return ignored(Some(key.view), "missing session");
        };
        if session.request.task_key() != key.task {
            return ignored(Some(key.view), "stale timeout");
        }
        let Some(source) = session.sources.get_mut(&key.source) else {
            return ignored(Some(key.view), "inactive source");
        };
        source.pending = false;
        source.timed_out = true;
        vec![CompletionEffect::ViewChanged(key.view)]
    }

    fn source_failed(&mut self, key: SourceRequestKey) -> Vec<CompletionEffect> {
        let Some(session) = self.sessions.get_mut(&key.view) else {
            return ignored(Some(key.view), "missing session");
        };
        if session.request.task_key() != key.task {
            return ignored(Some(key.view), "stale request identity");
        }
        let Some(source) = session.sources.get_mut(&key.source) else {
            return ignored(Some(key.view), "inactive source");
        };
        if source.timed_out || source.failed || source.final_batch {
            return ignored(Some(key.view), "source already completed");
        }
        source.pending = false;
        source.failed = true;
        vec![CompletionEffect::ViewChanged(key.view)]
    }

    fn source_completed(&mut self, key: SourceRequestKey) -> Vec<CompletionEffect> {
        let Some(session) = self.sessions.get_mut(&key.view) else {
            return ignored(Some(key.view), "missing session");
        };
        if session.request.task_key() != key.task {
            return ignored(Some(key.view), "stale request identity");
        }
        let Some(source) = session.sources.get_mut(&key.source) else {
            return ignored(Some(key.view), "inactive source");
        };
        if source.timed_out || source.failed || source.final_batch {
            return ignored(Some(key.view), "source already completed");
        }
        source.pending = false;
        source.final_batch = true;
        vec![CompletionEffect::ViewChanged(key.view)]
    }

    fn resolve_selected(&self, view: ViewId) -> Vec<CompletionEffect> {
        let Some(session) = self.sessions.get(&view) else {
            return ignored(Some(view), "missing session");
        };
        let Some(candidate) = session.selected.clone() else {
            return ignored(Some(view), "no selected candidate");
        };
        if !session.source_available_for_resolve(&candidate) {
            return ignored(Some(view), "selected source unavailable for resolve");
        }
        vec![CompletionEffect::RequestResolve(ResolveRequestKey {
            task: session.request.task_key(),
            view,
            candidate,
        })]
    }

    fn install_resolved(
        &mut self,
        key: ResolveRequestKey,
        data: ResolveData,
    ) -> Vec<CompletionEffect> {
        let Some(session) = self.sessions.get_mut(&key.view) else {
            return ignored(Some(key.view), "missing session");
        };
        if session.request.task_key() != key.task
            || session.selected.as_ref() != Some(&key.candidate)
            || !session.candidate_exists(&key.candidate)
            || !session.source_available_for_resolve(&key.candidate)
        {
            return ignored(Some(key.view), "stale resolve result");
        }
        if data
            .detail
            .iter()
            .chain(data.documentation.iter())
            .any(|value| value.len() > self.limits.max_string_bytes)
        {
            return self.fail_resolve_source(key, "resolve string limit exceeded");
        }
        let previous_bytes = session
            .resolved
            .as_ref()
            .map_or(0, |(_, data)| data.estimated_heap_bytes());
        let next_bytes = session
            .estimated_heap_bytes()
            .saturating_sub(previous_bytes)
            .saturating_add(data.estimated_heap_bytes());
        if next_bytes > self.limits.max_session_bytes {
            return self.fail_resolve_source(key, "resolve session limit exceeded");
        }
        session.resolved = Some((key.candidate, data));
        vec![CompletionEffect::ViewChanged(key.view)]
    }

    fn fail_resolve_source(
        &mut self,
        key: ResolveRequestKey,
        reason: &'static str,
    ) -> Vec<CompletionEffect> {
        let Some(session) = self.sessions.get_mut(&key.view) else {
            return ignored(Some(key.view), "missing session");
        };
        let source_key = session.request.source_key(key.candidate.source.clone());
        if let Some(source) = session.sources.get_mut(&key.candidate.source) {
            source.pending = false;
            source.failed = true;
        }
        source_failed(source_key, reason)
    }

    fn accept_selected(&self, view: ViewId) -> Vec<CompletionEffect> {
        let Some(session) = self.sessions.get(&view) else {
            return ignored(Some(view), "missing session");
        };
        let Some(candidate) = session.selected.clone() else {
            return ignored(Some(view), "no selected candidate");
        };
        let Some(item) = session.item(&candidate) else {
            return ignored(Some(view), "selected candidate disappeared");
        };
        vec![CompletionEffect::Accept(CompletionAcceptance {
            task: session.request.task_key(),
            view,
            content: session.request.content(),
            content_revision: session.request.content_revision(),
            view_revision: session.request.view_revision(),
            selection: session.request.selection(),
            candidate,
            range: item.insert_range.unwrap_or(session.request.range()),
            text: item.insert_text.clone(),
        })]
    }

    fn acceptance_committed(
        &mut self,
        view: ViewId,
        task: crate::model::CompletionTaskKey,
        candidate: CandidateId,
    ) -> Vec<CompletionEffect> {
        let valid = self.sessions.get(&view).is_some_and(|session| {
            session.request.task_key() == task && session.selected.as_ref() == Some(&candidate)
        });
        if !valid {
            return ignored(Some(view), "stale acceptance commit");
        }
        let session = self
            .sessions
            .remove(&view)
            .expect("validated session exists");
        let mut effects = session.cancel_effects();
        effects.push(CompletionEffect::ViewChanged(view));
        effects
    }
}

struct Session {
    request: CompletionRequest,
    sources: HashMap<CompletionSourceKey, SourceState>,
    source_order: Vec<CompletionSourceKey>,
    matched: Vec<Matched>,
    selected: Option<CandidateId>,
    resolved: Option<(CandidateId, ResolveData)>,
    item_count: usize,
    item_heap_bytes: usize,
}

impl Session {
    fn new(request: CompletionRequest, sources: &[CompletionSourceKey]) -> Self {
        Self {
            request,
            sources: sources
                .iter()
                .cloned()
                .map(|source| {
                    (
                        source,
                        SourceState {
                            pending: true,
                            ..SourceState::default()
                        },
                    )
                })
                .collect(),
            source_order: sources.to_vec(),
            matched: Vec::new(),
            selected: None,
            resolved: None,
            item_count: 0,
            item_heap_bytes: 0,
        }
    }

    fn cancel_effects(self) -> Vec<CompletionEffect> {
        self.sources
            .into_iter()
            .filter(|(_, source)| source.pending)
            .map(|(source, _)| {
                CompletionEffect::CancelSource(SourceRequestKey {
                    view: self.request.view(),
                    task: self.request.task_key(),
                    source,
                })
            })
            .collect()
    }

    fn item(&self, id: &CandidateId) -> Option<&CompletionItem> {
        let source = self.sources.get(&id.source)?;
        (source.version == Some(id.batch))
            .then(|| source.item(id.ordinal))
            .flatten()
    }

    fn candidate_exists(&self, id: &CandidateId) -> bool {
        self.matched.iter().any(|candidate| &candidate.id == id)
    }

    fn source_available_for_resolve(&self, id: &CandidateId) -> bool {
        self.sources
            .get(&id.source)
            .is_some_and(|source| !source.failed && !source.timed_out)
    }

    fn recompute(&mut self, limit: usize) {
        let previous_selection = self.selected.clone();
        let previous_selected_match = previous_selection.as_ref().and_then(|selected| {
            self.matched
                .iter()
                .find(|candidate| &candidate.id == selected)
                .cloned()
        });
        let was_empty = self.matched.is_empty();
        let inputs = self
            .source_order
            .iter()
            .enumerate()
            .flat_map(|(source_order, key)| {
                let source = self
                    .sources
                    .get(key)
                    .expect("source order only contains registered sources");
                let version = source.version;
                source
                    .chunks
                    .iter()
                    .flat_map(|chunk| chunk.iter())
                    .enumerate()
                    .filter_map(move |(ordinal, item)| {
                        version.map(|batch| MatchInput {
                            source: key,
                            batch,
                            ordinal,
                            item,
                            source_order,
                        })
                    })
            });
        self.matched = match_top_k(self.request.query(), inputs, limit);
        let pinned_selection = previous_selected_match.filter(|selected| {
            self.item(&selected.id).is_some() && !self.candidate_exists(&selected.id)
        });
        if let Some(selected) = pinned_selection {
            if self.matched.len() == limit {
                self.matched.pop();
            }
            if limit > 0 {
                self.matched.push(selected);
            }
        }
        self.selected = match previous_selection {
            Some(selected) if self.candidate_exists(&selected) => Some(selected),
            Some(_) => None,
            None if was_empty => self.matched.first().map(|candidate| candidate.id.clone()),
            None => None,
        };
        if self
            .resolved
            .as_ref()
            .is_some_and(|(candidate, _)| Some(candidate) != self.selected.as_ref())
        {
            self.resolved = None;
        }
    }

    fn move_selection(&mut self, movement: crate::model::SelectionMove) {
        if self.matched.is_empty() {
            self.selected = None;
            self.resolved = None;
            return;
        }
        let current = self.selected.as_ref().and_then(|selected| {
            self.matched
                .iter()
                .position(|candidate| &candidate.id == selected)
        });
        let next = match movement {
            crate::model::SelectionMove::Next => current.map_or(0, |index| {
                index
                    .checked_add(1)
                    .filter(|next| *next < self.matched.len())
                    .unwrap_or(0)
            }),
            crate::model::SelectionMove::Previous => current
                .and_then(|index| index.checked_sub(1))
                .unwrap_or(self.matched.len() - 1),
            crate::model::SelectionMove::First => 0,
            crate::model::SelectionMove::None => {
                self.selected = None;
                self.resolved = None;
                return;
            }
        };
        self.selected = Some(self.matched[next].id.clone());
        self.resolved = None;
    }

    fn estimated_heap_bytes(&self) -> usize {
        self.base_heap_bytes()
            .saturating_add(self.item_heap_bytes)
            .saturating_add(self.matched_heap_bytes())
            .saturating_add(
                self.resolved
                    .as_ref()
                    .map_or(0, |(_, data)| data.estimated_heap_bytes()),
            )
    }

    fn estimated_heap_upper_bound(&self, item_bytes: usize, visible_items: usize) -> usize {
        let match_bytes = visible_items.saturating_mul(
            mem::size_of::<Matched>().saturating_add(
                self.request
                    .query()
                    .chars()
                    .count()
                    .saturating_mul(mem::size_of::<usize>()),
            ),
        );
        self.base_heap_bytes()
            .saturating_add(item_bytes)
            .saturating_add(self.matched_heap_bytes())
            .saturating_add(match_bytes)
            .saturating_add(
                self.resolved
                    .as_ref()
                    .map_or(0, |(_, data)| data.estimated_heap_bytes()),
            )
    }

    fn matched_heap_bytes(&self) -> usize {
        self.matched
            .capacity()
            .saturating_mul(mem::size_of::<Matched>())
            .saturating_add(
                self.matched
                    .iter()
                    .map(|matched| {
                        matched
                            .positions
                            .len()
                            .saturating_mul(mem::size_of::<usize>())
                    })
                    .sum::<usize>(),
            )
    }

    fn base_heap_bytes(&self) -> usize {
        let source_bytes = self
            .source_order
            .capacity()
            .saturating_mul(mem::size_of::<CompletionSourceKey>())
            .saturating_add(self.sources.capacity().saturating_mul(
                mem::size_of::<CompletionSourceKey>().saturating_add(mem::size_of::<SourceState>()),
            ))
            .saturating_add(self.source_order.iter().fold(0_usize, |total, source| {
                total.saturating_add(source.as_str().len())
            }))
            .saturating_add(
                self.sources
                    .values()
                    .map(SourceState::chunk_heap_bytes)
                    .sum::<usize>(),
            );
        mem::size_of::<Self>()
            .saturating_add(self.request.estimated_heap_bytes())
            .saturating_add(source_bytes)
    }
}

#[derive(Default)]
struct SourceState {
    version: Option<SourceBatchVersion>,
    next_sequence: u32,
    chunks: Vec<Box<[CompletionItem]>>,
    item_count: usize,
    pending: bool,
    final_batch: bool,
    timed_out: bool,
    failed: bool,
    incomplete: IncompleteDirections,
    item_heap_bytes: usize,
}

impl SourceState {
    fn item(&self, mut ordinal: usize) -> Option<&CompletionItem> {
        for chunk in &self.chunks {
            if ordinal < chunk.len() {
                return chunk.get(ordinal);
            }
            ordinal -= chunk.len();
        }
        None
    }

    fn chunk_heap_bytes(&self) -> usize {
        self.chunks
            .capacity()
            .saturating_mul(mem::size_of::<Box<[CompletionItem]>>())
    }

    fn predicted_append_chunk_heap_bytes(&self) -> usize {
        let next_capacity = if self.chunks.len() < self.chunks.capacity() {
            self.chunks.capacity()
        } else {
            self.chunks.capacity().max(2).saturating_mul(2)
        };
        next_capacity.saturating_mul(mem::size_of::<Box<[CompletionItem]>>())
    }
}

fn ignored(view: Option<ViewId>, reason: &'static str) -> Vec<CompletionEffect> {
    vec![CompletionEffect::Ignored { view, reason }]
}

fn rejected(view: Option<ViewId>, reason: &'static str) -> Vec<CompletionEffect> {
    vec![CompletionEffect::Rejected { view, reason }]
}

fn source_failed(key: SourceRequestKey, reason: &'static str) -> Vec<CompletionEffect> {
    let view = key.view();
    vec![
        CompletionEffect::SourceFailed {
            key: key.clone(),
            reason,
        },
        CompletionEffect::CancelSource(key),
        CompletionEffect::ViewChanged(view),
    ]
}

#[cfg(test)]
mod tests {
    use vell_protocol::ids::{ContentId, ViewId};
    use vell_protocol::revision::Revision;
    use vell_protocol::selection::{Selection, TextOffset};

    use super::*;
    use crate::model::{
        CompletionRequestContext, CompletionTextRange, CompletionTrigger, SelectionMove,
    };

    fn seed(view: u64, query: &str) -> CompletionRequestSeed {
        let end = TextOffset {
            char_index: query.chars().count(),
        };
        CompletionRequestSeed::new(
            ViewId(view),
            ContentId(7),
            Revision(3),
            Revision(2),
            Selection::collapsed(end),
            CompletionTextRange::new(TextOffset::origin(), end).unwrap(),
            query,
            CompletionTrigger::Manual,
            CompletionRequestContext::new(Some("rust"), "main.rs", Some("src/main.rs"), query, ""),
        )
        .unwrap()
    }

    fn source(name: &str) -> CompletionSourceKey {
        CompletionSourceKey::from(name)
    }

    fn item(label: &str) -> CompletionItem {
        CompletionItem::new(label, label)
    }

    struct FakeSource(CompletionSourceKey);

    impl FakeSource {
        fn new(name: &str) -> Self {
            Self(source(name))
        }

        fn key(&self) -> CompletionSourceKey {
            self.0.clone()
        }

        fn append(
            &self,
            request: &CompletionRequest,
            version: u64,
            sequence: u32,
            items: Vec<CompletionItem>,
            is_final: bool,
        ) -> CompletionBatch {
            CompletionBatch::append(
                request.source_key(self.key()),
                SourceBatchVersion(version),
                sequence,
                items,
                is_final,
                IncompleteDirections::default(),
            )
        }
    }

    fn start(
        engine: &mut CompletionEngine,
        seed: CompletionRequestSeed,
        sources: Vec<CompletionSourceKey>,
    ) -> CompletionRequest {
        let view = seed.view();
        engine.transition(CompletionEvent::Trigger {
            request: seed,
            sources,
        });
        engine.snapshot(view).unwrap().request
    }

    fn replace(
        request: &CompletionRequest,
        source: &CompletionSourceKey,
        version: u64,
        items: Vec<CompletionItem>,
        final_batch: bool,
    ) -> CompletionBatch {
        CompletionBatch::replace(
            request.source_key(source.clone()),
            SourceBatchVersion(version),
            items,
            final_batch,
            IncompleteDirections::default(),
        )
    }

    #[test]
    fn tests_drive_sessions_only_through_transition_and_snapshot() {
        let mut engine = CompletionEngine::default();
        let request = start(
            &mut engine,
            seed(1, "pr"),
            vec![source("words"), source("lsp")],
        );
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            1,
            vec![
                item("println")
                    .with_kind("function")
                    .with_group("buffer")
                    .deprecated(true),
                item("alpha"),
            ],
            true,
        )));
        let snapshot = engine.snapshot(ViewId(1)).unwrap();
        assert_eq!(snapshot.candidates.len(), 1);
        assert_eq!(&*snapshot.candidates[0].label, "println");
        assert_eq!(snapshot.candidates[0].kind.as_deref(), Some("function"));
        assert_eq!(snapshot.candidates[0].group.as_deref(), Some("buffer"));
        assert!(snapshot.candidates[0].deprecated);
        assert!(snapshot.collecting);
    }

    #[test]
    fn snapshot_does_not_apply_filter_text_offsets_to_a_different_label() {
        let mut engine = CompletionEngine::default();
        let request = start(&mut engine, seed(1, "pr"), vec![source("words")]);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            1,
            vec![item("display label").with_filter_text("print")],
            true,
        )));

        let snapshot = engine.snapshot(ViewId(1)).unwrap();
        assert_eq!(&*snapshot.candidates[0].label, "display label");
        assert!(snapshot.candidates[0].match_positions.is_empty());
    }

    #[test]
    fn stale_start_and_previous_session_batch_cannot_replace_current_session() {
        let mut engine = CompletionEngine::default();
        let old = start(&mut engine, seed(1, "a"), vec![source("slow")]);
        let current = start(&mut engine, seed(1, "ab"), vec![source("slow")]);
        assert_eq!(old.session(), current.session());
        assert_eq!(old.epoch(), RequestEpoch(1));
        assert_eq!(current.epoch(), RequestEpoch(2));

        let effects = engine.transition(CompletionEvent::InstallBatch(replace(
            &old,
            &source("slow"),
            1,
            vec![item("abacus")],
            true,
        )));
        assert!(matches!(
            effects.as_slice(),
            [CompletionEffect::Ignored {
                reason: "stale request identity",
                ..
            }]
        ));
    }

    #[test]
    fn streaming_requires_monotonic_sequence_and_preserves_selection() {
        let mut engine = CompletionEngine::default();
        let stream = FakeSource::new("stream");
        let request = start(&mut engine, seed(1, "a"), vec![stream.key()]);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &stream.key(),
            4,
            vec![item("alpha")],
            false,
        )));
        let selected = engine.snapshot(ViewId(1)).unwrap().selected.unwrap();
        let effects = engine.transition(CompletionEvent::InstallBatch(stream.append(
            &request,
            4,
            2,
            vec![item("amber")],
            false,
        )));
        assert!(matches!(
            effects.as_slice(),
            [CompletionEffect::Ignored {
                reason: "out-of-order source batch",
                ..
            }]
        ));
        engine.transition(CompletionEvent::InstallBatch(stream.append(
            &request,
            4,
            1,
            vec![item("amber")],
            true,
        )));
        let snapshot = engine.snapshot(ViewId(1)).unwrap();
        assert_eq!(snapshot.candidates.len(), 2);
        assert_eq!(snapshot.selected, Some(selected));
        assert!(!snapshot.collecting);
    }

    #[test]
    fn selection_is_pinned_by_identity_when_it_falls_out_of_top_k() {
        let mut engine = CompletionEngine::new(CompletionLimits {
            visible_items: 2,
            ..CompletionLimits::default()
        })
        .unwrap();
        let first = source("first");
        let second = source("second");
        let request = start(
            &mut engine,
            seed(1, "a"),
            vec![first.clone(), second.clone()],
        );
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &first,
            1,
            vec![item("aa"), item("ab")],
            true,
        )));
        engine.transition(CompletionEvent::MoveSelection {
            view: ViewId(1),
            movement: SelectionMove::Next,
        });
        assert_eq!(
            engine.snapshot(ViewId(1)).unwrap().selected,
            Some(CandidateId {
                source: first.clone(),
                batch: SourceBatchVersion(1),
                ordinal: 1,
            })
        );

        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &second,
            1,
            vec![item("a0")],
            true,
        )));
        let snapshot = engine.snapshot(ViewId(1)).unwrap();
        assert_eq!(snapshot.candidates.len(), 2);
        let selected = CandidateId {
            source: first,
            batch: SourceBatchVersion(1),
            ordinal: 1,
        };
        assert_eq!(snapshot.selected, Some(selected.clone()));
        assert_eq!(snapshot.candidates[1].id, selected);
        let effects = engine.transition(CompletionEvent::AcceptSelected { view: ViewId(1) });
        assert!(matches!(
            effects.as_slice(),
            [CompletionEffect::Accept(CompletionAcceptance { candidate, .. })]
                if candidate == &selected
        ));
    }

    #[test]
    fn removed_selection_becomes_none_instead_of_changing_identity() {
        let mut engine = CompletionEngine::default();
        let words = source("words");
        let request = start(&mut engine, seed(1, "a"), vec![words.clone()]);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &words,
            1,
            vec![item("aa"), item("ab")],
            false,
        )));
        engine.transition(CompletionEvent::MoveSelection {
            view: ViewId(1),
            movement: SelectionMove::Next,
        });
        let old = engine.snapshot(ViewId(1)).unwrap().selected.unwrap();

        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &words,
            2,
            vec![item("aa"), item("ac")],
            true,
        )));
        let snapshot = engine.snapshot(ViewId(1)).unwrap();
        assert_eq!(snapshot.selected, None);
        assert!(
            snapshot
                .candidates
                .iter()
                .all(|candidate| candidate.id != old)
        );
        assert!(matches!(
            engine
                .transition(CompletionEvent::AcceptSelected { view: ViewId(1) })
                .as_slice(),
            [CompletionEffect::Ignored {
                reason: "no selected candidate",
                ..
            }]
        ));
    }

    #[test]
    fn timeout_and_cancel_stop_collection() {
        let mut engine = CompletionEngine::default();
        let request = start(
            &mut engine,
            seed(1, "a"),
            vec![source("slow"), source("other")],
        );
        engine.transition(CompletionEvent::SourceTimedOut(
            request.source_key(source("slow")),
        ));
        assert!(engine.snapshot(ViewId(1)).unwrap().collecting);
        let effects = engine.transition(CompletionEvent::CancelView { view: ViewId(1) });
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, CompletionEffect::CancelSource(_)))
                .count(),
            1
        );
        assert!(engine.snapshot(ViewId(1)).is_none());
    }

    #[test]
    fn stale_resolve_cannot_replace_current_selection() {
        let mut engine = CompletionEngine::default();
        let request = start(&mut engine, seed(1, "a"), vec![source("words")]);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            1,
            vec![item("alpha"), item("amber")],
            true,
        )));
        let resolve = engine.transition(CompletionEvent::ResolveSelected { view: ViewId(1) });
        let CompletionEffect::RequestResolve(key) = resolve[0].clone() else {
            panic!("selected candidate should request resolve");
        };
        engine.transition(CompletionEvent::MoveSelection {
            view: ViewId(1),
            movement: SelectionMove::Next,
        });
        let effects = engine.transition(CompletionEvent::InstallResolved {
            key,
            data: ResolveData {
                detail: Some("stale".into()),
                documentation: None,
            },
        });
        assert!(matches!(
            effects.as_slice(),
            [CompletionEffect::Ignored {
                reason: "stale resolve result",
                ..
            }]
        ));
    }

    #[test]
    fn acceptance_is_frozen_and_session_closes_only_after_commit() {
        let mut engine = CompletionEngine::default();
        let request = start(&mut engine, seed(1, "pr"), vec![source("words")]);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            1,
            vec![item("println")],
            true,
        )));
        let effects = engine.transition(CompletionEvent::AcceptSelected { view: ViewId(1) });
        let CompletionEffect::Accept(acceptance) = effects[0].clone() else {
            panic!("selected candidate should produce acceptance");
        };
        assert_eq!(&*acceptance.text, "println");
        assert_eq!(acceptance.range, request.range());
        assert!(engine.snapshot(ViewId(1)).is_some());
        engine.transition(CompletionEvent::AcceptanceCommitted {
            view: ViewId(1),
            task: acceptance.task,
            candidate: acceptance.candidate,
        });
        assert!(engine.snapshot(ViewId(1)).is_none());
    }

    #[test]
    fn oversized_batch_ends_only_its_source_without_partial_install() {
        let mut engine = CompletionEngine::new(CompletionLimits {
            max_source_items: 1,
            ..CompletionLimits::default()
        })
        .unwrap();
        let request = start(
            &mut engine,
            seed(1, ""),
            vec![source("bad"), source("good")],
        );
        let effects = engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("bad"),
            1,
            vec![item("alpha"), item("beta")],
            true,
        )));
        assert!(matches!(
            effects.as_slice(),
            [
                CompletionEffect::SourceFailed { .. },
                CompletionEffect::CancelSource(_),
                CompletionEffect::ViewChanged(_)
            ]
        ));
        let snapshot = engine.snapshot(ViewId(1)).unwrap();
        assert_eq!(snapshot.item_count, 0);
        assert!(snapshot.collecting);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("good"),
            1,
            vec![item("gamma")],
            true,
        )));
        assert!(!engine.snapshot(ViewId(1)).unwrap().collecting);
    }

    #[test]
    fn equal_version_replace_is_ignored_without_changing_identity() {
        let mut engine = CompletionEngine::default();
        let request = start(&mut engine, seed(1, "a"), vec![source("words")]);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            1,
            vec![item("alpha")],
            false,
        )));
        let before = engine.snapshot(ViewId(1)).unwrap();
        let effects = engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            1,
            vec![item("amber")],
            false,
        )));
        assert!(matches!(
            effects.as_slice(),
            [CompletionEffect::Ignored {
                reason: "out-of-order source batch",
                ..
            }]
        ));
        let after = engine.snapshot(ViewId(1)).unwrap();
        assert_eq!(before.candidates, after.candidates);
        assert_eq!(before.selected, after.selected);
    }

    #[test]
    fn invalid_start_limits_do_not_replace_the_current_session() {
        let mut engine = CompletionEngine::new(CompletionLimits {
            max_context_bytes: 128,
            max_sources: 1,
            ..CompletionLimits::default()
        })
        .unwrap();
        let current = start(&mut engine, seed(1, "a"), vec![source("words")]);
        let oversized = CompletionRequestSeed::new(
            ViewId(1),
            ContentId(7),
            Revision(4),
            Revision(3),
            Selection::collapsed(TextOffset::origin()),
            CompletionTextRange::new(TextOffset::origin(), TextOffset::origin()).unwrap(),
            "",
            CompletionTrigger::Manual,
            CompletionRequestContext::new(
                Some("rust"),
                "main.rs",
                Some("src/main.rs"),
                "a context deliberately larger than the configured bound",
                "",
            ),
        )
        .unwrap();
        let effects = engine.transition(CompletionEvent::Trigger {
            request: oversized,
            sources: vec![source("words")],
        });
        assert!(matches!(
            effects.as_slice(),
            [CompletionEffect::Rejected {
                reason: "request context limit exceeded",
                ..
            }]
        ));
        assert_eq!(engine.snapshot(ViewId(1)).unwrap().request, current);

        let effects = engine.transition(CompletionEvent::Trigger {
            request: seed(1, "a"),
            sources: vec![source("first"), source("second")],
        });
        assert!(matches!(
            effects.as_slice(),
            [CompletionEffect::Rejected {
                reason: "source limit exceeded",
                ..
            }]
        ));
        assert_eq!(engine.snapshot(ViewId(1)).unwrap().request, current);
    }

    #[test]
    fn invalid_resolve_faults_only_its_source_with_a_keyed_effect() {
        let mut engine = CompletionEngine::new(CompletionLimits {
            max_string_bytes: 16,
            ..CompletionLimits::default()
        })
        .unwrap();
        let request = start(&mut engine, seed(1, "a"), vec![source("words")]);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            1,
            vec![item("alpha")],
            true,
        )));
        let resolve = engine.transition(CompletionEvent::ResolveSelected { view: ViewId(1) });
        let CompletionEffect::RequestResolve(key) = resolve[0].clone() else {
            panic!("selected candidate should request resolve");
        };
        let effects = engine.transition(CompletionEvent::InstallResolved {
            key,
            data: ResolveData {
                detail: Some("provider detail larger than bound".into()),
                documentation: None,
            },
        });
        assert!(matches!(
            effects.as_slice(),
            [
                CompletionEffect::SourceFailed {
                    key,
                    reason: "resolve string limit exceeded",
                },
                CompletionEffect::CancelSource(cancelled),
                CompletionEffect::ViewChanged(ViewId(1)),
            ] if key == cancelled && key.source().as_str() == "words"
        ));
        let retry = engine.transition(CompletionEvent::ResolveSelected { view: ViewId(1) });
        assert!(matches!(
            retry.as_slice(),
            [CompletionEffect::Ignored {
                reason: "selected source unavailable for resolve",
                ..
            }]
        ));
    }

    #[test]
    fn late_resolve_is_ignored_after_source_timeout() {
        let mut engine = CompletionEngine::default();
        let request = start(&mut engine, seed(1, "a"), vec![source("slow")]);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("slow"),
            1,
            vec![item("alpha")],
            false,
        )));
        let resolve = engine.transition(CompletionEvent::ResolveSelected { view: ViewId(1) });
        let CompletionEffect::RequestResolve(key) = resolve[0].clone() else {
            panic!("selected candidate should request resolve");
        };
        engine.transition(CompletionEvent::SourceTimedOut(
            request.source_key(source("slow")),
        ));
        let effects = engine.transition(CompletionEvent::InstallResolved {
            key,
            data: ResolveData {
                detail: Some("late".into()),
                documentation: None,
            },
        });
        assert!(matches!(
            effects.as_slice(),
            [CompletionEffect::Ignored {
                reason: "stale resolve result",
                ..
            }]
        ));
        assert!(engine.snapshot(ViewId(1)).unwrap().resolved.is_none());
    }

    #[test]
    fn engine_rejects_unrepresentable_limits() {
        let result = CompletionEngine::new(CompletionLimits {
            visible_items: usize::MAX,
            max_session_items: usize::MAX,
            max_session_bytes: usize::MAX,
            max_batch_bytes: usize::MAX,
            max_query_bytes: usize::MAX,
            max_context_bytes: usize::MAX,
            ..CompletionLimits::default()
        });
        assert_eq!(
            result.err().unwrap().reason(),
            "completion limits are inconsistent"
        );
    }

    #[test]
    fn retained_batch_storage_has_exact_capacity() {
        let mut engine = CompletionEngine::new(CompletionLimits {
            max_source_items: 10,
            max_batch_bytes: 512 * 1024,
            max_session_bytes: 1024 * 1024,
            max_string_bytes: 64 * 1024,
            max_query_bytes: 1024,
            max_context_bytes: 64 * 1024,
            ..CompletionLimits::default()
        })
        .unwrap();
        let request = start(&mut engine, seed(1, "a"), vec![source("words")]);
        let mut items = Vec::with_capacity(100_000);
        items.push(item("alpha"));
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            1,
            items,
            true,
        )));
        let snapshot = engine.snapshot(ViewId(1)).unwrap();
        assert_eq!(snapshot.item_count, 1);
        assert!(snapshot.estimated_heap_bytes < 1024 * 1024);
    }

    #[test]
    fn batch_limit_includes_an_existing_resolve_payload() {
        let mut engine = CompletionEngine::new(CompletionLimits {
            max_batch_bytes: 31 * 1024,
            max_session_bytes: 32 * 1024,
            max_string_bytes: 31 * 1024,
            max_query_bytes: 1024,
            max_context_bytes: 1024,
            ..CompletionLimits::default()
        })
        .unwrap();
        let request = start(&mut engine, seed(1, "a"), vec![source("words")]);
        engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            1,
            vec![item("alpha")],
            false,
        )));
        let resolve = engine.transition(CompletionEvent::ResolveSelected { view: ViewId(1) });
        let CompletionEffect::RequestResolve(key) = resolve[0].clone() else {
            panic!("selected candidate should request resolve");
        };
        let resolved = engine.transition(CompletionEvent::InstallResolved {
            key,
            data: ResolveData {
                detail: Some("d".repeat(16 * 1024).into()),
                documentation: None,
            },
        });
        assert!(matches!(
            resolved.as_slice(),
            [CompletionEffect::ViewChanged(ViewId(1))]
        ));
        let large = format!("a{}", "x".repeat(10 * 1024));
        let effects = engine.transition(CompletionEvent::InstallBatch(replace(
            &request,
            &source("words"),
            2,
            vec![CompletionItem::new(large.clone(), large)],
            true,
        )));
        assert!(matches!(
            effects.first(),
            Some(CompletionEffect::SourceFailed {
                reason: "session limit exceeded",
                ..
            })
        ));
    }
}
