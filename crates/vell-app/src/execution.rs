use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::buffer_lifecycle::BufferInfo;
use crate::dispatcher::DispatcherInputSnapshot;
use crate::kernel::{FileBaseline, OpenCompletion};
use crate::mode::ModeDraftJournal;
use crate::operation::{OperationError, ViewSpec};
use crate::theme::{FaceRemapOwner, ResolvedFaceOperation};
use crate::transaction::TransactionRecord;
use vell_core::clipboard::ClipboardPayload;
use vell_core::content::SaveSnapshot;
use vell_core::content_store::ContentSnapshot;
use vell_core::transaction::TransactionDirection;
use vell_mode::command_registry::{CommandPending, CommandTaskId};
use vell_mode::operation::MAX_OPERATIONS_PER_FRAME;
use vell_protocol::content_query::{FaceName, FaceRemapScope, FaceRemapToken};
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::Selections;
use vell_protocol::space::SplitDirection;
use vell_protocol::view::BindingKey;
use vell_protocol::viewport::ResolvedViewportCommand;

const DEFAULT_NESTED_MODE_BUDGET: usize = 256;
const DEFAULT_REPLAYED_INPUT_BUDGET: usize = 256;

pub(super) struct ExecutionFrame {
    checkpoints: CheckpointJournal,
    mode_drafts: ModeDraftJournal,
    provisional_contents: Vec<ContentId>,
    view_touches: HashMap<ViewId, Revision>,
    prepared_effects: Vec<PreparedEffect>,
    prepared_face_bases: HashMap<(FaceRemapScope, FaceName), FaceRemapOwner>,
    prepared_face_tokens: HashSet<FaceRemapToken>,
    topology_effect_prepared: bool,
    viewport_effect_prepared: bool,
    pending_command: Option<PendingCommandStart>,
    budget: ExecutionBudget,
}

pub(super) struct PendingCommandStart {
    pub pending: CommandPending,
    pub view: ViewId,
    pub content: ContentId,
}

pub(super) struct ExecutionFrameParts {
    pub checkpoints: CheckpointJournal,
    pub mode_drafts: ModeDraftJournal,
    pub provisional_contents: Vec<ContentId>,
    pub view_touches: HashMap<ViewId, Revision>,
    pub effects: Vec<PreparedEffect>,
    pub pending_command: Option<PendingCommandStart>,
}

pub(super) struct CheckpointJournal {
    target: Option<ContentId>,
    content: Option<ContentSnapshot>,
    selections: Option<SelectionCheckpoint>,
    input: Option<InputCheckpoint>,
    state_rollbacks: Vec<StateRollback>,
}

pub(super) type SelectionCheckpoint = HashMap<ViewId, (Selections, Revision)>;

pub(super) struct InputCheckpoint {
    pub dispatcher: DispatcherInputSnapshot,
}

pub(super) enum StateRollback {
    Text(TransactionRecord, TransactionDirection),
}

pub(super) enum PreparedEffect {
    HistoryCommit {
        content: ContentId,
    },
    Save {
        content: ContentId,
        snapshot: SaveSnapshot,
        force: bool,
        task: Option<CommandTaskId>,
    },
    SaveAs {
        content: ContentId,
        snapshot: SaveSnapshot,
        identity: PathBuf,
        force: bool,
    },
    ReloadCommit {
        content: ContentId,
        path: PathBuf,
        baseline: FileBaseline,
    },
    ContentOpenCommit(OpenCompletion),
    ContentList(Vec<BufferInfo>),
    ContentCreate,
    ContentOpen(PathBuf),
    ViewSwitch {
        target: ViewId,
        spec: ViewSpec,
    },
    ViewRebind {
        view: ViewId,
        binding: BindingKey,
        content: ContentId,
    },
    ContentClose {
        content: ContentId,
        force: bool,
    },
    Viewport {
        view: ViewId,
        command: ResolvedViewportCommand,
    },
    Split {
        target: SpaceId,
        content: ContentId,
        direction: SplitDirection,
    },
    Close {
        target: SpaceId,
    },
    Focus {
        target: SpaceId,
    },
    Face(ResolvedFaceOperation),
    ClipboardStore {
        payload: ClipboardPayload,
        write_system: bool,
    },
    Quit,
}

pub(super) struct ExecutionBudget {
    operations: usize,
    nested_mode_calls: usize,
    replayed_inputs: usize,
}

impl ExecutionFrame {
    pub(super) fn new(target: Option<ContentId>, input: Option<InputCheckpoint>) -> Self {
        Self {
            checkpoints: CheckpointJournal {
                target,
                content: None,
                selections: None,
                input,
                state_rollbacks: Vec::new(),
            },
            mode_drafts: ModeDraftJournal::default(),
            provisional_contents: Vec::new(),
            view_touches: HashMap::new(),
            prepared_effects: Vec::new(),
            prepared_face_bases: HashMap::new(),
            prepared_face_tokens: HashSet::new(),
            topology_effect_prepared: false,
            viewport_effect_prepared: false,
            pending_command: None,
            budget: ExecutionBudget::default(),
        }
    }

    pub(super) fn prepare(&mut self, effect: PreparedEffect) {
        self.prepared_effects.push(effect);
    }

    pub(super) fn prepared_effect_count(&self) -> usize {
        self.prepared_effects.len()
    }

    pub(super) fn attach_task_to_save_since(&mut self, start: usize, task: CommandTaskId) -> bool {
        self.prepared_effects[start..]
            .iter_mut()
            .rev()
            .find_map(|effect| match effect {
                PreparedEffect::Save {
                    task: save_task, ..
                } if save_task.is_none() => Some(save_task),
                _ => None,
            })
            .is_some_and(|save_task| {
                *save_task = Some(task);
                true
            })
    }

    pub(super) fn stage_pending_command(
        &mut self,
        pending: CommandPending,
        view: ViewId,
        content: ContentId,
    ) -> Result<(), OperationError> {
        if self.pending_command.is_some() {
            return Err(OperationError::new(
                "an execution frame cannot suspend more than one command",
            ));
        }
        self.pending_command = Some(PendingCommandStart {
            pending,
            view,
            content,
        });
        Ok(())
    }

    pub(super) fn has_pending_command(&self) -> bool {
        self.pending_command.is_some()
    }

    pub(super) fn prepare_face(
        &mut self,
        operation: ResolvedFaceOperation,
    ) -> Result<(), OperationError> {
        match &operation {
            ResolvedFaceOperation::SetBase {
                scope, face, owner, ..
            } => {
                let key = (*scope, face.clone());
                if self
                    .prepared_face_bases
                    .get(&key)
                    .is_some_and(|prepared_owner| prepared_owner != owner)
                {
                    return Err(OperationError::new(
                        "face remap base has multiple owners in one execution frame",
                    ));
                }
                self.prepared_face_bases.insert(key, *owner);
            }
            ResolvedFaceOperation::AddRelative { token, .. }
            | ResolvedFaceOperation::RemoveRelative { token, .. } => {
                if !self.prepared_face_tokens.insert(*token) {
                    return Err(OperationError::new(
                        "face remap token is used more than once in one execution frame",
                    ));
                }
            }
        }
        self.prepared_effects.push(PreparedEffect::Face(operation));
        Ok(())
    }

    pub(super) fn prepare_topology(
        &mut self,
        effect: PreparedEffect,
    ) -> Result<(), OperationError> {
        if self.topology_effect_prepared || self.viewport_effect_prepared {
            return Err(OperationError::new(
                "an execution frame accepts only one topology effect and cannot combine it with viewport effects",
            ));
        }
        self.topology_effect_prepared = true;
        self.prepared_effects.push(effect);
        Ok(())
    }

    pub(super) fn prepare_viewport(
        &mut self,
        effect: PreparedEffect,
    ) -> Result<(), OperationError> {
        if self.topology_effect_prepared {
            return Err(OperationError::new(
                "viewport effects cannot share an execution frame with a topology effect",
            ));
        }
        self.viewport_effect_prepared = true;
        self.prepared_effects.push(effect);
        Ok(())
    }

    pub(super) fn record_state_rollback(&mut self, rollback: StateRollback) {
        self.checkpoints.state_rollbacks.push(rollback);
    }

    pub(super) fn record_provisional_content(&mut self, content: ContentId) {
        self.provisional_contents.push(content);
    }

    pub(super) fn retarget(&mut self, content: ContentId) -> Result<(), OperationError> {
        if self.checkpoints.target == Some(content) {
            return Ok(());
        }
        if self.checkpoints.content.is_some()
            || self.checkpoints.selections.is_some()
            || !self.prepared_effects.is_empty()
        {
            return Err(OperationError::new(
                "execution frame cannot change target after preparing content or effects",
            ));
        }
        self.checkpoints.target = Some(content);
        Ok(())
    }

    pub(super) fn targets(&self, content: ContentId) -> bool {
        self.checkpoints.target == Some(content)
    }

    pub(super) fn needs_target_checkpoint(&self, content: ContentId) -> bool {
        assert_eq!(
            self.checkpoints.target,
            Some(content),
            "execution frame changed content target"
        );
        self.checkpoints.content.is_none()
    }

    pub(super) fn record_target_checkpoint(
        &mut self,
        content: ContentSnapshot,
        selections: SelectionCheckpoint,
    ) {
        assert!(self.checkpoints.content.is_none());
        assert!(self.checkpoints.selections.is_none());
        self.checkpoints.content = Some(content);
        self.checkpoints.selections = Some(selections);
    }

    pub(super) fn consume_operation(&mut self) -> Result<(), OperationError> {
        self.budget.consume_operation()
    }

    pub(super) fn can_prepare_lifecycle(&self) -> bool {
        self.checkpoints.content.is_none()
            && self.checkpoints.selections.is_none()
            && self.prepared_effects.is_empty()
    }

    pub(super) fn consume_nested_mode_call(&mut self) -> Result<(), OperationError> {
        self.budget.consume_nested_mode_call()
    }

    pub(super) fn consume_replayed_inputs(&mut self, count: usize) -> Result<(), OperationError> {
        self.budget.consume_replayed_inputs(count)
    }

    pub(super) fn mode_drafts_mut(&mut self) -> &mut ModeDraftJournal {
        &mut self.mode_drafts
    }

    pub(super) fn record_view_touch(&mut self, view: ViewId, revision: Revision) {
        self.view_touches.entry(view).or_insert(revision);
    }

    pub(super) fn into_parts(self) -> ExecutionFrameParts {
        ExecutionFrameParts {
            checkpoints: self.checkpoints,
            mode_drafts: self.mode_drafts,
            provisional_contents: self.provisional_contents,
            view_touches: self.view_touches,
            effects: self.prepared_effects,
            pending_command: self.pending_command,
        }
    }
}

impl CheckpointJournal {
    pub(super) fn into_parts(
        self,
    ) -> (
        Option<ContentSnapshot>,
        Option<SelectionCheckpoint>,
        Option<InputCheckpoint>,
        Vec<StateRollback>,
    ) {
        (
            self.content,
            self.selections,
            self.input,
            self.state_rollbacks,
        )
    }
}

impl Default for ExecutionBudget {
    fn default() -> Self {
        Self {
            operations: MAX_OPERATIONS_PER_FRAME,
            nested_mode_calls: DEFAULT_NESTED_MODE_BUDGET,
            replayed_inputs: DEFAULT_REPLAYED_INPUT_BUDGET,
        }
    }
}

impl ExecutionBudget {
    fn consume_operation(&mut self) -> Result<(), OperationError> {
        consume(&mut self.operations, || {
            format!("command chain exceeded the limit of {MAX_OPERATIONS_PER_FRAME} commands")
        })
    }

    fn consume_nested_mode_call(&mut self) -> Result<(), OperationError> {
        consume(&mut self.nested_mode_calls, || {
            "nested mode calls exceeded the limit of 256 calls".to_owned()
        })
    }

    fn consume_replayed_inputs(&mut self, count: usize) -> Result<(), OperationError> {
        if count > self.replayed_inputs {
            return Err(OperationError::new(
                "replayed inputs exceeded the limit of 256 inputs",
            ));
        }
        self.replayed_inputs -= count;
        Ok(())
    }
}

fn consume(remaining: &mut usize, message: impl FnOnce() -> String) -> Result<(), OperationError> {
    let Some(next) = remaining.checked_sub(1) else {
        return Err(OperationError::new(message()));
    };
    *remaining = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_owns_the_operation_budget() {
        let mut frame = ExecutionFrame::new(None, None);

        for _ in 0..MAX_OPERATIONS_PER_FRAME {
            frame.consume_operation().unwrap();
        }

        let error = frame.consume_operation().unwrap_err();
        assert!(error.to_string().contains("command chain exceeded"));
    }

    #[test]
    fn frame_owns_nested_mode_and_replay_budgets() {
        let mut frame = ExecutionFrame::new(None, None);

        for _ in 0..DEFAULT_NESTED_MODE_BUDGET {
            frame.consume_nested_mode_call().unwrap();
        }
        assert!(frame.consume_nested_mode_call().is_err());

        frame
            .consume_replayed_inputs(DEFAULT_REPLAYED_INPUT_BUDGET)
            .unwrap();
        assert!(frame.consume_replayed_inputs(1).is_err());
    }

    #[test]
    fn frame_rejects_conflicting_face_contributions_before_publish() {
        let mut frame = ExecutionFrame::new(None, None);
        let first = ResolvedFaceOperation::AddRelative {
            scope: FaceRemapScope::Session,
            face: FaceName::new("ui.editor"),
            token: FaceRemapToken(7),
            expressions: vec![vell_protocol::content_query::FaceExpr::Patch(
                vell_protocol::content_query::FacePatch::default(),
            )],
            owner: FaceRemapOwner::User,
        };
        frame.prepare_face(first.clone()).unwrap();

        assert!(frame.prepare_face(first).is_err());
        assert_eq!(frame.into_parts().effects.len(), 1);
    }
}
