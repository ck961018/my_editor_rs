use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
#[cfg(feature = "test-support")]
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::command::{Command, ModeValue};
use crate::mode_name::{ModeActionName, ModeName};
use crate::operation::OperationRequest;
use crate::presentation::{ContentPresentationLayer, ViewPresentationLayer};
use vell_core::content::{ContentChange, ContentKind};
use vell_core::content_store::ContentStore;
use vell_core::content_view_state::{BufferViewState, ContentViewState, StatusBarViewState};
use vell_core::input::{InputDecision, InputStatus};
use vell_core::keymap::Keymap;
use vell_protocol::content_query::{
    BufferBackingState, ContentData, ContentQuery, CursorStyle, DirtyState, Face, FaceDefinition,
    FaceName, FacePatch, NamedTextDecoration, RowRange, SaveState, SelectionShape, TextMetrics,
    is_host_face_name,
};
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::key_event::KeyEvent;
use vell_protocol::revision::Revision;
use vell_protocol::selection::{Selections, TextOffset, TextPoint};

static EMPTY_KEYMAP: LazyLock<Keymap<Command>> = LazyLock::new(Keymap::new);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeId(u32);

impl ModeId {
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(id: u32) -> Self {
        Self(id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeActionId(u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeError {
    #[allow(dead_code, reason = "script adapters map callback failures")]
    CallbackFailed {
        mode: ModeName,
        message: String,
    },
    UnknownMode {
        mode: ModeName,
    },
    UnknownAction {
        mode: ModeName,
        action: ModeActionName,
    },
    InactiveMode {
        requested: ModeName,
        active: Option<ModeName>,
    },
    UnsupportedContent {
        mode: ModeName,
        kind: ContentKind,
    },
    InvalidViewContext(ModeContextError),
    StateTypeMismatch {
        mode: ModeName,
        state: ModeStateKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeFaultPhase {
    ContentState,
    ViewState,
    Input,
    Action,
    ContentChanged,
    BackgroundJob,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeFaultCategory {
    Callback,
    State,
    Contract,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeFault {
    pub mode: ModeName,
    pub phase: ModeFaultPhase,
    pub category: ModeFaultCategory,
    pub callback: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeStateKind {
    Content,
    View,
    JobOutput,
}

impl fmt::Display for ModeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CallbackFailed { mode, message } => {
                write!(
                    formatter,
                    "mode '{}' callback failed: {message}",
                    mode.as_str()
                )
            }
            Self::UnknownMode { mode } => write!(formatter, "unknown mode '{}'", mode.as_str()),
            Self::UnknownAction { mode, action } => write!(
                formatter,
                "unknown action '{}' for mode '{}'",
                action.as_str(),
                mode.as_str()
            ),
            Self::InactiveMode { requested, active } => match active {
                Some(active) => write!(
                    formatter,
                    "mode '{}' is not active; active mode is '{}'",
                    requested.as_str(),
                    active.as_str()
                ),
                None => write!(
                    formatter,
                    "mode '{}' cannot execute because the view has no active mode",
                    requested.as_str()
                ),
            },
            Self::UnsupportedContent { mode, kind } => write!(
                formatter,
                "mode '{}' has no {kind:?} adapter",
                mode.as_str()
            ),
            Self::InvalidViewContext(error) => error.fmt(formatter),
            Self::StateTypeMismatch { mode, state } => write!(
                formatter,
                "mode '{}' received an invalid {state:?} state type",
                mode.as_str()
            ),
        }
    }
}

impl std::error::Error for ModeError {}

impl ModeError {
    fn faults_instance(&self) -> bool {
        matches!(
            self,
            Self::CallbackFailed { .. } | Self::StateTypeMismatch { .. }
        )
    }
}

impl ModeFault {
    fn from_error(
        mode: &ModeName,
        phase: ModeFaultPhase,
        callback: impl Into<String>,
        error: &ModeError,
    ) -> Self {
        let category = match error {
            ModeError::CallbackFailed { .. } => ModeFaultCategory::Callback,
            ModeError::StateTypeMismatch { .. } => ModeFaultCategory::State,
            _ => ModeFaultCategory::Contract,
        };
        Self {
            mode: mode.clone(),
            phase,
            category,
            callback: callback.into(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for ModeFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "mode '{}' faulted during {}: {}",
            self.mode.as_str(),
            self.callback,
            self.message
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeContextError {
    MissingContent {
        view: ViewId,
        content: ContentId,
    },
    IncompatibleViewState {
        view: ViewId,
        content: ContentId,
        content_kind: ContentKind,
        state_kind: ContentKind,
    },
    UnboundStatusBar {
        view: ViewId,
        content: ContentId,
    },
}

impl fmt::Display for ModeContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContent { view, content } => write!(
                formatter,
                "view {} references missing content {}",
                view.0, content.0
            ),
            Self::IncompatibleViewState {
                view,
                content,
                content_kind,
                state_kind,
            } => write!(
                formatter,
                "view {} for content {} has {state_kind:?} state, expected {content_kind:?}",
                view.0, content.0
            ),
            Self::UnboundStatusBar { view, content } => write!(
                formatter,
                "status-bar view {} for content {} has no target",
                view.0, content.0
            ),
        }
    }
}

impl std::error::Error for ModeContextError {}

pub trait ModeState: Any {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn clone_box(&self) -> Box<dyn ModeState>;
    fn eq_box(&self, _other: &dyn ModeState) -> Option<bool> {
        None
    }
}

#[cfg(feature = "test-support")]
static MODE_STATE_CLONE_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "test-support")]
static MODE_STATE_CLONE_NANOS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "test-support")]
static MODE_STATE_CLONE_INLINE_BYTES: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "test-support")]
pub struct ModeStateCloneMetrics {
    pub count: u64,
    pub nanos: u64,
    pub inline_bytes: u64,
}

#[cfg(feature = "test-support")]
pub fn reset_mode_state_clone_metrics() {
    MODE_STATE_CLONE_COUNT.store(0, Ordering::Relaxed);
    MODE_STATE_CLONE_NANOS.store(0, Ordering::Relaxed);
    MODE_STATE_CLONE_INLINE_BYTES.store(0, Ordering::Relaxed);
}

#[cfg(feature = "test-support")]
pub fn mode_state_clone_metrics() -> ModeStateCloneMetrics {
    ModeStateCloneMetrics {
        count: MODE_STATE_CLONE_COUNT.load(Ordering::Relaxed),
        nanos: MODE_STATE_CLONE_NANOS.load(Ordering::Relaxed),
        inline_bytes: MODE_STATE_CLONE_INLINE_BYTES.load(Ordering::Relaxed),
    }
}

#[cfg(feature = "test-support")]
pub(crate) fn record_mode_state_clone<T: ?Sized>(started: Instant, state: &T) {
    let nanos = started.elapsed().as_nanos().min(u64::MAX.into()) as u64;
    let bytes = u64::try_from(std::mem::size_of_val(state)).unwrap_or(u64::MAX);
    MODE_STATE_CLONE_COUNT.fetch_add(1, Ordering::Relaxed);
    MODE_STATE_CLONE_NANOS.fetch_add(nanos, Ordering::Relaxed);
    MODE_STATE_CLONE_INLINE_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub type ModeJobResult = Result<Box<dyn Any + Send>, String>;
pub type ModeJobRunner = Box<dyn FnOnce(CancellationToken) -> ModeJobResult + Send>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModeJobSlot(Arc<str>);

impl ModeJobSlot {
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ModeJobSlot {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}

impl From<&str> for ModeJobSlot {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModeJobKey {
    pub mode: ModeId,
    pub content: ContentId,
    pub slot: ModeJobSlot,
}

pub struct ModeJobRequest {
    slot: ModeJobSlot,
    version: u64,
    run: ModeJobRunner,
}

impl ModeJobRequest {
    pub fn new(
        slot: impl Into<ModeJobSlot>,
        version: u64,
        run: impl FnOnce(CancellationToken) -> ModeJobResult + Send + 'static,
    ) -> Self {
        Self {
            slot: slot.into(),
            version,
            run: Box::new(run),
        }
    }

    pub fn into_parts(self) -> (ModeJobSlot, u64, ModeJobRunner) {
        (self.slot, self.version, self.run)
    }
}

impl<T: Any + Clone> ModeState for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn clone_box(&self) -> Box<dyn ModeState> {
        #[cfg(feature = "test-support")]
        let started = Instant::now();
        let cloned: Box<dyn ModeState> = Box::new(self.clone());
        #[cfg(feature = "test-support")]
        record_mode_state_clone(started, self);
        cloned
    }
}

#[derive(Default)]
pub struct ModeDraftJournal {
    content: HashMap<(ModeId, ContentId), ModeContentDraft>,
    views: HashMap<(ModeId, ViewId), ModeViewDraft>,
}

struct ModeContentDraft {
    state: Box<dyn ModeState>,
    fault: Option<ModeFault>,
    background_job_dirty: bool,
}

struct ModeViewDraft {
    state: Box<dyn ModeState>,
    fault: Option<ModeFault>,
}

impl ModeDraftJournal {
    fn content<'a>(
        &'a self,
        mode: ModeId,
        content: ContentId,
        persistent: &'a ModeContentInstance,
    ) -> (&'a dyn ModeState, bool) {
        self.content.get(&(mode, content)).map_or(
            (persistent.state.as_ref(), persistent.fault.is_some()),
            |draft| (draft.state.as_ref(), draft.fault.is_some()),
        )
    }

    fn content_mut<'a>(
        &'a mut self,
        mode: ModeId,
        content: ContentId,
        persistent: &ModeContentInstance,
    ) -> &'a mut ModeContentDraft {
        self.content
            .entry((mode, content))
            .or_insert_with(|| ModeContentDraft {
                state: persistent.state.clone_box(),
                fault: persistent.fault.clone(),
                background_job_dirty: persistent.background_job_dirty,
            })
    }

    fn view<'a>(
        &'a self,
        mode: ModeId,
        view: ViewId,
        persistent: &'a ModeViewInstance,
    ) -> (&'a dyn ModeState, bool) {
        self.views.get(&(mode, view)).map_or(
            (persistent.state.as_ref(), persistent.fault.is_some()),
            |draft| (draft.state.as_ref(), draft.fault.is_some()),
        )
    }

    fn content_and_view_mut<'a>(
        &'a mut self,
        mode: ModeId,
        content: ContentId,
        view: ViewId,
        persistent_content: &ModeContentInstance,
        persistent_view: &ModeViewInstance,
    ) -> (&'a mut ModeContentDraft, &'a mut ModeViewDraft) {
        let content_draft =
            self.content
                .entry((mode, content))
                .or_insert_with(|| ModeContentDraft {
                    state: persistent_content.state.clone_box(),
                    fault: persistent_content.fault.clone(),
                    background_job_dirty: persistent_content.background_job_dirty,
                });
        let view_draft = self
            .views
            .entry((mode, view))
            .or_insert_with(|| ModeViewDraft {
                state: persistent_view.state.clone_box(),
                fault: persistent_view.fault.clone(),
            });
        (content_draft, view_draft)
    }

    pub fn commit_content(&mut self, store: &mut ModeContentStore) {
        for (key, draft) in self.content.drain() {
            let instance = store
                .instances
                .get_mut(&key)
                .expect("drafted mode content still exists");
            if draft.state.eq_box(instance.state.as_ref()) == Some(true)
                && draft.fault == instance.fault
                && draft.background_job_dirty == instance.background_job_dirty
            {
                continue;
            }
            instance.state = draft.state;
            instance.fault = draft.fault;
            instance.background_job_dirty = draft.background_job_dirty;
            instance.revision.next();
        }
    }

    pub fn commit_views(&mut self, store: &mut ModeViewStore) {
        for (key, draft) in self.views.drain() {
            let instance = store
                .instances
                .get_mut(&key)
                .expect("drafted mode view still exists");
            if draft.state.eq_box(instance.state.as_ref()) == Some(true)
                && draft.fault == instance.fault
            {
                continue;
            }
            instance.state = draft.state;
            instance.fault = draft.fault;
            instance.revision.next();
        }
    }

    pub fn commit_faults(
        &mut self,
        content_store: &mut ModeContentStore,
        view_store: &mut ModeViewStore,
    ) {
        for (key, draft) in self.content.drain() {
            let Some(fault) = draft.fault else {
                continue;
            };
            let instance = content_store
                .instances
                .get_mut(&key)
                .expect("drafted mode content still exists");
            if instance.fault.as_ref() == Some(&fault) {
                continue;
            }
            instance.fault = Some(fault);
            instance.background_job_dirty = false;
            instance.revision.next();
        }
        for (key, draft) in self.views.drain() {
            let Some(fault) = draft.fault else {
                continue;
            };
            let instance = view_store
                .instances
                .get_mut(&key)
                .expect("drafted mode view still exists");
            if instance.fault.as_ref() == Some(&fault) {
                continue;
            }
            instance.fault = Some(fault);
            instance.revision.next();
        }
    }
}

#[derive(Default)]
pub struct FaceRegistry {
    faces: HashMap<FaceName, RegisteredFace>,
    conflicts: Vec<FaceConflict>,
    registration_errors: Vec<FaceRegistrationError>,
}

#[derive(Clone)]
struct RegisteredFace {
    definition: FaceDefinition,
    provider: Option<ModeName>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceConflict {
    pub face: FaceName,
    pub active_provider: Option<ModeName>,
    pub rejected_provider: ModeName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceRegistrationError {
    pub face: FaceName,
    pub rejected_provider: ModeName,
    pub reason: FaceRegistrationErrorReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceRegistrationErrorReason {
    HostNamespace,
    InheritanceCycle,
}

impl FaceRegistry {
    fn register_defaults(&mut self, mode: &dyn Mode) {
        self.register_definitions(mode.name().clone(), mode.face_definitions());
    }

    pub fn register_definitions(&mut self, provider: ModeName, definitions: Vec<FaceDefinition>) {
        let mut candidate = self.faces.clone();
        let mut inserted = Vec::new();
        for definition in definitions {
            let name = definition.name.clone();
            if is_host_face_name(&name) {
                if !self
                    .registration_errors
                    .iter()
                    .any(|error| error.face == name && error.rejected_provider == provider)
                {
                    self.registration_errors.push(FaceRegistrationError {
                        face: name,
                        rejected_provider: provider.clone(),
                        reason: FaceRegistrationErrorReason::HostNamespace,
                    });
                }
                continue;
            }
            if let Some(existing) = candidate.get(&name) {
                if existing.definition != definition
                    && !self.conflicts.iter().any(|conflict| {
                        conflict.face == name && conflict.rejected_provider == provider
                    })
                {
                    self.conflicts.push(FaceConflict {
                        face: name,
                        active_provider: existing.provider.clone(),
                        rejected_provider: provider.clone(),
                    });
                }
                continue;
            }
            candidate.insert(
                name.clone(),
                RegisteredFace {
                    definition,
                    provider: Some(provider.clone()),
                },
            );
            inserted.push(name);
        }
        let cyclic = inserted
            .iter()
            .filter(|name| face_inheritance_has_cycle(&candidate, name))
            .cloned()
            .collect::<Vec<_>>();
        if cyclic.is_empty() {
            self.faces = candidate;
        } else {
            for face in cyclic {
                if !self.registration_errors.iter().any(|error| {
                    error.face == face
                        && error.rejected_provider == provider
                        && error.reason == FaceRegistrationErrorReason::InheritanceCycle
                }) {
                    self.registration_errors.push(FaceRegistrationError {
                        face,
                        rejected_provider: provider.clone(),
                        reason: FaceRegistrationErrorReason::InheritanceCycle,
                    });
                }
            }
        }
    }

    pub fn resolve(&self, name: &FaceName) -> FacePatch {
        self.resolve_inner(name, &mut Vec::new())
    }

    fn resolve_inner(&self, name: &FaceName, visiting: &mut Vec<FaceName>) -> FacePatch {
        if visiting.contains(name) {
            return FacePatch::default();
        }
        let Some(registered) = self.faces.get(name) else {
            return FacePatch::default();
        };
        visiting.push(name.clone());
        let mut resolved = FacePatch::default();
        for parent in registered.definition.inherits.iter().rev() {
            resolved.overlay(&self.resolve_inner(parent, visiting));
        }
        visiting.pop();
        resolved.overlay(&registered.definition.fallback);
        resolved
    }

    pub fn provider(&self, name: &FaceName) -> Option<&ModeName> {
        self.faces.get(name)?.provider.as_ref()
    }

    pub fn definition(&self, name: &FaceName) -> Option<&FaceDefinition> {
        self.faces
            .get(name)
            .map(|registered| &registered.definition)
    }

    pub fn conflicts(&self) -> &[FaceConflict] {
        &self.conflicts
    }

    pub fn registration_errors(&self) -> &[FaceRegistrationError] {
        &self.registration_errors
    }

    #[allow(dead_code, reason = "theme and script adapters override named faces")]
    pub fn set(&mut self, name: FaceName, face: Face) {
        for conflict in &mut self.conflicts {
            if conflict.face == name {
                conflict.active_provider = None;
            }
        }
        let definition_name = name.clone();
        self.faces.insert(
            name,
            RegisteredFace {
                definition: FaceDefinition {
                    name: definition_name,
                    inherits: Vec::new(),
                    fallback: FacePatch::from(&face),
                },
                provider: None,
            },
        );
    }
}

fn face_inheritance_has_cycle(faces: &HashMap<FaceName, RegisteredFace>, start: &FaceName) -> bool {
    fn visit(
        faces: &HashMap<FaceName, RegisteredFace>,
        name: &FaceName,
        visiting: &mut Vec<FaceName>,
    ) -> bool {
        if visiting.contains(name) {
            return true;
        }
        let Some(face) = faces.get(name) else {
            return false;
        };
        visiting.push(name.clone());
        let cyclic = face
            .definition
            .inherits
            .iter()
            .any(|parent| visit(faces, parent, visiting));
        visiting.pop();
        cyclic
    }
    visit(faces, start, &mut Vec::new())
}

#[allow(
    dead_code,
    reason = "native Mode content contexts are used by extensions"
)]
pub enum ModeContentContext<'a> {
    Buffer(BufferModeContentContext<'a>),
    StatusBar(StatusBarModeContentContext<'a>),
}

#[allow(dead_code, reason = "native Mode adapter capability surface")]
pub struct BufferModeContentContext<'a> {
    content_id: ContentId,
    contents: &'a ContentStore,
}

#[allow(dead_code, reason = "native Mode adapter capability surface")]
pub struct StatusBarModeContentContext<'a> {
    content_id: ContentId,
    contents: &'a ContentStore,
}

#[allow(
    dead_code,
    reason = "native Mode content contexts are used by extensions"
)]
impl<'a> ModeContentContext<'a> {
    pub fn new(content_id: ContentId, contents: &'a ContentStore) -> Self {
        match contents
            .kind(content_id)
            .expect("mode content context references existing content")
        {
            ContentKind::Buffer => Self::Buffer(BufferModeContentContext {
                content_id,
                contents,
            }),
            ContentKind::StatusBar => Self::StatusBar(StatusBarModeContentContext {
                content_id,
                contents,
            }),
        }
    }

    pub fn content_id(&self) -> ContentId {
        match self {
            Self::Buffer(context) => context.content_id,
            Self::StatusBar(context) => context.content_id,
        }
    }

    pub fn content_kind(&self) -> ContentKind {
        match self {
            Self::Buffer(_) => ContentKind::Buffer,
            Self::StatusBar(_) => ContentKind::StatusBar,
        }
    }

    pub fn content_revision(&self) -> Option<Revision> {
        match self {
            Self::Buffer(context) => context.contents.revision(context.content_id),
            Self::StatusBar(context) => context.contents.revision(context.content_id),
        }
    }

    pub fn buffer(&self) -> Option<&BufferModeContentContext<'a>> {
        match self {
            Self::Buffer(context) => Some(context),
            Self::StatusBar(_) => None,
        }
    }

    pub fn status_bar(&self) -> Option<&StatusBarModeContentContext<'a>> {
        match self {
            Self::Buffer(_) => None,
            Self::StatusBar(context) => Some(context),
        }
    }
}

#[allow(dead_code, reason = "native Mode adapter capability surface")]
impl BufferModeContentContext<'_> {
    pub fn text_rows(&self, rows: RowRange) -> Option<Vec<String>> {
        match self
            .contents
            .query(self.content_id, ContentQuery::TextRows(rows))
        {
            ContentData::TextRows(rows) => Some(rows),
            _ => None,
        }
    }

    pub fn text_points(&self, offsets: Vec<TextOffset>) -> Option<Vec<TextPoint>> {
        match self
            .contents
            .query(self.content_id, ContentQuery::TextPoints(offsets))
        {
            ContentData::TextPoints(points) => Some(points),
            _ => None,
        }
    }

    pub fn resource_name(&self) -> Option<String> {
        match self
            .contents
            .query(self.content_id, ContentQuery::ResourceName)
        {
            ContentData::ResourceName(name) => name,
            _ => None,
        }
    }

    pub fn resource_path(&self) -> Option<String> {
        match self
            .contents
            .query(self.content_id, ContentQuery::ResourcePath)
        {
            ContentData::ResourcePath(path) => path,
            _ => None,
        }
    }

    pub fn backing_state(&self) -> Option<BufferBackingState> {
        match self
            .contents
            .query(self.content_id, ContentQuery::BackingState)
        {
            ContentData::BackingState(state) => Some(state),
            _ => None,
        }
    }

    pub fn dirty_state(&self) -> Option<DirtyState> {
        match self
            .contents
            .query(self.content_id, ContentQuery::DirtyState)
        {
            ContentData::DirtyState(state) => Some(state),
            _ => None,
        }
    }

    pub fn save_state(&self) -> Option<SaveState> {
        match self
            .contents
            .query(self.content_id, ContentQuery::SaveState)
        {
            ContentData::SaveState(state) => Some(state),
            _ => None,
        }
    }

    pub fn text_metrics(&self) -> Option<TextMetrics> {
        match self
            .contents
            .query(self.content_id, ContentQuery::TextMetrics)
        {
            ContentData::TextMetrics(metrics) => Some(metrics),
            _ => None,
        }
    }

    pub fn text_snapshot(&self) -> Option<vell_core::text_snapshot::TextSnapshot> {
        self.contents.text_snapshot(self.content_id)
    }
}

#[allow(dead_code, reason = "native Mode adapter capability surface")]
impl StatusBarModeContentContext<'_> {}

#[allow(dead_code, reason = "reserved for generic Mode extensions")]
pub enum ModeViewContext<'a> {
    Buffer(BufferModeViewContext<'a>),
    StatusBar(StatusBarModeViewContext<'a>),
}

#[allow(dead_code, reason = "native Mode adapter capability surface")]
pub struct BufferModeViewContext<'a> {
    view_id: ViewId,
    content_id: ContentId,
    state: &'a BufferViewState,
    contents: &'a ContentStore,
}

#[allow(dead_code, reason = "native Mode adapter capability surface")]
pub struct StatusBarModeViewContext<'a> {
    view_id: ViewId,
    content_id: ContentId,
    state: &'a StatusBarViewState,
    contents: &'a ContentStore,
}

#[allow(dead_code, reason = "reserved for generic Mode extensions")]
impl<'a> ModeViewContext<'a> {
    pub fn new(
        view_id: ViewId,
        content_id: ContentId,
        state: &'a ContentViewState,
        contents: &'a ContentStore,
    ) -> Result<Self, ModeContextError> {
        let content_kind = contents
            .kind(content_id)
            .ok_or(ModeContextError::MissingContent {
                view: view_id,
                content: content_id,
            })?;
        match (content_kind, state) {
            (ContentKind::Buffer, ContentViewState::Buffer(state)) => {
                Ok(Self::Buffer(BufferModeViewContext {
                    view_id,
                    content_id,
                    state,
                    contents,
                }))
            }
            (ContentKind::StatusBar, ContentViewState::StatusBar(state)) => {
                if state.target().is_none() {
                    return Err(ModeContextError::UnboundStatusBar {
                        view: view_id,
                        content: content_id,
                    });
                }
                Ok(Self::StatusBar(StatusBarModeViewContext {
                    view_id,
                    content_id,
                    state,
                    contents,
                }))
            }
            (_, state) => Err(ModeContextError::IncompatibleViewState {
                view: view_id,
                content: content_id,
                content_kind,
                state_kind: state.kind(),
            }),
        }
    }

    pub fn view_id(&self) -> ViewId {
        match self {
            Self::Buffer(context) => context.view_id,
            Self::StatusBar(context) => context.view_id,
        }
    }

    pub fn content_id(&self) -> ContentId {
        match self {
            Self::Buffer(context) => context.content_id,
            Self::StatusBar(context) => context.content_id,
        }
    }

    pub fn content_kind(&self) -> ContentKind {
        match self {
            Self::Buffer(_) => ContentKind::Buffer,
            Self::StatusBar(_) => ContentKind::StatusBar,
        }
    }

    pub fn content_revision(&self) -> Option<Revision> {
        match self {
            Self::Buffer(context) => context.contents.revision(context.content_id),
            Self::StatusBar(context) => context.contents.revision(context.content_id),
        }
    }

    pub fn buffer(&self) -> Option<&BufferModeViewContext<'a>> {
        match self {
            Self::Buffer(context) => Some(context),
            Self::StatusBar(_) => None,
        }
    }

    pub fn status_bar(&self) -> Option<&StatusBarModeViewContext<'a>> {
        match self {
            Self::Buffer(_) => None,
            Self::StatusBar(context) => Some(context),
        }
    }
}

#[allow(dead_code, reason = "native Mode adapter capability surface")]
impl BufferModeViewContext<'_> {
    pub fn selections(&self) -> &Selections {
        self.state.selections()
    }

    pub fn text_rows(&self, rows: RowRange) -> Option<Vec<String>> {
        match self
            .contents
            .query(self.content_id, ContentQuery::TextRows(rows))
        {
            ContentData::TextRows(rows) => Some(rows),
            _ => None,
        }
    }

    pub fn text_points(&self, offsets: Vec<TextOffset>) -> Option<Vec<TextPoint>> {
        match self
            .contents
            .query(self.content_id, ContentQuery::TextPoints(offsets))
        {
            ContentData::TextPoints(points) => Some(points),
            _ => None,
        }
    }

    pub fn resource_name(&self) -> Option<String> {
        match self
            .contents
            .query(self.content_id, ContentQuery::ResourceName)
        {
            ContentData::ResourceName(name) => name,
            _ => None,
        }
    }

    pub fn resource_path(&self) -> Option<String> {
        match self
            .contents
            .query(self.content_id, ContentQuery::ResourcePath)
        {
            ContentData::ResourcePath(path) => path,
            _ => None,
        }
    }

    pub fn backing_state(&self) -> Option<BufferBackingState> {
        match self
            .contents
            .query(self.content_id, ContentQuery::BackingState)
        {
            ContentData::BackingState(state) => Some(state),
            _ => None,
        }
    }

    pub fn dirty_state(&self) -> Option<DirtyState> {
        match self
            .contents
            .query(self.content_id, ContentQuery::DirtyState)
        {
            ContentData::DirtyState(state) => Some(state),
            _ => None,
        }
    }

    pub fn save_state(&self) -> Option<SaveState> {
        match self
            .contents
            .query(self.content_id, ContentQuery::SaveState)
        {
            ContentData::SaveState(state) => Some(state),
            _ => None,
        }
    }

    pub fn text_metrics(&self) -> Option<TextMetrics> {
        match self
            .contents
            .query(self.content_id, ContentQuery::TextMetrics)
        {
            ContentData::TextMetrics(metrics) => Some(metrics),
            _ => None,
        }
    }

    pub fn text_snapshot(&self) -> Option<vell_core::text_snapshot::TextSnapshot> {
        self.contents.text_snapshot(self.content_id)
    }
}

#[allow(dead_code, reason = "native Mode adapter capability surface")]
impl StatusBarModeViewContext<'_> {
    fn target(&self) -> (ViewId, ContentId) {
        self.state
            .target()
            .expect("status-bar mode context validates its target")
    }

    pub fn target_view_id(&self) -> ViewId {
        self.target().0
    }

    pub fn target_content_id(&self) -> ContentId {
        self.target().1
    }

    pub fn resource_name(&self) -> Option<String> {
        match self
            .contents
            .query(self.target().1, ContentQuery::ResourceName)
        {
            ContentData::ResourceName(name) => name,
            _ => None,
        }
    }

    pub fn resource_path(&self) -> Option<String> {
        match self
            .contents
            .query(self.target().1, ContentQuery::ResourcePath)
        {
            ContentData::ResourcePath(path) => path,
            _ => None,
        }
    }

    pub fn backing_state(&self) -> Option<BufferBackingState> {
        match self
            .contents
            .query(self.target().1, ContentQuery::BackingState)
        {
            ContentData::BackingState(state) => Some(state),
            _ => None,
        }
    }

    pub fn dirty_state(&self) -> Option<DirtyState> {
        match self
            .contents
            .query(self.target().1, ContentQuery::DirtyState)
        {
            ContentData::DirtyState(state) => Some(state),
            _ => None,
        }
    }

    pub fn save_state(&self) -> Option<SaveState> {
        match self
            .contents
            .query(self.target().1, ContentQuery::SaveState)
        {
            ContentData::SaveState(state) => Some(state),
            _ => None,
        }
    }

    pub fn text_metrics(&self) -> Option<TextMetrics> {
        match self
            .contents
            .query(self.target().1, ContentQuery::TextMetrics)
        {
            ContentData::TextMetrics(metrics) => Some(metrics),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeResult {
    flow: InputFlow,
    operations: Vec<OperationRequest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFlow {
    Continue,
    Stop,
}

impl ModeResult {
    #[allow(
        dead_code,
        reason = "empty typed results are part of the mode contract"
    )]
    pub fn none() -> Self {
        Self {
            flow: InputFlow::Stop,
            operations: Vec::new(),
        }
    }

    #[allow(dead_code, reason = "Mode results are an extension-facing API")]
    pub fn operations(operations: Vec<OperationRequest>) -> Self {
        Self {
            flow: InputFlow::Stop,
            operations,
        }
    }

    #[allow(dead_code, reason = "dynamic modes can pass input to the next mode")]
    pub fn continue_with(operations: Vec<OperationRequest>) -> Self {
        Self {
            flow: InputFlow::Continue,
            operations,
        }
    }

    fn into_operations(self) -> Vec<OperationRequest> {
        self.operations
    }

    pub fn into_parts(self) -> (InputFlow, Vec<OperationRequest>) {
        (self.flow, self.operations)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorDomain {
    InsertionPoint,
    Character,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModeViewPolicy {
    pub cursor_style: Option<CursorStyle>,
    pub cursor_domain: Option<CursorDomain>,
    pub selection_shape: Option<SelectionShape>,
    pub selection_face: Option<FaceName>,
    pub status_bar: Option<NamedStatusBarPresentation>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamedStatusBarSegment {
    pub text: String,
    pub face: Option<FaceName>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamedStatusBarPresentation {
    pub left: Vec<NamedStatusBarSegment>,
    pub center: Vec<NamedStatusBarSegment>,
    pub right: Vec<NamedStatusBarSegment>,
}

impl ModeViewPolicy {
    pub fn merge_missing(&mut self, next: Self) {
        self.cursor_style = self.cursor_style.or(next.cursor_style);
        self.cursor_domain = self.cursor_domain.or(next.cursor_domain);
        self.selection_shape = self.selection_shape.or(next.selection_shape);
        self.selection_face = self.selection_face.take().or(next.selection_face);
        self.status_bar = self.status_bar.take().or(next.status_bar);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeActionScope {
    #[allow(dead_code, reason = "content-scoped modes are an extension contract")]
    Content,
    View,
}

#[derive(Clone, Copy)]
pub enum ModeAdapter<'a> {
    Buffer(&'a dyn Mode),
    StatusBar(&'a dyn Mode),
}

impl<'a> ModeAdapter<'a> {
    fn behavior(self) -> &'a dyn Mode {
        match self {
            Self::Buffer(mode) | Self::StatusBar(mode) => mode,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct ModeAdapters {
    buffer: bool,
    status_bar: bool,
}

impl ModeAdapters {
    pub fn buffer() -> Self {
        Self {
            buffer: true,
            status_bar: false,
        }
    }

    #[allow(
        dead_code,
        reason = "native modes may adapt multiple closed content kinds"
    )]
    pub fn status_bar() -> Self {
        Self {
            buffer: false,
            status_bar: true,
        }
    }

    #[allow(
        dead_code,
        reason = "native modes may adapt multiple closed content kinds"
    )]
    pub fn buffer_and_status_bar() -> Self {
        Self {
            buffer: true,
            status_bar: true,
        }
    }

    pub fn contains(self, kind: ContentKind) -> bool {
        match kind {
            ContentKind::Buffer => self.buffer,
            ContentKind::StatusBar => self.status_bar,
        }
    }

    fn is_empty(self) -> bool {
        !self.buffer && !self.status_bar
    }
}

pub trait Mode {
    fn name(&self) -> &ModeName;
    fn actions(&self) -> &[ModeActionName];
    fn adapters(&self) -> ModeAdapters;
    fn before(&self) -> Option<&ModeName> {
        None
    }
    fn faces(&self) -> Vec<(FaceName, Face)> {
        Vec::new()
    }
    fn face_definitions(&self) -> Vec<FaceDefinition> {
        self.faces()
            .into_iter()
            .map(|(name, face)| FaceDefinition {
                name,
                inherits: Vec::new(),
                fallback: FacePatch::from(&face),
            })
            .collect()
    }
    fn action_scope(&self, _action: &ModeActionName) -> ModeActionScope {
        ModeActionScope::View
    }
    fn create_content_state(
        &self,
        _context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Ok(Box::new(()))
    }
    fn create_view_state(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Ok(Box::new(()))
    }
    fn execute_content_with_arguments(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name().clone(),
            action: action.clone(),
        })
    }
    fn on_content_changed(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        _change: &ContentChange,
    ) -> Result<(), ModeError> {
        Ok(())
    }
    fn take_background_jobs(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
    ) -> Vec<ModeJobRequest> {
        Vec::new()
    }
    fn apply_background_job(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        _slot: &ModeJobSlot,
        _version: u64,
        _result: ModeJobResult,
    ) -> Result<bool, ModeError> {
        Ok(false)
    }
    fn on_view_content_changed(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        _change: &ContentChange,
    ) -> Result<(), ModeError> {
        Ok(())
    }
    fn content_decorations(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeContentContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        Vec::new()
    }
    fn view_decorations(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        Vec::new()
    }
    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        ModeViewPolicy::default()
    }
    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        &EMPTY_KEYMAP
    }
    fn input_typing(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }
    fn execute_input(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name().clone(),
            action: ModeActionName::new("<input>"),
        })
    }
    fn mode_input_status(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> InputStatus {
        InputStatus::Ready
    }
    fn input_capture(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> InputDecision<Command> {
        InputDecision::Pass
    }
    fn input_timeout(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeResult {
        ModeResult::none()
    }
    fn input_cancel(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) {
    }
    fn execute_view_with_arguments(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name().clone(),
            action: action.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeRegistrationError {
    DuplicateMode(ModeName),
    MissingAdapter(ModeName),
    DuplicateAction {
        mode: ModeName,
        action: ModeActionName,
    },
}

impl fmt::Display for ModeRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateMode(mode) => {
                write!(formatter, "mode '{}' is already registered", mode.as_str())
            }
            Self::MissingAdapter(mode) => {
                write!(
                    formatter,
                    "mode '{}' defines no content adapter",
                    mode.as_str()
                )
            }
            Self::DuplicateAction { mode, action } => write!(
                formatter,
                "mode '{}' defines action '{}' more than once",
                mode.as_str(),
                action.as_str()
            ),
        }
    }
}

impl std::error::Error for ModeRegistrationError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeAttachmentError {
    UnknownContent(ContentId),
    UnknownMode(ModeName),
    InvalidViewContext(ModeContextError),
    UnsupportedContent {
        mode: ModeName,
        content: ContentId,
        kind: ContentKind,
    },
}

impl fmt::Display for ModeAttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownContent(content) => {
                write!(formatter, "content {} does not exist", content.0)
            }
            Self::UnknownMode(mode) => write!(formatter, "unknown mode '{}'", mode.as_str()),
            Self::InvalidViewContext(error) => error.fmt(formatter),
            Self::UnsupportedContent {
                mode,
                content,
                kind,
            } => write!(
                formatter,
                "mode '{}' has no {kind:?} adapter for content {}",
                mode.as_str(),
                content.0
            ),
        }
    }
}

impl std::error::Error for ModeAttachmentError {}

impl From<ModeContextError> for ModeAttachmentError {
    fn from(error: ModeContextError) -> Self {
        Self::InvalidViewContext(error)
    }
}

pub struct ModeRegistry {
    definitions: HashMap<ModeId, Rc<ModeRegistration>>,
    ids_by_name: HashMap<ModeName, ModeId>,
    next_id: u32,
}

struct ModeRegistration {
    id: ModeId,
    definition: Rc<dyn Mode>,
    adapters: RegisteredModeAdapters,
    action_names: Vec<ModeActionName>,
    actions: HashMap<ModeActionName, ModeActionId>,
}

struct RegisteredModeAdapters {
    buffer: Option<Rc<dyn Mode>>,
    status_bar: Option<Rc<dyn Mode>>,
}

pub struct ModeViewInstance {
    registered: Rc<ModeRegistration>,
    adapter_kind: ContentKind,
    state: Box<dyn ModeState>,
    fault: Option<ModeFault>,
    revision: Revision,
}

impl ModeRegistry {
    pub fn new() -> Self {
        Self {
            definitions: HashMap::new(),
            ids_by_name: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn register(&mut self, mode: impl Mode + 'static) -> Result<ModeId, ModeRegistrationError> {
        self.register_boxed(Box::new(mode))
    }

    pub fn register_boxed(&mut self, mode: Box<dyn Mode>) -> Result<ModeId, ModeRegistrationError> {
        let name = mode.name().clone();
        let actions = mode.actions().to_vec();
        self.register_definition(name, actions, mode)
    }

    fn register_definition(
        &mut self,
        name: ModeName,
        action_names: Vec<ModeActionName>,
        definition: Box<dyn Mode>,
    ) -> Result<ModeId, ModeRegistrationError> {
        if self.ids_by_name.contains_key(&name) {
            return Err(ModeRegistrationError::DuplicateMode(name));
        }
        let declared_adapters = definition.adapters();
        if declared_adapters.is_empty() {
            return Err(ModeRegistrationError::MissingAdapter(name));
        }
        let has_buffer = declared_adapters.contains(ContentKind::Buffer);
        let has_status_bar = declared_adapters.contains(ContentKind::StatusBar);
        let mut actions = HashMap::new();
        for (index, action_name) in action_names.iter().cloned().enumerate() {
            let action = ModeActionId(u32::try_from(index).expect("mode action id overflow"));
            if actions.insert(action_name.clone(), action).is_some() {
                return Err(ModeRegistrationError::DuplicateAction {
                    mode: name,
                    action: action_name,
                });
            }
        }
        let id = ModeId(self.next_id);
        self.next_id = self.next_id.checked_add(1).expect("mode id overflow");
        let definition: Rc<dyn Mode> = Rc::from(definition);
        let adapters = RegisteredModeAdapters {
            buffer: has_buffer.then(|| definition.clone()),
            status_bar: has_status_bar.then(|| definition.clone()),
        };
        let registered = Rc::new(ModeRegistration {
            id,
            definition,
            adapters,
            action_names,
            actions,
        });
        self.ids_by_name.insert(name, id);
        self.definitions.insert(id, registered);
        Ok(id)
    }

    pub fn resolve_mode(&self, name: &ModeName) -> Option<ModeId> {
        self.ids_by_name.get(name).copied()
    }

    pub fn mode_name(&self, mode: ModeId) -> Option<&ModeName> {
        Some(self.definitions.get(&mode)?.mode().name())
    }

    pub fn adapter(&self, mode: ModeId, kind: ContentKind) -> Option<ModeAdapter<'_>> {
        self.definitions.get(&mode)?.adapter(kind)
    }

    pub fn ensure_adapter(
        &self,
        name: &ModeName,
        content: ContentId,
        kind: ContentKind,
    ) -> Result<(), ModeAttachmentError> {
        let id = self
            .resolve_mode(name)
            .ok_or_else(|| ModeAttachmentError::UnknownMode(name.clone()))?;
        if self.adapter(id, kind).is_none() {
            return Err(ModeAttachmentError::UnsupportedContent {
                mode: name.clone(),
                content,
                kind,
            });
        }
        Ok(())
    }

    pub fn resolve_action(&self, mode: ModeId, name: &ModeActionName) -> Option<ModeActionId> {
        self.definitions.get(&mode)?.actions.get(name).copied()
    }

    pub fn resolve_command_checked(
        &self,
        mode: &ModeName,
        action: &ModeActionName,
    ) -> Result<(ModeId, ModeActionId), ModeError> {
        let mode_id = self
            .resolve_mode(mode)
            .ok_or_else(|| ModeError::UnknownMode { mode: mode.clone() })?;
        let action_id =
            self.resolve_action(mode_id, action)
                .ok_or_else(|| ModeError::UnknownAction {
                    mode: mode.clone(),
                    action: action.clone(),
                })?;
        Ok((mode_id, action_id))
    }

    pub fn command_scope(
        &self,
        mode: &ModeName,
        action: &ModeActionName,
        kind: ContentKind,
    ) -> Result<ModeActionScope, ModeError> {
        let (mode_id, _) = self.resolve_command_checked(mode, action)?;
        let registered = &self.definitions[&mode_id];
        let adapter = registered
            .adapter(kind)
            .ok_or_else(|| ModeError::UnsupportedContent {
                mode: mode.clone(),
                kind,
            })?;
        Ok(adapter.behavior().action_scope(action))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn instantiate(&self, name: &ModeName) -> Option<ModeViewInstance> {
        let id = self.resolve_mode(name)?;
        let registered = self.definitions.get(&id)?.clone();
        registered.adapter(ContentKind::Buffer)?;
        Some(ModeViewInstance {
            state: Box::new(()),
            registered,
            adapter_kind: ContentKind::Buffer,
            fault: None,
            revision: Revision::default(),
        })
    }

    pub fn instantiate_with_context(
        &self,
        name: &ModeName,
        content: ContentId,
        kind: ContentKind,
        mode_contents: &mut ModeContentStore,
        content_context: &ModeContentContext<'_>,
        view_context: &ModeViewContext<'_>,
    ) -> Result<ModeViewInstance, ModeAttachmentError> {
        let id = self
            .resolve_mode(name)
            .ok_or_else(|| ModeAttachmentError::UnknownMode(name.clone()))?;
        let registered = self
            .definitions
            .get(&id)
            .expect("resolved mode exists")
            .clone();
        if registered.adapter(kind).is_none() {
            return Err(ModeAttachmentError::UnsupportedContent {
                mode: name.clone(),
                content,
                kind,
            });
        }
        let mut mode = ModeViewInstance {
            state: Box::new(()),
            registered,
            adapter_kind: kind,
            fault: None,
            revision: Revision::default(),
        };
        mode_contents.attach_view_with_context(content, &mut mode, content_context, view_context);
        Ok(mode)
    }
}

impl Default for ModeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ModeRegistration {
    fn mode(&self) -> &dyn Mode {
        self.definition.as_ref()
    }

    fn adapter(&self, kind: ContentKind) -> Option<ModeAdapter<'_>> {
        match kind {
            ContentKind::Buffer => self.adapters.buffer.as_deref().map(ModeAdapter::Buffer),
            ContentKind::StatusBar => self
                .adapters
                .status_bar
                .as_deref()
                .map(ModeAdapter::StatusBar),
        }
    }
}

pub struct ModeContentInstance {
    content: ContentId,
    registered: Rc<ModeRegistration>,
    adapter_kind: ContentKind,
    state: Box<dyn ModeState>,
    attachments: usize,
    fault: Option<ModeFault>,
    background_job_dirty: bool,
    revision: Revision,
}

impl ModeContentInstance {
    fn adapter(&self) -> &dyn Mode {
        self.registered
            .adapter(self.adapter_kind)
            .expect("attached content mode keeps its registered adapter")
            .behavior()
    }

    fn execute(
        &self,
        state: &mut dyn ModeState,
        faulted: bool,
        mode: ModeId,
        action: ModeActionId,
        arguments: &ModeValue,
        contents: &ContentStore,
    ) -> Result<ModeResult, ModeError> {
        if faulted {
            return Err(ModeError::InactiveMode {
                requested: self.registered.mode().name().clone(),
                active: None,
            });
        }
        assert_eq!(self.registered.id, mode, "resolved mode must be active");
        let action = self
            .registered
            .action_names
            .get(usize::try_from(action.0).expect("mode action index overflow"))
            .expect("mode action id belongs to registered mode");
        let context = ModeContentContext::new(self.content, contents);
        self.adapter()
            .execute_content_with_arguments(state, &context, action, arguments)
    }
}

#[derive(Default)]
pub struct ModeContentStore {
    instances: HashMap<(ModeId, ContentId), ModeContentInstance>,
}

impl ModeContentStore {
    pub fn is_faulted(&self, mode: ModeId, content: ContentId) -> bool {
        self.instances
            .get(&(mode, content))
            .is_some_and(|instance| instance.fault.is_some())
    }

    pub fn fault(&self, mode: ModeId, content: ContentId) -> Option<&ModeFault> {
        self.instances.get(&(mode, content))?.fault.as_ref()
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn faults_for_test(&self) -> Vec<(String, ContentId)> {
        self.instances
            .values()
            .filter(|instance| instance.fault.is_some())
            .map(|instance| {
                (
                    instance.registered.mode().name().as_str().to_owned(),
                    instance.content,
                )
            })
            .collect()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn state_for_test<T: 'static>(&self, mode: ModeId, content: ContentId) -> Option<&T> {
        self.instances
            .get(&(mode, content))?
            .state
            .as_any()
            .downcast_ref()
    }

    pub fn take_background_jobs(
        &mut self,
        contents: &ContentStore,
    ) -> Vec<(ModeId, ContentId, ModeJobRequest)> {
        let targets: Vec<_> = self.instances.keys().copied().collect();
        let mut drafts = ModeDraftJournal::default();
        let mut jobs = Vec::new();
        for (mode, content) in targets {
            let instance = self
                .instances
                .get(&(mode, content))
                .expect("collected mode content exists");
            if instance.fault.is_some() || !instance.background_job_dirty {
                continue;
            }
            let draft = drafts.content_mut(mode, content, instance);
            draft.background_job_dirty = false;
            let context = ModeContentContext::new(content, contents);
            let requests = instance
                .adapter()
                .take_background_jobs(draft.state.as_mut(), &context);
            for request in requests {
                jobs.push((mode, content, request));
            }
        }
        drafts.commit_content(self);
        jobs
    }

    pub fn apply_background_job(
        &mut self,
        mode: ModeId,
        content: ContentId,
        contents: &ContentStore,
        slot: &ModeJobSlot,
        version: u64,
        result: ModeJobResult,
    ) -> bool {
        let Some(instance) = self.instance(mode, content) else {
            return false;
        };
        let mut drafts = ModeDraftJournal::default();
        let draft = drafts.content_mut(mode, content, instance);
        if draft.fault.is_some() {
            return false;
        }
        let checkpoint = draft.state.clone_box();
        let context = ModeContentContext::new(content, contents);
        let changed = match instance.adapter().apply_background_job(
            draft.state.as_mut(),
            &context,
            slot,
            version,
            result,
        ) {
            Ok(changed) => {
                draft.background_job_dirty |= changed;
                changed
            }
            Err(error) => {
                draft.state = checkpoint;
                draft.fault = Some(ModeFault::from_error(
                    instance.registered.mode().name(),
                    ModeFaultPhase::BackgroundJob,
                    slot.as_str(),
                    &error,
                ));
                true
            }
        };
        drafts.commit_content(self);
        changed
    }

    pub fn presentation_layer(
        &self,
        mode: ModeId,
        content: ContentId,
        contents: &ContentStore,
        visible_rows: RowRange,
    ) -> Option<ContentPresentationLayer> {
        let instance = self.instance(mode, content)?;
        if instance.fault.is_some() {
            return None;
        }
        let context = ModeContentContext::new(content, contents);
        Some(ContentPresentationLayer {
            source_revision: context.content_revision()?,
            mode_revision: instance.revision,
            decorations: instance.adapter().content_decorations(
                instance.state.as_ref(),
                &context,
                visible_rows,
            ),
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn attach_view(&mut self, content: ContentId, mode: &ModeViewInstance) {
        let id = mode.registered.id;
        if let Some(existing) = self.instances.get_mut(&(id, content)) {
            existing.attachments += 1;
            return;
        }
        let mut contents = ContentStore::default();
        contents
            .insert(
                content,
                vell_core::content::Content::Buffer(vell_core::buffer::Buffer::new()),
            )
            .expect("test helper inserts one content");
        let context = ModeContentContext::new(content, &contents);
        let state = mode
            .adapter()
            .create_content_state(&context)
            .expect("test mode creates content state");
        self.instances.insert(
            (id, content),
            ModeContentInstance {
                content,
                registered: mode.registered.clone(),
                adapter_kind: mode.adapter_kind,
                state,
                attachments: 1,
                fault: None,
                background_job_dirty: true,
                revision: Revision::default(),
            },
        );
    }

    pub fn attach_view_with_context(
        &mut self,
        content: ContentId,
        mode: &mut ModeViewInstance,
        content_context: &ModeContentContext<'_>,
        view_context: &ModeViewContext<'_>,
    ) {
        let id = mode.registered.id;
        if let Some(existing) = self.instances.get_mut(&(id, content)) {
            existing.attachments += 1;
        } else {
            let (state, fault) = match mode.adapter().create_content_state(content_context) {
                Ok(state) => (state, None),
                Err(error) => (
                    Box::new(()) as Box<dyn ModeState>,
                    Some(ModeFault::from_error(
                        mode.name(),
                        ModeFaultPhase::ContentState,
                        "<content-state>",
                        &error,
                    )),
                ),
            };
            self.instances.insert(
                (id, content),
                ModeContentInstance {
                    content,
                    registered: mode.registered.clone(),
                    adapter_kind: mode.adapter_kind,
                    state,
                    attachments: 1,
                    background_job_dirty: fault.is_none(),
                    fault,
                    revision: Revision::default(),
                },
            );
        }
        let content_state = self
            .instances
            .get(&(id, content))
            .expect("attached mode has content state");
        mode.initialize(
            content_state.state.as_ref(),
            content_state.fault.is_some(),
            view_context,
        );
    }

    pub fn detach_view(&mut self, content: ContentId, mode: ModeId) {
        let key = (mode, content);
        let remove = self.instances.get_mut(&key).is_some_and(|instance| {
            instance.attachments -= 1;
            instance.attachments == 0
        });
        if remove {
            self.instances.remove(&key);
        }
    }

    pub fn notify_changed(
        &self,
        content: ContentId,
        contents: &ContentStore,
        change: &ContentChange,
        drafts: &mut ModeDraftJournal,
    ) {
        let modes: Vec<_> = self
            .instances
            .keys()
            .filter_map(|(mode, candidate)| (*candidate == content).then_some(*mode))
            .collect();
        for mode in modes {
            let instance = self
                .instances
                .get(&(mode, content))
                .expect("collected mode exists");
            let draft = drafts.content_mut(mode, content, instance);
            if draft.fault.is_some() {
                continue;
            }
            let checkpoint = draft.state.clone_box();
            let context = ModeContentContext::new(content, contents);
            if let Err(error) =
                instance
                    .adapter()
                    .on_content_changed(draft.state.as_mut(), &context, change)
            {
                draft.state = checkpoint;
                draft.fault = Some(ModeFault::from_error(
                    instance.registered.mode().name(),
                    ModeFaultPhase::ContentChanged,
                    "<content-changed>",
                    &error,
                ));
            } else {
                draft.background_job_dirty = true;
            }
        }
    }

    fn active_instance(&self, content: ContentId) -> Option<&ModeContentInstance> {
        self.instances
            .iter()
            .find_map(|((_, candidate), instance)| (*candidate == content).then_some(instance))
    }

    fn instance(&self, mode: ModeId, content: ContentId) -> Option<&ModeContentInstance> {
        self.instances.get(&(mode, content))
    }

    pub fn revision(&self, mode: ModeId, content: ContentId) -> Option<Revision> {
        Some(self.instance(mode, content)?.revision)
    }

    pub fn execute(
        &mut self,
        registry: &ModeRegistry,
        contents: &ContentStore,
        content: ContentId,
        command: &crate::command::ModeCommand,
        drafts: &mut ModeDraftJournal,
    ) -> Result<ModeResult, ModeError> {
        let (mode, action) = registry.resolve_command_checked(&command.mode, &command.action)?;
        let Some(instance) = self.instances.get(&(mode, content)) else {
            return Err(ModeError::InactiveMode {
                requested: command.mode.clone(),
                active: self
                    .active_instance(content)
                    .map(|instance| instance.registered.mode().name().clone()),
            });
        };
        let draft = drafts.content_mut(mode, content, instance);
        let result = instance.execute(
            draft.state.as_mut(),
            draft.fault.is_some(),
            mode,
            action,
            &command.arguments,
            contents,
        );
        if result.is_ok() {
            draft.background_job_dirty = true;
        } else if let Err(error) = &result
            && error.faults_instance()
        {
            draft.fault = Some(ModeFault::from_error(
                instance.registered.mode().name(),
                ModeFaultPhase::Action,
                command.action.as_str(),
                error,
            ));
        }
        result
    }
}

impl ModeViewInstance {
    fn adapter(&self) -> &dyn Mode {
        self.registered
            .adapter(self.adapter_kind)
            .expect("attached view mode keeps its registered adapter")
            .behavior()
    }

    fn initialize(
        &mut self,
        content_state: &dyn ModeState,
        content_faulted: bool,
        context: &ModeViewContext<'_>,
    ) {
        if content_faulted {
            return;
        }
        let state = self.adapter().create_view_state(content_state, context);
        match state {
            Ok(state) => self.state = state,
            Err(error) => {
                self.fault = Some(ModeFault::from_error(
                    self.name(),
                    ModeFaultPhase::ViewState,
                    "<view-state>",
                    &error,
                ));
            }
        }
    }

    pub fn name(&self) -> &ModeName {
        self.registered.mode().name()
    }

    pub fn register_faces(&self, faces: &mut FaceRegistry) {
        faces.register_defaults(self.registered.mode());
    }

    pub fn execute_with_context(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        faulted: bool,
        action: ModeActionId,
        arguments: &ModeValue,
        context: &ModeViewContext<'_>,
    ) -> Result<ModeResult, ModeError> {
        if faulted {
            return Err(ModeError::InactiveMode {
                requested: self.name().clone(),
                active: None,
            });
        }
        let action = self
            .registered
            .action_names
            .get(usize::try_from(action.0).expect("mode action index overflow"))
            .expect("mode action id belongs to registered mode");
        self.adapter().execute_view_with_arguments(
            content_state,
            view_state,
            context,
            action,
            arguments,
        )
    }

    fn execute_input_with_context(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        faulted: bool,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Result<ModeResult, ModeError> {
        if faulted {
            return Err(ModeError::InactiveMode {
                requested: self.name().clone(),
                active: None,
            });
        }
        self.adapter()
            .execute_input(content_state, view_state, context, key)
    }
}

impl ModeViewInstance {
    fn input_cancel_with_content(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        faulted: bool,
        context: &ModeViewContext<'_>,
    ) {
        if faulted {
            return;
        }
        self.adapter()
            .input_cancel(content_state, view_state, context);
    }
}

#[derive(Default)]
pub struct ModeViewStore {
    chains: HashMap<ViewId, Vec<ModeId>>,
    instances: HashMap<(ModeId, ViewId), ModeViewInstance>,
}

impl ModeViewStore {
    pub fn contains_mode(&self, mode: ModeId) -> bool {
        self.instances
            .keys()
            .any(|(candidate, _)| *candidate == mode)
    }

    pub fn is_faulted(&self, mode: ModeId, view: ViewId) -> bool {
        self.instances
            .get(&(mode, view))
            .is_some_and(|instance| instance.fault.is_some())
    }

    pub fn fault(&self, mode: ModeId, view: ViewId) -> Option<&ModeFault> {
        self.instances.get(&(mode, view))?.fault.as_ref()
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn faults_for_test(&self) -> Vec<(String, ViewId)> {
        self.instances
            .iter()
            .filter(|(_, instance)| instance.fault.is_some())
            .map(|((_, view), instance)| (instance.name().as_str().to_owned(), *view))
            .collect()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn state_for_test<T: 'static>(&self, mode: ModeId, view: ViewId) -> Option<&T> {
        self.instances
            .get(&(mode, view))?
            .state
            .as_any()
            .downcast_ref()
    }

    pub fn is_active(&self, view: ViewId) -> bool {
        self.chains
            .get(&view)
            .is_some_and(|chain| !chain.is_empty())
    }

    pub fn revision(&self, mode: ModeId, view: ViewId) -> Option<Revision> {
        Some(self.instances.get(&(mode, view))?.revision)
    }

    pub fn insert(&mut self, view: ViewId, mode: ModeViewInstance) {
        let id = mode.registered.id;
        let chain = self.chains.entry(view).or_default();
        assert!(
            !chain.contains(&id),
            "a mode may only be attached to a view once"
        );
        chain.push(id);
        assert!(self.instances.insert((id, view), mode).is_none());
    }

    pub fn remove(&mut self, view: ViewId) -> Vec<ModeId> {
        let modes = self.chains.remove(&view).unwrap_or_default();
        for mode in &modes {
            self.instances.remove(&(*mode, view));
        }
        modes
    }

    pub fn mode_ids(&self, view: ViewId) -> &[ModeId] {
        self.chains.get(&view).map_or(&[], Vec::as_slice)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn mode_names(&self, view: ViewId) -> Vec<ModeName> {
        self.mode_ids(view)
            .iter()
            .filter_map(|mode| self.instances.get(&(*mode, view)))
            .map(|instance| instance.name().clone())
            .collect()
    }

    pub fn notify_changed<'a>(
        &self,
        views: impl IntoIterator<Item = (ViewId, ContentId, &'a ContentViewState)>,
        content: ContentId,
        mode_contents: &ModeContentStore,
        contents: &ContentStore,
        change: &ContentChange,
        drafts: &mut ModeDraftJournal,
    ) {
        for (view, view_content, state) in views {
            if view_content != content {
                continue;
            }
            let Ok(context) = ModeViewContext::new(view, view_content, state, contents) else {
                continue;
            };
            let modes = self.mode_ids(view).to_vec();
            for mode in modes {
                let Some(content_instance) = mode_contents.instance(mode, content) else {
                    continue;
                };
                let Some(view_instance) = self.instances.get(&(mode, view)) else {
                    continue;
                };
                let (content_draft, view_draft) = drafts.content_and_view_mut(
                    mode,
                    content,
                    view,
                    content_instance,
                    view_instance,
                );
                if content_draft.fault.is_some() || view_draft.fault.is_some() {
                    continue;
                }
                let content_checkpoint = content_draft.state.clone_box();
                let view_checkpoint = view_draft.state.clone_box();
                if let Err(error) = view_instance.adapter().on_view_content_changed(
                    content_draft.state.as_mut(),
                    view_draft.state.as_mut(),
                    &context,
                    change,
                ) {
                    content_draft.state = content_checkpoint;
                    view_draft.state = view_checkpoint;
                    view_draft.fault = Some(ModeFault::from_error(
                        view_instance.name(),
                        ModeFaultPhase::ContentChanged,
                        "<view-content-changed>",
                        &error,
                    ));
                }
            }
        }
    }

    pub fn presentation_layer(
        &self,
        mode: ModeId,
        view: ViewId,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        view_revision: Revision,
        visible_rows: RowRange,
    ) -> Option<ViewPresentationLayer> {
        let content_instance = mode_contents.instance(mode, context.content_id())?;
        let view_instance = self.instances.get(&(mode, view))?;
        if content_instance.fault.is_some() || view_instance.fault.is_some() {
            return None;
        }
        let definition = view_instance.adapter();
        Some(ViewPresentationLayer {
            content_revision: context.content_revision()?,
            view_revision,
            content_mode_revision: content_instance.revision,
            view_mode_revision: view_instance.revision,
            policy: definition.view_policy(
                content_instance.state.as_ref(),
                view_instance.state.as_ref(),
                context,
            ),
            decorations: definition.view_decorations(
                content_instance.state.as_ref(),
                view_instance.state.as_ref(),
                context,
                visible_rows,
            ),
        })
    }

    pub fn contains(&self, view: ViewId, name: &ModeName) -> bool {
        self.mode_ids(view)
            .iter()
            .filter_map(|mode| self.instances.get(&(*mode, view)))
            .any(|instance| instance.name() == name)
    }

    fn first(&self, view: ViewId) -> Option<&ModeViewInstance> {
        let mode = *self.mode_ids(view).first()?;
        self.instances.get(&(mode, view))
    }

    pub fn keymap_at<'a>(
        &'a self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &'a ModeContentStore,
        drafts: &'a ModeDraftJournal,
    ) -> Option<&'a Keymap<Command>> {
        let mode = *self.mode_ids(view).get(index)?;
        let content_state = mode_contents.instance(mode, context.content_id())?;
        let instance = self.instances.get(&(mode, view))?;
        let (content_state, content_faulted) =
            drafts.content(mode, context.content_id(), content_state);
        let (view_state, view_faulted) = drafts.view(mode, view, instance);
        (!content_faulted && !view_faulted).then(|| {
            instance
                .adapter()
                .input_keymap(content_state, view_state, context)
        })
    }

    pub fn fallback_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &ModeDraftJournal,
        key: KeyEvent,
    ) -> Option<Command> {
        let mode = *self.mode_ids(view).get(index)?;
        let content_state = mode_contents.instance(mode, context.content_id())?;
        let instance = self.instances.get(&(mode, view))?;
        let (content_state, content_faulted) =
            drafts.content(mode, context.content_id(), content_state);
        let (view_state, view_faulted) = drafts.view(mode, view, instance);
        if content_faulted || view_faulted {
            return None;
        }
        instance
            .adapter()
            .input_typing(content_state, view_state, context, key)
    }

    pub fn status_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &ModeDraftJournal,
    ) -> InputStatus {
        let Some(mode) = self.mode_ids(view).get(index).copied() else {
            return InputStatus::Ready;
        };
        let Some(content_state) = mode_contents.instance(mode, context.content_id()) else {
            return InputStatus::Ready;
        };
        let Some(instance) = self.instances.get(&(mode, view)) else {
            return InputStatus::Ready;
        };
        let (content_state, content_faulted) =
            drafts.content(mode, context.content_id(), content_state);
        let (view_state, view_faulted) = drafts.view(mode, view, instance);
        if content_faulted || view_faulted {
            return InputStatus::Ready;
        }
        instance
            .adapter()
            .mode_input_status(content_state, view_state, context)
    }

    pub fn capture_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &mut ModeDraftJournal,
        key: KeyEvent,
    ) -> InputDecision<Command> {
        let Some(mode) = self.mode_ids(view).get(index).copied() else {
            return InputDecision::Pass;
        };
        let Some(content_state) = mode_contents.instance(mode, context.content_id()) else {
            return InputDecision::Pass;
        };
        let Some(instance) = self.instances.get(&(mode, view)) else {
            return InputDecision::Pass;
        };
        let (content_draft, view_draft) =
            drafts.content_and_view_mut(mode, context.content_id(), view, content_state, instance);
        if content_draft.fault.is_some() || view_draft.fault.is_some() {
            return InputDecision::Pass;
        }
        instance.adapter().input_capture(
            content_draft.state.as_mut(),
            view_draft.state.as_mut(),
            context,
            key,
        )
    }

    pub fn timeout_at(
        &self,
        view: ViewId,
        index: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> Option<Vec<OperationRequest>> {
        let mode = self.mode_ids(view).get(index).copied()?;
        let content_state = mode_contents.instance(mode, context.content_id())?;
        let instance = self.instances.get(&(mode, view))?;
        let (content_draft, view_draft) =
            drafts.content_and_view_mut(mode, context.content_id(), view, content_state, instance);
        if content_draft.fault.is_some() || view_draft.fault.is_some() {
            return None;
        }
        Some(
            instance
                .adapter()
                .input_timeout(
                    content_draft.state.as_mut(),
                    view_draft.state.as_mut(),
                    context,
                )
                .into_operations(),
        )
    }

    pub fn fallback_in_chain(
        &self,
        view: ViewId,
        start_mode: usize,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &ModeDraftJournal,
        key: KeyEvent,
    ) -> Option<(usize, Command)> {
        self.mode_ids(view)
            .iter()
            .enumerate()
            .skip(start_mode)
            .find_map(|(index, mode)| {
                let content_instance = mode_contents.instance(*mode, context.content_id())?;
                let view_instance = self.instances.get(&(*mode, view))?;
                let (content_state, content_faulted) =
                    drafts.content(*mode, context.content_id(), content_instance);
                let (view_state, view_faulted) = drafts.view(*mode, view, view_instance);
                if content_faulted || view_faulted {
                    return None;
                }
                view_instance
                    .adapter()
                    .input_typing(content_state, view_state, context, key)
                    .map(|command| (index, command))
            })
    }

    pub fn cancel_chain(
        &mut self,
        view: ViewId,
        context: &ModeViewContext<'_>,
        mode_contents: &mut ModeContentStore,
        _contents: &ContentStore,
    ) {
        let modes = self.mode_ids(view).to_vec();
        let mut drafts = ModeDraftJournal::default();
        for mode in modes {
            let Some(content_state) = mode_contents.instance(mode, context.content_id()) else {
                continue;
            };
            let Some(instance) = self.instances.get(&(mode, view)) else {
                continue;
            };
            let (content_draft, view_draft) = drafts.content_and_view_mut(
                mode,
                context.content_id(),
                view,
                content_state,
                instance,
            );
            instance.input_cancel_with_content(
                content_draft.state.as_mut(),
                view_draft.state.as_mut(),
                view_draft.fault.is_some(),
                context,
            );
        }
        drafts.commit_content(mode_contents);
        drafts.commit_views(self);
    }

    pub fn view_policy_in_draft(
        &self,
        view: ViewId,
        context: &ModeViewContext<'_>,
        mode_contents: &ModeContentStore,
        drafts: &ModeDraftJournal,
    ) -> ModeViewPolicy {
        let mut policy = ModeViewPolicy::default();
        for mode in self.mode_ids(view) {
            let Some(content_instance) = mode_contents.instance(*mode, context.content_id()) else {
                continue;
            };
            let Some(view_instance) = self.instances.get(&(*mode, view)) else {
                continue;
            };
            let (content_state, content_faulted) =
                drafts.content(*mode, context.content_id(), content_instance);
            let (view_state, view_faulted) = drafts.view(*mode, view, view_instance);
            if content_faulted || view_faulted {
                continue;
            }
            policy.merge_missing(view_instance.adapter().view_policy(
                content_state,
                view_state,
                context,
            ));
        }
        policy
    }

    pub fn execute_with_context(
        &mut self,
        view: ViewId,
        registry: &ModeRegistry,
        command: &crate::command::ModeCommand,
        context: &ModeViewContext<'_>,
        mode_contents: &mut ModeContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> Result<ModeResult, ModeError> {
        let (mode, action) = registry.resolve_command_checked(&command.mode, &command.action)?;
        let Some(instance) = self.instances.get(&(mode, view)) else {
            return Err(ModeError::InactiveMode {
                requested: command.mode.clone(),
                active: self.first(view).map(|instance| instance.name().clone()),
            });
        };
        let content_state = mode_contents
            .instance(mode, context.content_id())
            .expect("attached mode has content state");
        let (content_draft, view_draft) =
            drafts.content_and_view_mut(mode, context.content_id(), view, content_state, instance);
        let result = instance.execute_with_context(
            content_draft.state.as_mut(),
            view_draft.state.as_mut(),
            view_draft.fault.is_some() || content_draft.fault.is_some(),
            action,
            &command.arguments,
            context,
        );
        if result.is_ok() {
            content_draft.background_job_dirty = true;
        } else if let Err(error) = &result
            && error.faults_instance()
        {
            view_draft.fault = Some(ModeFault::from_error(
                instance.name(),
                ModeFaultPhase::Action,
                command.action.as_str(),
                error,
            ));
        }
        result
    }

    pub fn execute_input_with_context(
        &mut self,
        view: ViewId,
        registry: &ModeRegistry,
        input: &crate::command::ModeInputCommand,
        context: &ModeViewContext<'_>,
        mode_contents: &mut ModeContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> Result<ModeResult, ModeError> {
        let mode = registry
            .resolve_mode(input.mode())
            .ok_or_else(|| ModeError::UnknownMode {
                mode: input.mode().clone(),
            })?;
        let Some(instance) = self.instances.get(&(mode, view)) else {
            return Err(ModeError::InactiveMode {
                requested: input.mode().clone(),
                active: self.first(view).map(|instance| instance.name().clone()),
            });
        };
        let content_state = mode_contents
            .instance(mode, context.content_id())
            .expect("attached mode has content state");
        let (content_draft, view_draft) =
            drafts.content_and_view_mut(mode, context.content_id(), view, content_state, instance);
        let result = instance.execute_input_with_context(
            content_draft.state.as_mut(),
            view_draft.state.as_mut(),
            view_draft.fault.is_some() || content_draft.fault.is_some(),
            context,
            input.key(),
        );
        if result.is_ok() {
            content_draft.background_job_dirty = true;
        } else if let Err(error) = &result
            && error.faults_instance()
        {
            view_draft.fault = Some(ModeFault::from_error(
                instance.name(),
                ModeFaultPhase::Input,
                "<input>",
                error,
            ));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::TypedMode;

    struct CountingJobMode {
        name: ModeName,
        calls: Rc<Cell<usize>>,
    }

    struct DraftStateMode {
        name: ModeName,
        actions: Vec<ModeActionName>,
        fail_observer: bool,
    }

    fn contents_with_buffer(content: ContentId) -> ContentStore {
        let mut contents = ContentStore::default();
        contents
            .insert(
                content,
                vell_core::content::Content::Buffer(vell_core::buffer::Buffer::new()),
            )
            .unwrap();
        contents
    }

    struct NoAdapterMode(ModeName);

    struct StandardFaceMode(ModeName);

    impl Mode for NoAdapterMode {
        fn name(&self) -> &ModeName {
            &self.0
        }

        fn actions(&self) -> &[ModeActionName] {
            &[]
        }

        fn adapters(&self) -> ModeAdapters {
            ModeAdapters::default()
        }
    }

    impl Mode for StandardFaceMode {
        fn name(&self) -> &ModeName {
            &self.0
        }

        fn actions(&self) -> &[ModeActionName] {
            &[]
        }

        fn adapters(&self) -> ModeAdapters {
            ModeAdapters::buffer()
        }

        fn faces(&self) -> Vec<(FaceName, Face)> {
            vec![(
                FaceName::new("syntax.keyword"),
                Face {
                    bold: Some(true),
                    ..Face::default()
                },
            )]
        }
    }

    impl TypedMode for DraftStateMode {
        type ContentState = u8;
        type ViewState = ();
        type JobOutput = ();

        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[ModeActionName] {
            &self.actions
        }

        fn adapters(&self) -> ModeAdapters {
            ModeAdapters::buffer()
        }

        fn action_scope(&self, _action: &ModeActionName) -> ModeActionScope {
            ModeActionScope::Content
        }

        fn create_content_state(
            &self,
            _context: &ModeContentContext<'_>,
        ) -> Result<Self::ContentState, ModeError> {
            Ok(0)
        }

        fn create_view_state(
            &self,
            _content_state: &Self::ContentState,
            _context: &ModeViewContext<'_>,
        ) -> Result<Self::ViewState, ModeError> {
            Ok(())
        }

        fn execute_content_with_arguments(
            &self,
            state: &mut Self::ContentState,
            _context: &ModeContentContext<'_>,
            _action: &ModeActionName,
            _arguments: &ModeValue,
        ) -> Result<ModeResult, ModeError> {
            *state += 1;
            Ok(ModeResult::none())
        }

        fn on_content_changed(
            &self,
            state: &mut Self::ContentState,
            _context: &ModeContentContext<'_>,
            _change: &ContentChange,
        ) -> Result<(), ModeError> {
            *state += 1;
            if self.fail_observer {
                Err(ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message: "observer failed".to_owned(),
                })
            } else {
                Ok(())
            }
        }
    }

    impl Mode for CountingJobMode {
        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[ModeActionName] {
            &[]
        }

        fn adapters(&self) -> ModeAdapters {
            ModeAdapters::buffer()
        }

        fn take_background_jobs(
            &self,
            _state: &mut dyn ModeState,
            _context: &ModeContentContext<'_>,
        ) -> Vec<ModeJobRequest> {
            self.calls.set(self.calls.get() + 1);
            Vec::new()
        }

        fn apply_background_job(
            &self,
            _state: &mut dyn ModeState,
            _context: &ModeContentContext<'_>,
            _slot: &ModeJobSlot,
            _version: u64,
            _result: ModeJobResult,
        ) -> Result<bool, ModeError> {
            Err(ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: "job apply failed".to_owned(),
            })
        }
    }

    #[test]
    fn registration_rejects_a_mode_without_content_adapters() {
        let name = ModeName::new("no-adapter");
        let mut registry = ModeRegistry::new();

        assert_eq!(
            registry.register(NoAdapterMode(name.clone())),
            Err(ModeRegistrationError::MissingAdapter(name))
        );
    }

    #[test]
    fn face_registry_rejects_mode_owned_host_namespaces() {
        let mode = StandardFaceMode(ModeName::new("standard-face"));
        let mut registry = FaceRegistry::default();

        registry.register_defaults(&mode);

        assert_eq!(
            registry.resolve(&FaceName::new("syntax.keyword")),
            FacePatch::default()
        );
        assert_eq!(
            registry.registration_errors(),
            &[FaceRegistrationError {
                face: FaceName::new("syntax.keyword"),
                rejected_provider: ModeName::new("standard-face"),
                reason: FaceRegistrationErrorReason::HostNamespace,
            }]
        );
    }

    #[test]
    fn face_registry_resolves_multiple_parents_in_declared_priority_order() {
        let mut registry = FaceRegistry::default();
        registry.register_definitions(
            ModeName::new("faces"),
            vec![
                FaceDefinition {
                    name: FaceName::new("plugin.faces.low"),
                    inherits: Vec::new(),
                    fallback: FacePatch {
                        bold: vell_protocol::content_query::FaceValue::Value(false),
                        italic: vell_protocol::content_query::FaceValue::Value(true),
                        ..FacePatch::default()
                    },
                },
                FaceDefinition {
                    name: FaceName::new("plugin.faces.high"),
                    inherits: Vec::new(),
                    fallback: FacePatch {
                        bold: vell_protocol::content_query::FaceValue::Value(true),
                        ..FacePatch::default()
                    },
                },
                FaceDefinition {
                    name: FaceName::new("plugin.faces.child"),
                    inherits: vec![
                        FaceName::new("plugin.faces.high"),
                        FaceName::new("plugin.faces.low"),
                    ],
                    fallback: FacePatch::default(),
                },
            ],
        );

        let resolved = registry.resolve(&FaceName::new("plugin.faces.child"));
        assert_eq!(
            resolved.bold,
            vell_protocol::content_query::FaceValue::Value(true)
        );
        assert_eq!(
            resolved.italic,
            vell_protocol::content_query::FaceValue::Value(true)
        );
    }

    #[test]
    fn cyclic_face_definition_batch_is_rejected_atomically() {
        let mut registry = FaceRegistry::default();
        registry.register_definitions(
            ModeName::new("cyclic"),
            vec![
                FaceDefinition {
                    name: FaceName::new("plugin.cyclic.a"),
                    inherits: vec![FaceName::new("plugin.cyclic.b")],
                    fallback: FacePatch::default(),
                },
                FaceDefinition {
                    name: FaceName::new("plugin.cyclic.b"),
                    inherits: vec![FaceName::new("plugin.cyclic.a")],
                    fallback: FacePatch::default(),
                },
            ],
        );

        assert!(
            registry
                .definition(&FaceName::new("plugin.cyclic.a"))
                .is_none()
        );
        assert_eq!(registry.registration_errors().len(), 2);
        assert!(
            registry
                .registration_errors()
                .iter()
                .all(|error| { error.reason == FaceRegistrationErrorReason::InheritanceCycle })
        );
    }

    #[test]
    fn unchanged_content_does_not_poll_background_jobs_again() {
        let calls = Rc::new(Cell::new(0));
        let name = ModeName::new("counting-jobs");
        let mut registry = ModeRegistry::new();
        registry
            .register(CountingJobMode {
                name: name.clone(),
                calls: calls.clone(),
            })
            .unwrap();
        let mode = registry.instantiate(&name).unwrap();
        let mut content_modes = ModeContentStore::default();
        content_modes.attach_view(ContentId(1), &mode);
        let contents = contents_with_buffer(ContentId(1));

        assert!(content_modes.take_background_jobs(&contents).is_empty());
        assert!(content_modes.take_background_jobs(&contents).is_empty());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn background_fault_invalidates_presentation_and_keeps_structured_error() {
        let name = ModeName::new("failing-job");
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(CountingJobMode {
                name: name.clone(),
                calls: Rc::new(Cell::new(0)),
            })
            .unwrap();
        let mode = registry.instantiate(&name).unwrap();
        let content = ContentId(1);
        let contents = contents_with_buffer(content);
        let mut content_modes = ModeContentStore::default();
        content_modes.attach_view(content, &mode);

        let changed = content_modes.apply_background_job(
            mode_id,
            content,
            &contents,
            &ModeJobSlot::from("parse"),
            1,
            Ok(Box::new(())),
        );

        assert!(changed);
        let fault = content_modes.fault(mode_id, content).unwrap();
        assert_eq!(fault.phase, ModeFaultPhase::BackgroundJob);
        assert_eq!(fault.category, ModeFaultCategory::Callback);
        assert_eq!(fault.callback, "parse");
        assert_eq!(
            fault.message,
            "mode 'failing-job' callback failed: job apply failed"
        );
    }

    #[test]
    fn last_view_detaches_shared_content_state() {
        let name = ModeName::new("counting-jobs");
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(CountingJobMode {
                name: name.clone(),
                calls: Rc::new(Cell::new(0)),
            })
            .unwrap();
        let mode = registry.instantiate(&name).unwrap();
        let content = ContentId(1);
        let mut content_modes = ModeContentStore::default();

        content_modes.attach_view(content, &mode);
        content_modes.attach_view(content, &mode);
        content_modes.detach_view(content, mode_id);
        assert!(content_modes.instance(mode_id, content).is_some());

        content_modes.detach_view(content, mode_id);
        assert!(content_modes.instance(mode_id, content).is_none());
    }

    #[test]
    fn content_state_draft_is_visible_in_frame_and_published_only_on_commit() {
        let name = ModeName::new("draft-state");
        let action = ModeActionName::new("advance");
        let mut registry = ModeRegistry::new();
        registry
            .register_typed(DraftStateMode {
                name: name.clone(),
                actions: vec![action.clone()],
                fail_observer: false,
            })
            .unwrap();
        let mode = registry.instantiate(&name).unwrap();
        let mode_id = mode.registered.id;
        let content = ContentId(1);
        let mut content_modes = ModeContentStore::default();
        content_modes.attach_view(content, &mode);
        let contents = contents_with_buffer(content);
        let command = crate::command::ModeCommand::new(name, action);
        let mut drafts = ModeDraftJournal::default();

        content_modes
            .execute(&registry, &contents, content, &command, &mut drafts)
            .unwrap();
        content_modes
            .execute(&registry, &contents, content, &command, &mut drafts)
            .unwrap();

        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&0)
        );
        let draft = drafts.content.get(&(mode_id, content)).unwrap();
        assert_eq!(draft.state.as_any().downcast_ref::<u8>(), Some(&2));

        drafts.commit_content(&mut content_modes);
        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&2)
        );

        let mut discarded = ModeDraftJournal::default();
        content_modes
            .execute(&registry, &contents, content, &command, &mut discarded)
            .unwrap();
        drop(discarded);
        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&2)
        );
    }

    #[test]
    fn unchanged_draft_does_not_advance_mode_revision() {
        let name = ModeName::new("unchanged-draft");
        let mut registry = ModeRegistry::new();
        registry
            .register_typed(DraftStateMode {
                name: name.clone(),
                actions: vec![ModeActionName::new("advance")],
                fail_observer: false,
            })
            .unwrap();
        let mode = registry.instantiate(&name).unwrap();
        let mode_id = mode.registered.id;
        let content = ContentId(1);
        let mut content_modes = ModeContentStore::default();
        content_modes.attach_view(content, &mode);
        let revision = content_modes.revision(mode_id, content).unwrap();
        let instance = content_modes.instance(mode_id, content).unwrap();
        let mut drafts = ModeDraftJournal::default();

        drafts.content_mut(mode_id, content, instance);
        drafts.commit_content(&mut content_modes);

        assert_eq!(content_modes.revision(mode_id, content), Some(revision));
    }

    #[test]
    fn passive_callback_fault_is_published_only_with_its_frame() {
        let name = ModeName::new("faulting-observer-draft");
        let mut registry = ModeRegistry::new();
        registry
            .register_typed(DraftStateMode {
                name: name.clone(),
                actions: vec![ModeActionName::new("advance")],
                fail_observer: true,
            })
            .unwrap();
        let mode = registry.instantiate(&name).unwrap();
        let mode_id = mode.registered.id;
        let content = ContentId(1);
        let mut content_modes = ModeContentStore::default();
        content_modes.attach_view(content, &mode);
        let contents = contents_with_buffer(content);
        let change = ContentChange::Text(
            vell_core::transaction::TextChangeSet::from_edits(
                0,
                vec![vell_core::transaction::TextEdit::new(0..0, "x")],
            )
            .unwrap(),
        );
        let mut discarded = ModeDraftJournal::default();

        content_modes.notify_changed(content, &contents, &change, &mut discarded);

        assert!(content_modes.faults_for_test().is_empty());
        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&0)
        );
        drop(discarded);
        assert!(content_modes.faults_for_test().is_empty());

        let mut committed = ModeDraftJournal::default();
        content_modes.notify_changed(content, &contents, &change, &mut committed);
        committed.commit_content(&mut content_modes);

        assert_eq!(
            content_modes.faults_for_test(),
            vec![(name.as_str().to_owned(), content)]
        );
        let fault = content_modes.fault(mode_id, content).unwrap();
        assert_eq!(fault.phase, ModeFaultPhase::ContentChanged);
        assert_eq!(fault.category, ModeFaultCategory::Callback);
        assert_eq!(fault.callback, "<content-changed>");
        assert!(fault.message.contains("observer failed"));
        assert_eq!(
            content_modes.state_for_test::<u8>(mode_id, content),
            Some(&0)
        );
    }

    #[test]
    fn registration_rejects_duplicate_mode_and_action_names() {
        let mut registry = ModeRegistry::new();
        registry
            .register_typed(DraftStateMode {
                name: ModeName::new("duplicate"),
                actions: vec![ModeActionName::new("run")],
                fail_observer: false,
            })
            .unwrap();

        assert!(matches!(
            registry.register_typed(DraftStateMode {
                name: ModeName::new("duplicate"),
                actions: vec![ModeActionName::new("other")],
                fail_observer: false,
            }),
            Err(ModeRegistrationError::DuplicateMode(_))
        ));
        assert!(matches!(
            registry.register_typed(DraftStateMode {
                name: ModeName::new("duplicate-action"),
                actions: vec![ModeActionName::new("run"), ModeActionName::new("run")],
                fail_observer: false,
            }),
            Err(ModeRegistrationError::DuplicateAction { .. })
        ));
    }
}
