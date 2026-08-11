use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::presentation::PresentationLayerStore;
use crate::view::View;
use crate::view_definition::ViewDefinitionRegistry;
use crate::view_workspace::ViewWorkspace;
use vell_core::content_store::ContentStore;
use vell_mode::{
    MAX_VIEW_EXTENSION_DOCUMENT_BYTES, NamedLineSegment, NamedLinesPresentation, ViewExtension,
    ViewExtensionContext, ViewExtensionContractError, ViewExtensionDocument, ViewExtensionId,
    ViewExtensionOwner, ViewExtensionPosition, ViewExtensionPresentation, ViewExtensionSelection,
};
use vell_protocol::content_query::{ContentData, ContentQuery};
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;

pub(super) struct ViewExtensionStore {
    extensions: Vec<Box<dyn ViewExtension>>,
    cache: HashMap<PaneCacheKey, PaneCacheEntry>,
    faulted: HashSet<PaneCacheKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PaneCacheKey {
    extension: ViewExtensionId,
    view: ViewId,
    pane: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PaneCacheEntry {
    view_revision: Revision,
    content_revision: Option<Revision>,
    content: Option<vell_protocol::ids::ContentId>,
    presentation: NamedLinesPresentation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ViewExtensionRegistrationError {
    Duplicate(ViewExtensionId),
    UnknownTarget {
        extension: ViewExtensionId,
        target: vell_protocol::view::ViewDefinitionId,
    },
    Layout(String),
}

impl fmt::Display for ViewExtensionRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(extension) => {
                write!(
                    formatter,
                    "view extension '{extension}' is already registered"
                )
            }
            Self::UnknownTarget { extension, target } => write!(
                formatter,
                "view extension '{extension}' targets unknown View definition '{target}'"
            ),
            Self::Layout(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ViewExtensionRegistrationError {}

impl ViewExtensionStore {
    pub(super) fn empty() -> Self {
        Self {
            extensions: Vec::new(),
            cache: HashMap::new(),
            faulted: HashSet::new(),
        }
    }

    pub(super) fn new(
        extensions: Vec<Box<dyn ViewExtension>>,
        definitions: &ViewDefinitionRegistry,
    ) -> Result<Self, ViewExtensionRegistrationError> {
        let mut ids = HashSet::new();
        for extension in &extensions {
            let definition = extension.definition();
            if !ids.insert(definition.id().clone()) {
                return Err(ViewExtensionRegistrationError::Duplicate(
                    definition.id().clone(),
                ));
            }
            if definitions.get(definition.target()).is_none() {
                return Err(ViewExtensionRegistrationError::UnknownTarget {
                    extension: definition.id().clone(),
                    target: definition.target().clone(),
                });
            }
        }
        Ok(Self {
            extensions,
            cache: HashMap::new(),
            faulted: HashSet::new(),
        })
    }

    pub(super) fn reconcile_workspace(
        &self,
        workspace: &mut ViewWorkspace,
    ) -> Result<(), ViewExtensionRegistrationError> {
        let views = workspace
            .views()
            .iter()
            .map(|(view, data)| (*view, data.definition().clone()))
            .collect::<Vec<_>>();
        for extension in &self.extensions {
            let definition = extension.definition();
            for (view, target) in &views {
                if target != definition.target() {
                    continue;
                }
                for pane in definition.panes() {
                    workspace
                        .install_extension_pane(
                            *view,
                            &definition.pane_key(pane.key()),
                            pane.side(),
                            pane.size(),
                        )
                        .map_err(|error| {
                            ViewExtensionRegistrationError::Layout(error.to_string())
                        })?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.extensions.is_empty()
    }

    pub(super) fn targets_any(
        &self,
        definitions: &HashSet<vell_protocol::view::ViewDefinitionId>,
    ) -> bool {
        self.extensions
            .iter()
            .any(|extension| definitions.contains(extension.definition().target()))
    }

    pub(super) fn pane_keys_for_owner(&self, owner: &ViewExtensionOwner) -> HashSet<String> {
        self.extensions
            .iter()
            .filter(|extension| extension.definition().owner() == owner)
            .flat_map(|extension| {
                let definition = extension.definition();
                definition
                    .panes()
                    .iter()
                    .map(|pane| definition.pane_key(pane.key()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub(super) fn remove_owner(&mut self, owner: &ViewExtensionOwner) -> usize {
        let removed_ids = self
            .extensions
            .iter()
            .filter(|extension| extension.definition().owner() == owner)
            .map(|extension| extension.definition().id().clone())
            .collect::<HashSet<_>>();
        if removed_ids.is_empty() {
            return 0;
        }
        let mut removed = 0;
        self.extensions.retain_mut(|extension| {
            if removed_ids.contains(extension.definition().id()) {
                extension.unload();
                removed += 1;
                false
            } else {
                true
            }
        });
        self.cache
            .retain(|key, _| !removed_ids.contains(&key.extension));
        self.faulted
            .retain(|key| !removed_ids.contains(&key.extension));
        removed
    }

    pub(super) fn refresh(
        &mut self,
        views: &HashMap<ViewId, View>,
        contents: &ContentStore,
        presentation: &mut PresentationLayerStore,
    ) {
        let mut active = HashSet::new();
        for extension in &mut self.extensions {
            let definition = extension.definition().clone();
            for (&view, view_data) in views {
                if view_data.definition() != definition.target() {
                    continue;
                }
                let view_revision = view_data.revision();
                let (content, content_revision) = extension_document_metadata(view_data, contents);
                let mut panes = Vec::new();
                for pane in definition.panes() {
                    let full_key = definition.pane_key(pane.key());
                    if view_data.panes().space_for_key(&full_key).is_none() {
                        continue;
                    }
                    let key = PaneCacheKey {
                        extension: definition.id().clone(),
                        view,
                        pane: pane.key().to_owned(),
                    };
                    active.insert(key.clone());
                    panes.push((key, full_key));
                }
                let needs_context = panes.iter().any(|(key, _)| {
                    !self.faulted.contains(key)
                        && !cache_is_current(&self.cache, key, view_revision, content_revision)
                });
                if needs_context {
                    match extension_context(view, view_data, contents) {
                        Ok(context) => {
                            for (key, _) in &panes {
                                if self.faulted.contains(key)
                                    || cache_is_current(
                                        &self.cache,
                                        key,
                                        view_revision,
                                        content_revision,
                                    )
                                {
                                    continue;
                                }
                                match extension.present(&key.pane, &context).and_then(
                                    |presentation| {
                                        presentation.validate()?;
                                        Ok(presentation)
                                    },
                                ) {
                                    Ok(ViewExtensionPresentation::Lines(lines)) => {
                                        self.cache.insert(
                                            key.clone(),
                                            PaneCacheEntry {
                                                view_revision,
                                                content_revision,
                                                content,
                                                presentation: lines,
                                            },
                                        );
                                    }
                                    Err(error) => {
                                        let message = bounded_message(error.to_string());
                                        self.faulted.insert(key.clone());
                                        self.cache.insert(
                                            key.clone(),
                                            PaneCacheEntry {
                                                view_revision,
                                                content_revision,
                                                content,
                                                presentation: fault_presentation(&message),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            let message = bounded_message(error.to_string());
                            for (key, _) in &panes {
                                if self.faulted.contains(key)
                                    || cache_is_current(
                                        &self.cache,
                                        key,
                                        view_revision,
                                        content_revision,
                                    )
                                {
                                    continue;
                                }
                                self.faulted.insert(key.clone());
                                self.cache.insert(
                                    key.clone(),
                                    PaneCacheEntry {
                                        view_revision,
                                        content_revision,
                                        content,
                                        presentation: fault_presentation(&message),
                                    },
                                );
                            }
                        }
                    }
                }
                for (key, full_key) in panes {
                    if let Some(entry) = self.cache.get(&key) {
                        presentation.set_extension_pane(
                            view,
                            full_key,
                            entry.content,
                            entry.presentation.clone(),
                        );
                    }
                }
            }
        }
        self.cache.retain(|key, _| active.contains(key));
        self.faulted.retain(|key| active.contains(key));
    }
}

fn cache_is_current(
    cache: &HashMap<PaneCacheKey, PaneCacheEntry>,
    key: &PaneCacheKey,
    view_revision: Revision,
    content_revision: Option<Revision>,
) -> bool {
    cache.get(key).is_some_and(|entry| {
        entry.view_revision == view_revision && entry.content_revision == content_revision
    })
}

fn extension_document_metadata(
    data: &View,
    contents: &ContentStore,
) -> (Option<ContentId>, Option<Revision>) {
    let Some(content) = data.document_content() else {
        return (None, None);
    };
    let Some(revision) = contents.revision(content) else {
        return (None, None);
    };
    (Some(content), Some(revision))
}

fn extension_context(
    view: ViewId,
    data: &View,
    contents: &ContentStore,
) -> Result<ViewExtensionContext, ViewExtensionContractError> {
    let document = extension_document(data, contents)?;
    Ok(ViewExtensionContext {
        view_id: view,
        definition: data.definition().clone(),
        revision: data.revision(),
        bindings: data
            .bindings()
            .iter()
            .map(|(binding, content)| (binding.clone(), content))
            .collect(),
        document,
    })
}

fn extension_document(
    data: &View,
    contents: &ContentStore,
) -> Result<Option<ViewExtensionDocument>, ViewExtensionContractError> {
    let Some((content, state)) = data.document() else {
        return Ok(None);
    };
    let (Some(revision), Some(snapshot), Some(selections)) = (
        contents.revision(content),
        contents.text_snapshot(content),
        state.selections(),
    ) else {
        return Ok(None);
    };
    let text_bytes = snapshot.len_bytes();
    if text_bytes > MAX_VIEW_EXTENSION_DOCUMENT_BYTES {
        return Err(ViewExtensionContractError::new(format!(
            "View extension document has {text_bytes} text bytes; maximum is \
             {MAX_VIEW_EXTENSION_DOCUMENT_BYTES}"
        )));
    }
    let primary_selection = extension_selection(&snapshot, selections.primary())?;
    let selections = selections
        .all()
        .map(|selection| extension_selection(&snapshot, selection))
        .collect::<Result<Vec<_>, ViewExtensionContractError>>()?;
    let resource_name = match contents.query(content, ContentQuery::ResourceName) {
        ContentData::ResourceName(name) => name,
        _ => None,
    };
    Ok(Some(ViewExtensionDocument {
        content_id: content,
        revision,
        text: snapshot.to_owned_string(),
        resource_name,
        selections,
        primary_selection,
    }))
}

fn extension_selection(
    snapshot: &vell_core::text_snapshot::TextSnapshot,
    selection: &vell_protocol::selection::Selection,
) -> Result<ViewExtensionSelection, ViewExtensionContractError> {
    Ok(ViewExtensionSelection {
        anchor: extension_position(snapshot, selection.anchor.char_index)?,
        head: extension_position(snapshot, selection.head.char_index)?,
    })
}

fn extension_position(
    snapshot: &vell_core::text_snapshot::TextSnapshot,
    char_offset: usize,
) -> Result<ViewExtensionPosition, ViewExtensionContractError> {
    let (line, character) = snapshot
        .char_to_utf16_position(char_offset)
        .ok_or_else(|| ViewExtensionContractError::new("View extension selection is invalid"))?;
    Ok(ViewExtensionPosition { line, character })
}

fn bounded_message(message: String) -> String {
    message.chars().take(512).collect()
}

fn fault_presentation(message: &str) -> NamedLinesPresentation {
    NamedLinesPresentation {
        base_face: None,
        rows: vec![vec![NamedLineSegment {
            text: format!("extension fault: {message}"),
            face: None,
        }]],
    }
}
