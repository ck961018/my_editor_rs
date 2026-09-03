use std::fmt;
use std::mem;
use std::sync::Arc;

use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::{Selection, TextOffset};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CompletionSourceKey(Arc<str>);

impl CompletionSourceKey {
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CompletionSourceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&str> for CompletionSourceKey {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for CompletionSourceKey {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CompletionSessionId(pub(crate) u64);

impl CompletionSessionId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestEpoch(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceBatchVersion(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CandidateId {
    pub source: CompletionSourceKey,
    pub batch: SourceBatchVersion,
    pub ordinal: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionTextRange {
    start: TextOffset,
    end: TextOffset,
}

impl CompletionTextRange {
    pub fn new(start: TextOffset, end: TextOffset) -> Option<Self> {
        (start.char_index <= end.char_index).then_some(Self { start, end })
    }

    pub fn char_len(self) -> usize {
        self.end.char_index - self.start.char_index
    }

    pub fn start(self) -> TextOffset {
        self.start
    }

    pub fn end(self) -> TextOffset {
        self.end
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionTrigger {
    Manual,
    Identifier,
    Delete,
    Character(char),
    Incomplete,
    Refresh,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionRequestContext {
    language: Option<Arc<str>>,
    resource_name: Arc<str>,
    resource_path: Option<Arc<str>>,
    before_cursor: Arc<str>,
    after_cursor: Arc<str>,
}

impl CompletionRequestContext {
    pub fn new(
        language: Option<impl Into<Arc<str>>>,
        resource_name: impl Into<Arc<str>>,
        resource_path: Option<impl Into<Arc<str>>>,
        before_cursor: impl Into<Arc<str>>,
        after_cursor: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            language: language.map(Into::into),
            resource_name: resource_name.into(),
            resource_path: resource_path.map(Into::into),
            before_cursor: before_cursor.into(),
            after_cursor: after_cursor.into(),
        }
    }

    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    pub fn resource_name(&self) -> &str {
        &self.resource_name
    }

    pub fn resource_path(&self) -> Option<&str> {
        self.resource_path.as_deref()
    }

    pub fn before_cursor(&self) -> &str {
        &self.before_cursor
    }

    pub fn after_cursor(&self) -> &str {
        &self.after_cursor
    }

    pub(crate) fn estimated_heap_bytes(&self) -> usize {
        self.language
            .as_ref()
            .map_or(0, estimated_arc_bytes)
            .saturating_add(estimated_arc_bytes(&self.resource_name))
            .saturating_add(self.resource_path.as_ref().map_or(0, estimated_arc_bytes))
            .saturating_add(estimated_arc_bytes(&self.before_cursor))
            .saturating_add(estimated_arc_bytes(&self.after_cursor))
    }

    pub(crate) fn strings(&self) -> impl Iterator<Item = &str> {
        self.language
            .iter()
            .chain(self.resource_path.iter())
            .map(AsRef::as_ref)
            .chain([
                self.resource_name.as_ref(),
                self.before_cursor.as_ref(),
                self.after_cursor.as_ref(),
            ])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionRequestSeed {
    view: ViewId,
    content: ContentId,
    content_revision: Revision,
    view_revision: Revision,
    selection: Selection,
    range: CompletionTextRange,
    query: Arc<str>,
    trigger: CompletionTrigger,
    context: CompletionRequestContext,
}

impl CompletionRequestSeed {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        view: ViewId,
        content: ContentId,
        content_revision: Revision,
        view_revision: Revision,
        selection: Selection,
        range: CompletionTextRange,
        query: impl Into<Arc<str>>,
        trigger: CompletionTrigger,
        context: CompletionRequestContext,
    ) -> Option<Self> {
        let query = query.into();
        let valid = selection.is_empty()
            && selection.head == range.end
            && query.chars().count() == range.char_len();
        valid.then_some(Self {
            view,
            content,
            content_revision,
            view_revision,
            selection,
            range,
            query,
            trigger,
            context,
        })
    }

    pub fn view(&self) -> ViewId {
        self.view
    }

    pub fn content(&self) -> ContentId {
        self.content
    }

    pub fn content_revision(&self) -> Revision {
        self.content_revision
    }

    pub fn view_revision(&self) -> Revision {
        self.view_revision
    }

    pub fn selection(&self) -> Selection {
        self.selection
    }

    pub fn range(&self) -> CompletionTextRange {
        self.range
    }

    pub fn trigger(&self) -> &CompletionTrigger {
        &self.trigger
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn context(&self) -> &CompletionRequestContext {
        &self.context
    }

    fn estimated_heap_bytes(&self) -> usize {
        mem::size_of::<Self>()
            + estimated_arc_bytes(&self.query)
            + self.context.estimated_heap_bytes()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionRequest {
    session: CompletionSessionId,
    epoch: RequestEpoch,
    seed: CompletionRequestSeed,
}

impl CompletionRequest {
    pub fn session(&self) -> CompletionSessionId {
        self.session
    }

    pub fn view(&self) -> ViewId {
        self.seed.view
    }

    pub fn content(&self) -> ContentId {
        self.seed.content
    }

    pub fn content_revision(&self) -> Revision {
        self.seed.content_revision
    }

    pub fn view_revision(&self) -> Revision {
        self.seed.view_revision
    }

    pub fn selection(&self) -> Selection {
        self.seed.selection
    }

    pub fn range(&self) -> CompletionTextRange {
        self.seed.range
    }

    pub fn query(&self) -> &str {
        &self.seed.query
    }

    /// Creates a request-scoped probe using the engine's matching policy.
    ///
    /// The probe lets a streaming source avoid publishing an invisible first
    /// batch. It exposes neither matcher configuration nor ranking; the engine
    /// still performs authoritative work when it installs the batch.
    pub fn preview_probe(&self) -> crate::CompletionPreviewProbe {
        crate::CompletionPreviewProbe::new(self.query())
    }

    pub fn trigger(&self) -> &CompletionTrigger {
        &self.seed.trigger
    }

    pub fn epoch(&self) -> RequestEpoch {
        self.epoch
    }

    pub fn context(&self) -> &CompletionRequestContext {
        &self.seed.context
    }

    pub fn task_key(&self) -> CompletionTaskKey {
        CompletionTaskKey {
            session: self.session,
            epoch: self.epoch(),
        }
    }

    pub fn source_key(&self, source: CompletionSourceKey) -> SourceRequestKey {
        SourceRequestKey {
            view: self.view(),
            task: self.task_key(),
            source,
        }
    }

    pub(crate) fn from_seed(
        session: CompletionSessionId,
        epoch: RequestEpoch,
        seed: CompletionRequestSeed,
    ) -> Self {
        Self {
            session,
            epoch,
            seed,
        }
    }

    pub(crate) fn estimated_heap_bytes(&self) -> usize {
        self.seed.estimated_heap_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompletionTaskKey {
    pub session: CompletionSessionId,
    pub epoch: RequestEpoch,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceRequestKey {
    pub(crate) view: ViewId,
    pub(crate) task: CompletionTaskKey,
    pub(crate) source: CompletionSourceKey,
}

impl SourceRequestKey {
    pub fn view(&self) -> ViewId {
        self.view
    }

    pub fn task(&self) -> CompletionTaskKey {
        self.task
    }

    pub fn source(&self) -> &CompletionSourceKey {
        &self.source
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResolveRequestKey {
    pub task: CompletionTaskKey,
    pub view: ViewId,
    pub candidate: CandidateId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolveData {
    pub detail: Option<Arc<str>>,
    pub documentation: Option<Arc<str>>,
}

impl ResolveData {
    pub(crate) fn estimated_heap_bytes(&self) -> usize {
        self.detail
            .as_ref()
            .map_or(0, estimated_arc_bytes)
            .saturating_add(self.documentation.as_ref().map_or(0, estimated_arc_bytes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: Arc<str>,
    pub filter_text: Option<Arc<str>>,
    pub sort_text: Option<Arc<str>>,
    pub insert_text: Arc<str>,
    pub insert_range: Option<CompletionTextRange>,
    pub source_bias: i32,
    pub detail: Option<Arc<str>>,
    pub kind: Option<Arc<str>>,
    pub group: Option<Arc<str>>,
    pub deprecated: bool,
}

impl CompletionItem {
    pub fn new(label: impl Into<Arc<str>>, insert_text: impl Into<Arc<str>>) -> Self {
        Self {
            label: label.into(),
            filter_text: None,
            sort_text: None,
            insert_text: insert_text.into(),
            insert_range: None,
            source_bias: 0,
            detail: None,
            kind: None,
            group: None,
            deprecated: false,
        }
    }

    pub fn with_filter_text(mut self, filter_text: impl Into<Arc<str>>) -> Self {
        self.filter_text = Some(filter_text.into());
        self
    }

    pub fn with_sort_text(mut self, sort_text: impl Into<Arc<str>>) -> Self {
        self.sort_text = Some(sort_text.into());
        self
    }

    pub fn with_insert_range(mut self, range: CompletionTextRange) -> Self {
        self.insert_range = Some(range);
        self
    }

    pub fn with_source_bias(mut self, source_bias: i32) -> Self {
        self.source_bias = source_bias;
        self
    }

    pub fn with_detail(mut self, detail: impl Into<Arc<str>>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_kind(mut self, kind: impl Into<Arc<str>>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    pub fn with_group(mut self, group: impl Into<Arc<str>>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn deprecated(mut self, deprecated: bool) -> Self {
        self.deprecated = deprecated;
        self
    }

    pub fn estimated_heap_bytes(&self) -> usize {
        mem::size_of::<Self>()
            + estimated_arc_bytes(&self.label)
            + self.filter_text.as_ref().map_or(0, estimated_arc_bytes)
            + self.sort_text.as_ref().map_or(0, estimated_arc_bytes)
            + estimated_arc_bytes(&self.insert_text)
            + self.detail.as_ref().map_or(0, estimated_arc_bytes)
            + self.kind.as_ref().map_or(0, estimated_arc_bytes)
            + self.group.as_ref().map_or(0, estimated_arc_bytes)
    }

    pub(crate) fn strings(&self) -> impl Iterator<Item = &str> {
        self.filter_text
            .iter()
            .chain(self.sort_text.iter())
            .chain(self.detail.iter())
            .chain(self.kind.iter())
            .chain(self.group.iter())
            .map(AsRef::as_ref)
            .chain([self.label.as_ref(), self.insert_text.as_ref()])
    }
}

fn estimated_arc_bytes(value: &Arc<str>) -> usize {
    // Arc's allocation also stores the strong and weak counters. Counting the
    // payload every time is deliberately conservative when providers reuse an
    // Arc across item fields.
    mem::size_of::<usize>() * 2 + value.len()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IncompleteDirections {
    pub forward: bool,
    pub backward: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionBatchKind {
    Replace,
    Append { sequence: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionBatch {
    pub key: SourceRequestKey,
    pub version: SourceBatchVersion,
    pub kind: CompletionBatchKind,
    pub items: Vec<CompletionItem>,
    pub is_final: bool,
    pub incomplete: IncompleteDirections,
}

impl CompletionBatch {
    pub fn replace(
        key: SourceRequestKey,
        version: SourceBatchVersion,
        items: Vec<CompletionItem>,
        is_final: bool,
        incomplete: IncompleteDirections,
    ) -> Self {
        Self {
            key,
            version,
            kind: CompletionBatchKind::Replace,
            items,
            is_final,
            incomplete,
        }
    }

    pub fn append(
        key: SourceRequestKey,
        version: SourceBatchVersion,
        sequence: u32,
        items: Vec<CompletionItem>,
        is_final: bool,
        incomplete: IncompleteDirections,
    ) -> Self {
        Self {
            key,
            version,
            kind: CompletionBatchKind::Append { sequence },
            items,
            is_final,
            incomplete,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMove {
    Next,
    Previous,
    First,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionLimits {
    pub max_sources: usize,
    pub max_source_key_bytes: usize,
    pub max_source_items: usize,
    pub max_source_batches: usize,
    pub max_batch_bytes: usize,
    pub max_session_items: usize,
    pub max_session_bytes: usize,
    pub max_string_bytes: usize,
    pub max_query_bytes: usize,
    pub max_context_bytes: usize,
    pub visible_items: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionConfigError {
    reason: &'static str,
}

impl CompletionConfigError {
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for CompletionConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for CompletionConfigError {}

impl Default for CompletionLimits {
    fn default() -> Self {
        Self {
            max_sources: 64,
            max_source_key_bytes: 256,
            max_source_items: 100_000,
            max_source_batches: 1_024,
            max_batch_bytes: 48 * 1024 * 1024,
            max_session_items: 100_000,
            max_session_bytes: 64 * 1024 * 1024,
            max_string_bytes: 1024 * 1024,
            max_query_bytes: 16 * 1024,
            max_context_bytes: 64 * 1024,
            visible_items: 100,
        }
    }
}

impl CompletionLimits {
    pub(crate) fn validate(self) -> Result<Self, CompletionConfigError> {
        let nonzero = self.max_sources > 0
            && self.max_source_key_bytes > 0
            && self.max_source_items > 0
            && self.max_source_batches > 0
            && self.max_batch_bytes > 0
            && self.max_session_items > 0
            && self.max_session_bytes > 0
            && self.max_string_bytes > 0
            && self.max_query_bytes > 0
            && self.max_context_bytes > 0
            && self.visible_items > 0;
        if !nonzero {
            return Err(CompletionConfigError {
                reason: "completion limits must be nonzero",
            });
        }
        if self.max_batch_bytes > self.max_session_bytes
            || self.max_string_bytes > self.max_session_bytes
            || self.max_query_bytes > self.max_session_bytes
            || self.max_context_bytes > self.max_session_bytes
            || self.visible_items > self.max_session_items
            || self.visible_items > 10_000
        {
            return Err(CompletionConfigError {
                reason: "completion limits are inconsistent",
            });
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionEvent {
    Trigger {
        request: CompletionRequestSeed,
        sources: Vec<CompletionSourceKey>,
    },
    InstallBatch(CompletionBatch),
    SourceCompleted(SourceRequestKey),
    SourceFailed(SourceRequestKey),
    SourceTimedOut(SourceRequestKey),
    MoveSelection {
        view: ViewId,
        movement: SelectionMove,
    },
    ResolveSelected {
        view: ViewId,
    },
    InstallResolved {
        key: ResolveRequestKey,
        data: ResolveData,
    },
    AcceptSelected {
        view: ViewId,
    },
    AcceptanceCommitted {
        view: ViewId,
        task: CompletionTaskKey,
        candidate: CandidateId,
    },
    CancelView {
        view: ViewId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionAcceptance {
    pub task: CompletionTaskKey,
    pub view: ViewId,
    pub content: ContentId,
    pub content_revision: Revision,
    pub view_revision: Revision,
    pub selection: Selection,
    pub candidate: CandidateId,
    pub range: CompletionTextRange,
    pub text: Arc<str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionInteractionState {
    pub content: ContentId,
    pub has_selection: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionSelectionChange {
    pub view: ViewId,
    pub candidate: Option<CandidateId>,
    pub visible_index: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionEffect {
    RequestSource {
        key: SourceRequestKey,
        request: CompletionRequest,
    },
    CancelSource(SourceRequestKey),
    SourceFailed {
        key: SourceRequestKey,
        reason: &'static str,
    },
    RequestResolve(ResolveRequestKey),
    Accept(CompletionAcceptance),
    SelectionChanged(CompletionSelectionChange),
    ViewChanged(ViewId),
    Ignored {
        view: Option<ViewId>,
        reason: &'static str,
    },
    Rejected {
        view: Option<ViewId>,
        reason: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionCandidate {
    pub id: CandidateId,
    pub label: Arc<str>,
    pub detail: Option<Arc<str>>,
    pub kind: Option<Arc<str>>,
    pub group: Option<Arc<str>>,
    pub deprecated: bool,
    pub match_positions: Arc<[usize]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionSnapshot {
    pub request: CompletionRequest,
    pub sources: Vec<CompletionSourceKey>,
    pub candidates: Vec<CompletionCandidate>,
    pub selected: Option<CandidateId>,
    pub collecting: bool,
    pub incomplete: bool,
    pub source_faults: usize,
    pub resolved: Option<(CandidateId, ResolveData)>,
    pub source_count: usize,
    pub item_count: usize,
    pub estimated_heap_bytes: usize,
}
