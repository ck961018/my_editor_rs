use std::collections::HashMap;

use crate::dispatcher::DispatcherInputSnapshot;
use crate::mode::ModeDraftJournal;
use crate::operation::OperationError;
use crate::transaction::TransactionRecord;
use vell_core::content::SaveSnapshot;
use vell_core::content_store::ContentSnapshot;
use vell_core::transaction::TransactionDirection;
use vell_mode::operation::MAX_OPERATIONS_PER_FRAME;
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::Selections;
use vell_protocol::space::SplitDirection;
use vell_protocol::viewport::ResolvedViewportCommand;

const DEFAULT_NESTED_MODE_BUDGET: usize = 256;
const DEFAULT_REPLAYED_INPUT_BUDGET: usize = 256;

pub(super) struct ExecutionFrame {
    checkpoints: CheckpointJournal,
    mode_drafts: ModeDraftJournal,
    view_touches: HashMap<ViewId, Revision>,
    prepared_effects: Vec<PreparedEffect>,
    topology_effect_prepared: bool,
    viewport_effect_prepared: bool,
    budget: ExecutionBudget,
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
            view_touches: HashMap::new(),
            prepared_effects: Vec::new(),
            topology_effect_prepared: false,
            viewport_effect_prepared: false,
            budget: ExecutionBudget::default(),
        }
    }

    pub(super) fn prepare(&mut self, effect: PreparedEffect) {
        self.prepared_effects.push(effect);
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

    pub(super) fn into_parts(
        self,
    ) -> (
        CheckpointJournal,
        ModeDraftJournal,
        HashMap<ViewId, Revision>,
        Vec<PreparedEffect>,
    ) {
        (
            self.checkpoints,
            self.mode_drafts,
            self.view_touches,
            self.prepared_effects,
        )
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
}
