use std::collections::{HashMap, HashSet};

use crate::app::mode::{ModeId, ModeViewPolicy};
use crate::core::text_snapshot::TextSnapshot;
use crate::protocol::content_query::{NamedTextDecoration, RowRange};
use crate::protocol::ids::{ContentId, ViewId};
use crate::protocol::revision::Revision;

pub(crate) struct ContentPresentationLayer {
    pub(crate) source_revision: Revision,
    pub(crate) decorations: Vec<NamedTextDecoration>,
}

pub(crate) struct ViewPresentationLayer {
    pub(crate) content_revision: Revision,
    pub(crate) view_revision: Revision,
    pub(crate) policy: ModeViewPolicy,
    pub(crate) decorations: Vec<NamedTextDecoration>,
}

#[derive(Default)]
pub(crate) struct PresentationLayerStore {
    content_layers: HashMap<(ModeId, ContentId), ContentPresentationLayer>,
    view_layers: HashMap<(ModeId, ViewId), ViewPresentationLayer>,
    view_contents: HashMap<ViewId, ContentId>,
    view_order: HashMap<ViewId, Vec<ModeId>>,
}

impl PresentationLayerStore {
    pub(crate) fn replace(
        &mut self,
        content_layers: HashMap<(ModeId, ContentId), ContentPresentationLayer>,
        view_layers: HashMap<(ModeId, ViewId), ViewPresentationLayer>,
        view_contents: HashMap<ViewId, ContentId>,
        view_order: HashMap<ViewId, Vec<ModeId>>,
    ) {
        self.content_layers = content_layers;
        self.view_layers = view_layers;
        self.view_contents = view_contents;
        self.view_order = view_order;
    }

    pub(crate) fn policy(
        &self,
        view: ViewId,
        content_revision: Revision,
        view_revision: Revision,
    ) -> ModeViewPolicy {
        let mut policy = ModeViewPolicy::default();
        for mode in self.view_order.get(&view).into_iter().flatten() {
            let Some(layer) = self.view_layers.get(&(*mode, view)) else {
                continue;
            };
            if layer.content_revision != content_revision || layer.view_revision != view_revision {
                continue;
            }
            policy.merge_missing(layer.policy.clone());
        }
        policy
    }

    pub(crate) fn decorations(
        &self,
        view: ViewId,
        content_revision: Revision,
        view_revision: Revision,
        snapshot: &TextSnapshot,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        let Some(content) = self.view_contents.get(&view).copied() else {
            return Vec::new();
        };
        let Some(order) = self.view_order.get(&view) else {
            return Vec::new();
        };
        let mut decorations = Vec::new();
        for mode in order.iter().rev() {
            if let Some(layer) = self.content_layers.get(&(*mode, content))
                && layer.source_revision == content_revision
            {
                decorations.extend(visible_decorations(
                    &layer.decorations,
                    snapshot,
                    visible_rows,
                ));
            }
            if let Some(layer) = self.view_layers.get(&(*mode, view))
                && layer.content_revision == content_revision
                && layer.view_revision == view_revision
            {
                decorations.extend(visible_decorations(
                    &layer.decorations,
                    snapshot,
                    visible_rows,
                ));
            }
        }
        decorations
    }

    #[cfg(test)]
    pub(crate) fn content_layer_count(&self) -> usize {
        self.content_layers.len()
    }

    #[cfg(test)]
    pub(crate) fn view_layer_count(&self) -> usize {
        self.view_layers.len()
    }
}

fn visible_decorations(
    decorations: &[NamedTextDecoration],
    snapshot: &TextSnapshot,
    visible_rows: RowRange,
) -> Vec<NamedTextDecoration> {
    let range = snapshot.char_range_for_rows(visible_rows.start, visible_rows.end);
    if range.is_empty() {
        return if visible_rows.start == 0 && visible_rows.end > 0 {
            decorations.to_vec()
        } else {
            Vec::new()
        };
    }
    decorations
        .iter()
        .filter(|decoration| {
            decoration.start.char_index < range.end && decoration.end.char_index > range.start
        })
        .cloned()
        .collect()
}

pub(crate) struct PresentationRefresh {
    pub(crate) content_layers: HashMap<(ModeId, ContentId), ContentPresentationLayer>,
    pub(crate) view_layers: HashMap<(ModeId, ViewId), ViewPresentationLayer>,
    pub(crate) view_contents: HashMap<ViewId, ContentId>,
    pub(crate) view_order: HashMap<ViewId, Vec<ModeId>>,
    refreshed_content: HashSet<(ModeId, ContentId)>,
}

impl PresentationRefresh {
    pub(crate) fn new() -> Self {
        Self {
            content_layers: HashMap::new(),
            view_layers: HashMap::new(),
            view_contents: HashMap::new(),
            view_order: HashMap::new(),
            refreshed_content: HashSet::new(),
        }
    }

    pub(crate) fn needs_content(&mut self, mode: ModeId, content: ContentId) -> bool {
        self.refreshed_content.insert((mode, content))
    }
}
