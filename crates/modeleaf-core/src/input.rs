use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::core::keymap::Keymap;
use crate::protocol::key_event::KeyEvent;

#[allow(
    dead_code,
    reason = "dynamic modes may opt into host-managed pending input"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeoutPolicy {
    After(Duration),
    Never,
}

#[allow(
    dead_code,
    reason = "dynamic modes may opt into host-managed pending input"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputStatus {
    Ready,
    Awaiting(TimeoutPolicy),
}

#[allow(
    dead_code,
    reason = "dynamic modes may capture, consume, or emit raw input"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputDecision<A> {
    Pass,
    Consumed,
    Emit(A),
}

#[derive(Clone, Debug)]
pub struct KeySequenceConfig {
    default_timeout: Duration,
    overrides: HashMap<Vec<KeyEvent>, TimeoutPolicy>,
}

impl KeySequenceConfig {
    pub fn new(default_timeout: Duration) -> Self {
        Self {
            default_timeout,
            overrides: HashMap::new(),
        }
    }

    pub fn set_override(&mut self, sequence: impl AsRef<[KeyEvent]>, timeout: TimeoutPolicy) {
        let sequence = sequence.as_ref();
        assert!(!sequence.is_empty(), "timeout prefix must not be empty");
        self.overrides.insert(sequence.to_vec(), timeout);
    }

    pub fn timeout_for(&self, sequence: &[KeyEvent]) -> TimeoutPolicy {
        let mut timeout = TimeoutPolicy::After(self.default_timeout);
        for length in 1..=sequence.len() {
            if let Some(configured) = self.overrides.get(&sequence[..length]) {
                timeout = *configured;
            }
        }
        timeout
    }

    pub fn deadline(&self, sequence: &[KeyEvent], now: Instant) -> Option<Instant> {
        match self.timeout_for(sequence) {
            TimeoutPolicy::After(duration) => now.checked_add(duration),
            TimeoutPolicy::Never => None,
        }
    }
}

#[derive(Clone, Copy)]
pub struct KeymapLayer<'a, A, S> {
    pub source: S,
    pub keymap: &'a dyn KeymapLookup<A>,
}

pub trait KeymapLookup<A> {
    fn lookup(&self, sequence: &[KeyEvent]) -> Option<(Option<A>, bool)>;
    fn extend_continuations(&self, sequence: &[KeyEvent], continuations: &mut HashSet<KeyEvent>);
}

impl<A: Clone> KeymapLookup<A> for Keymap<A> {
    fn lookup(&self, sequence: &[KeyEvent]) -> Option<(Option<A>, bool)> {
        let node = self.node(sequence)?;
        Some((node.action().cloned(), !node.children().is_empty()))
    }

    fn extend_continuations(&self, sequence: &[KeyEvent], continuations: &mut HashSet<KeyEvent>) {
        if let Some(node) = self.node(sequence) {
            continuations.extend(node.children().keys().copied());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAction<A, S> {
    pub action: A,
    pub source: S,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceMatch<A, S> {
    pub exact: Option<ResolvedAction<A, S>>,
    pub has_children: bool,
}

pub fn match_sequence<A, S: Clone>(
    layers: &[KeymapLayer<'_, A, S>],
    sequence: &[KeyEvent],
) -> Option<SequenceMatch<A, S>> {
    let mut exact = None;
    let mut has_children = false;
    let mut matched = false;
    for layer in layers {
        let Some((action, node_has_children)) = layer.keymap.lookup(sequence) else {
            continue;
        };
        matched = true;
        has_children |= node_has_children;
        if exact.is_none()
            && let Some(action) = action
        {
            exact = Some(ResolvedAction {
                action,
                source: layer.source.clone(),
            });
        }
    }
    matched.then_some(SequenceMatch {
        exact,
        has_children,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompleteMatch<A, S> {
    pub consumed: usize,
    pub resolved: ResolvedAction<A, S>,
}

pub fn longest_complete<A, S: Clone>(
    layers: &[KeymapLayer<'_, A, S>],
    sequence: &[KeyEvent],
) -> Option<CompleteMatch<A, S>> {
    for consumed in (1..=sequence.len()).rev() {
        if let Some(resolved) =
            match_sequence(layers, &sequence[..consumed]).and_then(|matched| matched.exact)
        {
            return Some(CompleteMatch { consumed, resolved });
        }
    }
    None
}

pub fn continuations<A, S>(
    layers: &[KeymapLayer<'_, A, S>],
    sequence: &[KeyEvent],
) -> HashSet<KeyEvent> {
    let mut continuations = HashSet::new();
    for layer in layers {
        layer
            .keymap
            .extend_continuations(sequence, &mut continuations);
    }
    continuations
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSequence<S> {
    pub owner: S,
    pub keys: Vec<KeyEvent>,
    pub deadline: Option<Instant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AwaitingEntry<S> {
    Context { source: S, idle_since: Instant },
    KeySequence(PendingSequence<S>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AwaitingSource<S> {
    Context(S),
    KeySequence,
}

#[derive(Clone, Debug)]
pub struct InputCoordinator<S> {
    awaiting: Vec<AwaitingEntry<S>>,
}

impl<S> Default for InputCoordinator<S> {
    fn default() -> Self {
        Self {
            awaiting: Vec::new(),
        }
    }
}

impl<S: Clone + PartialEq> InputCoordinator<S> {
    #[cfg(test)]
    pub fn sources_top_down(&self) -> Vec<AwaitingSource<S>> {
        self.awaiting
            .iter()
            .rev()
            .map(|entry| match entry {
                AwaitingEntry::Context { source, .. } => AwaitingSource::Context(source.clone()),
                AwaitingEntry::KeySequence(_) => AwaitingSource::KeySequence,
            })
            .collect()
    }

    pub fn sync_context(&mut self, source: S, status: InputStatus, handled: bool, now: Instant) {
        let existing = self.awaiting.iter().position(
            |entry| matches!(entry, AwaitingEntry::Context { source: item, .. } if item == &source),
        );
        match (status, existing) {
            (InputStatus::Ready, Some(index)) => {
                self.awaiting.remove(index);
            }
            (InputStatus::Ready, None) => {}
            (InputStatus::Awaiting(_), Some(index)) => {
                if handled
                    && let AwaitingEntry::Context { idle_since, .. } = &mut self.awaiting[index]
                {
                    *idle_since = now;
                }
            }
            (InputStatus::Awaiting(_), None) => {
                self.awaiting.push(AwaitingEntry::Context {
                    source,
                    idle_since: now,
                });
            }
        }
    }

    pub fn remove_context(&mut self, source: &S) {
        self.awaiting.retain(
            |entry| !matches!(entry, AwaitingEntry::Context { source: item, .. } if item == source),
        );
    }

    pub fn remove_contexts(&mut self, mut remove: impl FnMut(&S) -> bool) {
        self.awaiting.retain(|entry| match entry {
            AwaitingEntry::Context { source, .. } => !remove(source),
            AwaitingEntry::KeySequence(_) => true,
        });
    }

    pub fn push_sequence(&mut self, pending: PendingSequence<S>) {
        debug_assert!(self.pending_sequence().is_none());
        self.awaiting.push(AwaitingEntry::KeySequence(pending));
    }

    pub fn pending_sequence(&self) -> Option<&PendingSequence<S>> {
        self.awaiting.iter().find_map(|entry| match entry {
            AwaitingEntry::KeySequence(pending) => Some(pending),
            AwaitingEntry::Context { .. } => None,
        })
    }

    pub fn pending_sequence_mut(&mut self) -> Option<&mut PendingSequence<S>> {
        self.awaiting.iter_mut().find_map(|entry| match entry {
            AwaitingEntry::KeySequence(pending) => Some(pending),
            AwaitingEntry::Context { .. } => None,
        })
    }

    pub fn take_sequence(&mut self) -> Option<PendingSequence<S>> {
        let index = self
            .awaiting
            .iter()
            .position(|entry| matches!(entry, AwaitingEntry::KeySequence(_)))?;
        match self.awaiting.remove(index) {
            AwaitingEntry::KeySequence(pending) => Some(pending),
            AwaitingEntry::Context { .. } => unreachable!(),
        }
    }

    pub fn discard_sequence(&mut self) {
        let _ = self.take_sequence();
    }

    pub fn next_deadline(&self, mut status: impl FnMut(&S) -> InputStatus) -> Option<Instant> {
        self.awaiting
            .iter()
            .filter_map(|entry| entry_deadline(entry, &mut status))
            .min()
    }

    pub fn next_due(
        &self,
        now: Instant,
        mut status: impl FnMut(&S) -> InputStatus,
    ) -> Option<AwaitingSource<S>> {
        let mut selected: Option<(Instant, AwaitingSource<S>)> = None;
        for entry in self.awaiting.iter().rev() {
            let Some(deadline) = entry_deadline(entry, &mut status) else {
                continue;
            };
            if deadline > now {
                continue;
            }
            if selected
                .as_ref()
                .is_none_or(|(current, _)| deadline < *current)
            {
                let source = match entry {
                    AwaitingEntry::Context { source, .. } => {
                        AwaitingSource::Context(source.clone())
                    }
                    AwaitingEntry::KeySequence(_) => AwaitingSource::KeySequence,
                };
                selected = Some((deadline, source));
            }
        }
        selected.map(|(_, source)| source)
    }
}

fn entry_deadline<S>(
    entry: &AwaitingEntry<S>,
    status: &mut impl FnMut(&S) -> InputStatus,
) -> Option<Instant> {
    match entry {
        AwaitingEntry::Context { source, idle_since } => match status(source) {
            InputStatus::Awaiting(TimeoutPolicy::After(duration)) => {
                idle_since.checked_add(duration)
            }
            InputStatus::Ready | InputStatus::Awaiting(TimeoutPolicy::Never) => None,
        },
        AwaitingEntry::KeySequence(pending) => pending.deadline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_union_children_but_keep_earlier_exact_action() {
        let g = KeyEvent::char('g');
        let x = KeyEvent::char('x');
        let mut mode = Keymap::new();
        mode.bind([g], "mode-g");
        mode.bind([g, g], "mode-gg");
        let mut global = Keymap::new();
        global.bind([g], "global-g");
        global.bind([g, x], "global-gx");
        let layers = [
            KeymapLayer {
                source: "mode",
                keymap: &mode,
            },
            KeymapLayer {
                source: "global",
                keymap: &global,
            },
        ];

        let matched = match_sequence(&layers, &[g]).unwrap();
        assert_eq!(matched.exact.unwrap().action, "mode-g");
        assert!(matched.has_children);
        assert_eq!(continuations(&layers, &[g]), HashSet::from([g, x]));
    }

    #[test]
    fn longest_complete_prefers_the_most_consumed_keys() {
        let g = KeyEvent::char('g');
        let a = KeyEvent::char('a');
        let q = KeyEvent::char('q');
        let mut keymap = Keymap::new();
        keymap.bind([g], 1);
        keymap.bind([g, a, q], 3);
        let layers = [KeymapLayer {
            source: (),
            keymap: &keymap,
        }];

        let complete = longest_complete(&layers, &[g, a]).unwrap();
        assert_eq!(complete.consumed, 1);
        assert_eq!(complete.resolved.action, 1);
    }

    #[test]
    fn nearest_explicit_timeout_is_inherited() {
        let g = KeyEvent::char('g');
        let a = KeyEvent::char('a');
        let q = KeyEvent::char('q');
        let mut config = KeySequenceConfig::new(Duration::from_secs(1));
        config.set_override([g], TimeoutPolicy::After(Duration::from_secs(2)));
        config.set_override([g, a], TimeoutPolicy::Never);

        assert_eq!(config.timeout_for(&[g, a, q]), TimeoutPolicy::Never);
        assert_eq!(
            config.timeout_for(&[g, q]),
            TimeoutPolicy::After(Duration::from_secs(2))
        );
    }

    #[test]
    fn newest_awaiting_entry_is_first_and_pass_does_not_reset_idle_time() {
        let start = Instant::now();
        let mut coordinator = InputCoordinator::default();
        coordinator.sync_context(
            "vim",
            InputStatus::Awaiting(TimeoutPolicy::After(Duration::from_secs(2))),
            false,
            start,
        );
        coordinator.push_sequence(PendingSequence {
            owner: "vim",
            keys: vec![KeyEvent::char('g')],
            deadline: None,
        });
        coordinator.sync_context(
            "vim",
            InputStatus::Awaiting(TimeoutPolicy::After(Duration::from_secs(1))),
            false,
            start + Duration::from_millis(500),
        );

        assert_eq!(
            coordinator.sources_top_down(),
            vec![AwaitingSource::KeySequence, AwaitingSource::Context("vim")]
        );
        assert_eq!(
            coordinator.next_deadline(|_| InputStatus::Awaiting(TimeoutPolicy::After(
                Duration::from_secs(1)
            ))),
            Some(start + Duration::from_secs(1))
        );
    }
}
