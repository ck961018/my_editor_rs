use std::collections::{HashMap, HashSet};

use crate::{ModeId, ModeViewPolicy};
use modeleaf_core::text_snapshot::TextSnapshot;
use modeleaf_protocol::content_query::{NamedTextDecoration, RowRange};
use modeleaf_protocol::ids::{ContentId, ViewId};
use modeleaf_protocol::revision::Revision;

pub struct ContentPresentationLayer {
    pub source_revision: Revision,
    pub mode_revision: Revision,
    pub decorations: Vec<NamedTextDecoration>,
}

pub struct ViewPresentationLayer {
    pub content_revision: Revision,
    pub view_revision: Revision,
    pub content_mode_revision: Revision,
    pub view_mode_revision: Revision,
    pub policy: ModeViewPolicy,
    pub decorations: Vec<NamedTextDecoration>,
}

#[derive(Default)]
pub struct PresentationLayerStore {
    content_layers: HashMap<(ModeId, ContentId), ContentPresentationLayer>,
    view_layers: HashMap<(ModeId, ViewId), ViewPresentationLayer>,
    view_contents: HashMap<ViewId, ContentId>,
    view_order: HashMap<ViewId, Vec<ModeId>>,
}

impl PresentationLayerStore {
    pub fn begin_refresh(&mut self) {
        self.view_contents.clear();
        self.view_order.clear();
    }

    pub fn set_view(&mut self, view: ViewId, content: ContentId, order: Vec<ModeId>) {
        self.view_contents.insert(view, content);
        self.view_order.insert(view, order);
    }

    pub fn content_is_current(
        &self,
        mode: ModeId,
        content: ContentId,
        source_revision: Revision,
        mode_revision: Revision,
    ) -> bool {
        self.content_layers
            .get(&(mode, content))
            .is_some_and(|layer| {
                layer.source_revision == source_revision && layer.mode_revision == mode_revision
            })
    }

    pub fn set_content_layer(
        &mut self,
        mode: ModeId,
        content: ContentId,
        layer: ContentPresentationLayer,
    ) {
        self.content_layers.insert((mode, content), layer);
    }

    pub fn view_is_current(
        &self,
        mode: ModeId,
        view: ViewId,
        content_revision: Revision,
        view_revision: Revision,
        content_mode_revision: Revision,
        view_mode_revision: Revision,
    ) -> bool {
        self.view_layers.get(&(mode, view)).is_some_and(|layer| {
            layer.content_revision == content_revision
                && layer.view_revision == view_revision
                && layer.content_mode_revision == content_mode_revision
                && layer.view_mode_revision == view_mode_revision
        })
    }

    pub fn set_view_layer(&mut self, mode: ModeId, view: ViewId, layer: ViewPresentationLayer) {
        self.view_layers.insert((mode, view), layer);
    }

    pub fn finish_refresh(
        &mut self,
        content: &HashSet<(ModeId, ContentId)>,
        views: &HashSet<(ModeId, ViewId)>,
    ) {
        self.content_layers.retain(|key, _| content.contains(key));
        self.view_layers.retain(|key, _| views.contains(key));
    }

    pub fn policy(
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

    pub fn decorations(
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn content_layer_count(&self) -> usize {
        self.content_layers.len()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn view_layer_count(&self) -> usize {
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
