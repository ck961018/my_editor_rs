use std::collections::HashMap;

use crate::protocol::key_event::KeyEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "leader aliases are retained for runtime keymap configuration"
    )
)]
pub enum KeyStroke {
    Key(KeyEvent),
    Leader,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "leader expansion is retained for runtime keymap configuration"
    )
)]
pub fn expand_key_sequence(sequence: &[KeyStroke], leader: KeyEvent) -> Vec<KeyEvent> {
    sequence
        .iter()
        .map(|stroke| match stroke {
            KeyStroke::Key(key) => *key,
            KeyStroke::Leader => leader,
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyNode<A> {
    action: Option<A>,
    children: HashMap<KeyEvent, KeyNode<A>>,
}

impl<A> Default for KeyNode<A> {
    fn default() -> Self {
        Self {
            action: None,
            children: HashMap::new(),
        }
    }
}

impl<A> KeyNode<A> {
    pub fn action(&self) -> Option<&A> {
        self.action.as_ref()
    }

    pub fn children(&self) -> &HashMap<KeyEvent, KeyNode<A>> {
        &self.children
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap<A> {
    roots: HashMap<KeyEvent, KeyNode<A>>,
}

impl<A> Default for Keymap<A> {
    fn default() -> Self {
        Self {
            roots: HashMap::new(),
        }
    }
}

impl<A> Keymap<A> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn node(&self, sequence: &[KeyEvent]) -> Option<&KeyNode<A>> {
        let (first, rest) = sequence.split_first()?;
        let mut node = self.roots.get(first)?;
        for key in rest {
            node = node.children.get(key)?;
        }
        Some(node)
    }

    pub fn bind(&mut self, sequence: impl AsRef<[KeyEvent]>, action: A) -> Option<A> {
        let sequence = sequence.as_ref();
        let (first, rest) = sequence
            .split_first()
            .expect("key sequence must contain at least one key");
        let mut node = self.roots.entry(*first).or_default();
        for key in rest {
            node = node.children.entry(*key).or_default();
        }
        node.action.replace(action)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "unbind is retained for runtime keymap configuration"
        )
    )]
    pub fn unbind(&mut self, sequence: impl AsRef<[KeyEvent]>) -> Option<A> {
        let sequence = sequence.as_ref();
        let (first, rest) = sequence.split_first()?;
        let removed = remove_action(self.roots.get_mut(first)?, rest);
        if self.roots.get(first).is_some_and(node_is_empty) {
            self.roots.remove(first);
        }
        removed
    }
}

fn remove_action<A>(node: &mut KeyNode<A>, rest: &[KeyEvent]) -> Option<A> {
    let Some((key, tail)) = rest.split_first() else {
        return node.action.take();
    };
    let removed = remove_action(node.children.get_mut(key)?, tail);
    if node.children.get(key).is_some_and(node_is_empty) {
        node.children.remove(key);
    }
    removed
}

fn node_is_empty<A>(node: &KeyNode<A>) -> bool {
    node.action.is_none() && node.children.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::key_event::{ArrowKey, KeyCode};

    #[test]
    fn node_can_hold_an_action_and_children() {
        let g = KeyEvent::char('g');
        let mut keymap = Keymap::new();
        keymap.bind([g], 1);
        keymap.bind([g, g], 2);

        let node = keymap.node(&[g]).unwrap();
        assert_eq!(node.action(), Some(&1));
        assert!(node.children().contains_key(&g));
        assert_eq!(keymap.node(&[g, g]).unwrap().action(), Some(&2));
    }

    #[test]
    fn rebinding_replaces_only_the_action() {
        let g = KeyEvent::char('g');
        let mut keymap = Keymap::new();
        keymap.bind([g], 1);
        keymap.bind([g, g], 2);

        assert_eq!(keymap.bind([g], 3), Some(1));
        assert_eq!(keymap.node(&[g]).unwrap().action(), Some(&3));
        assert_eq!(keymap.node(&[g, g]).unwrap().action(), Some(&2));
    }

    #[test]
    fn unbind_keeps_descendants_and_prunes_empty_nodes() {
        let g = KeyEvent::char('g');
        let mut keymap = Keymap::new();
        keymap.bind([g], 1);
        keymap.bind([g, g], 2);

        assert_eq!(keymap.unbind([g]), Some(1));
        assert!(keymap.node(&[g]).unwrap().action().is_none());
        assert_eq!(keymap.unbind([g, g]), Some(2));
        assert!(keymap.node(&[g]).is_none());
    }

    #[test]
    fn generic_binding_preserves_the_action() {
        let mut keymap = Keymap::new();
        let enter = KeyEvent::plain(KeyCode::Enter);
        keymap.bind([enter], "insert-newline");

        assert_eq!(
            keymap.node(&[enter]).and_then(KeyNode::action),
            Some(&"insert-newline")
        );
    }

    #[test]
    fn leader_is_expanded_when_a_binding_is_defined() {
        let leader = KeyEvent::char(' ');
        let sequence = expand_key_sequence(
            &[KeyStroke::Leader, KeyStroke::Key(KeyEvent::char('s'))],
            leader,
        );
        let mut keymap = Keymap::new();
        keymap.bind(sequence, 7);

        assert_eq!(
            keymap
                .node(&[leader, KeyEvent::char('s')])
                .and_then(KeyNode::action),
            Some(&7)
        );
    }

    #[test]
    fn keymap_clone_eq() {
        let mut keymap = Keymap::new();
        keymap.bind([KeyEvent::arrow(ArrowKey::Left)], 1);
        assert_eq!(keymap, keymap.clone());
    }
}
