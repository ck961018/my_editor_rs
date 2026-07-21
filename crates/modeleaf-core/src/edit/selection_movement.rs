use crate::core::buffer::Buffer;
use crate::protocol::selection::{Selection, Selections};

#[derive(Clone, Copy)]
pub(super) enum CollapseEdge {
    Lower,
    Upper,
}

pub(super) fn collapse_or_move(
    buffer: &Buffer,
    selections: &mut Selections,
    edge: CollapseEdge,
    mut movement: impl FnMut(&Buffer, &mut Selection),
) {
    for selection in selections.all_mut() {
        if selection.anchor == selection.head {
            movement(buffer, selection);
        } else {
            collapse_to_edge(selection, edge);
        }
        Buffer::collapse_to_head(selection);
    }
}

pub(super) fn move_and_collapse(
    buffer: &Buffer,
    selections: &mut Selections,
    mut movement: impl FnMut(&Buffer, &mut Selection),
) {
    for selection in selections.all_mut() {
        movement(buffer, selection);
        Buffer::collapse_to_head(selection);
    }
}

pub(super) fn extend(
    buffer: &Buffer,
    selections: &mut Selections,
    mut movement: impl FnMut(&Buffer, &mut Selection),
) {
    for selection in selections.all_mut() {
        movement(buffer, selection);
    }
}

pub(super) fn collapse_all(selections: &mut Selections) {
    for selection in selections.all_mut() {
        Buffer::collapse_to_head(selection);
    }
}

fn collapse_to_edge(selection: &mut Selection, edge: CollapseEdge) {
    let anchor_is_edge = match edge {
        CollapseEdge::Lower => selection.anchor.char_index < selection.head.char_index,
        CollapseEdge::Upper => selection.anchor.char_index > selection.head.char_index,
    };
    if anchor_is_edge {
        selection.head = selection.anchor;
    }
}
