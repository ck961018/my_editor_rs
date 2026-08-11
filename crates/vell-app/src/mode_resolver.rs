use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use crate::content_classifier::ContentClassifier;
use crate::mode::{ModeAttachmentError, ModeRegistry};
use crate::mode_name::ModeName;
use crate::view::View;
use vell_core::content_store::ContentStore;
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::view::BindingKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ModeOverride {
    Enable,
    Disable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ModeAttachmentSpec {
    pub mode: ModeName,
    pub binding: Option<BindingKey>,
    pub content: Option<ContentId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ModeAttachmentPlan {
    pub view: ViewId,
    pub binding_revision: Revision,
    pub entries: Vec<ModeAttachmentSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ModeResolutionError {
    UnknownBefore {
        mode: ModeName,
        before: ModeName,
    },
    Cycle {
        blocked: Vec<ModeName>,
    },
    MissingContent {
        view: ViewId,
        binding: BindingKey,
        content: ContentId,
    },
    UnknownOrderOverride {
        view: ViewId,
        mode: ModeName,
    },
    DuplicateOrderOverride {
        view: ViewId,
        mode: ModeName,
    },
}

impl fmt::Display for ModeResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBefore { mode, before } => write!(
                formatter,
                "mode '{}' orders before unknown mode '{}'",
                mode.as_str(),
                before.as_str()
            ),
            Self::Cycle { blocked } => write!(
                formatter,
                "mode attachment ordering contains a cycle: {}",
                blocked
                    .iter()
                    .map(ModeName::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::MissingContent {
                view,
                binding,
                content,
            } => write!(
                formatter,
                "view {} binding {binding} references missing content {}",
                view.0, content.0
            ),
            Self::UnknownOrderOverride { view, mode } => write!(
                formatter,
                "view {} orders unknown mode '{}'",
                view.0,
                mode.as_str()
            ),
            Self::DuplicateOrderOverride { view, mode } => write!(
                formatter,
                "view {} orders mode '{}' more than once",
                view.0,
                mode.as_str()
            ),
        }
    }
}

impl std::error::Error for ModeResolutionError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AttachmentPlanError {
    Resolution(ModeResolutionError),
    StaleBindings {
        view: ViewId,
        expected: Revision,
        actual: Revision,
    },
    UnsupportedBinding {
        mode: ModeName,
        binding: Option<BindingKey>,
    },
    DuplicateMode(ModeName),
    Attachment(ModeAttachmentError),
}

impl fmt::Display for AttachmentPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolution(error) => error.fmt(formatter),
            Self::StaleBindings {
                view,
                expected,
                actual,
            } => write!(
                formatter,
                "view {} binding revision changed from {} to {}",
                view.0, expected.0, actual.0
            ),
            Self::UnsupportedBinding { mode, binding } => match binding {
                Some(binding) => write!(
                    formatter,
                    "mode '{}' cannot attach to non-document binding {binding} yet",
                    mode.as_str()
                ),
                None => write!(
                    formatter,
                    "mode '{}' has an inconsistent View-only attachment",
                    mode.as_str()
                ),
            },
            Self::DuplicateMode(mode) => write!(
                formatter,
                "attachment plan contains mode '{}' more than once",
                mode.as_str()
            ),
            Self::Attachment(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AttachmentPlanError {}

impl From<ModeResolutionError> for AttachmentPlanError {
    fn from(error: ModeResolutionError) -> Self {
        Self::Resolution(error)
    }
}

impl From<ModeAttachmentError> for AttachmentPlanError {
    fn from(error: ModeAttachmentError) -> Self {
        Self::Attachment(error)
    }
}

#[derive(Clone)]
pub(super) struct ModeResolver {
    content_overrides: HashMap<(ContentId, ModeName), ModeOverride>,
    view_overrides: HashMap<(ViewId, ModeName), ModeOverride>,
    view_order_overrides: HashMap<ViewId, Vec<ModeName>>,
}

impl ModeResolver {
    pub(super) fn new(registry: &ModeRegistry) -> Result<Self, ModeResolutionError> {
        resolve_order(registry)?;
        Ok(Self {
            content_overrides: HashMap::new(),
            view_overrides: HashMap::new(),
            view_order_overrides: HashMap::new(),
        })
    }

    pub(super) fn resolve(
        &self,
        view_id: ViewId,
        view: &View,
        registry: &ModeRegistry,
        classifier: &ContentClassifier,
        contents: &ContentStore,
    ) -> Result<ModeAttachmentPlan, ModeResolutionError> {
        let ordered_modes = resolve_order(registry)?;
        let definitions = registry
            .resolution_definitions()
            .map(|definition| (definition.name().clone(), definition))
            .collect::<HashMap<_, _>>();
        let mut entries = Vec::new();
        for name in &ordered_modes {
            let definition = definitions
                .get(name)
                .expect("resolver order references registered mode");
            let rule = definition.attachment();
            if rule.view() != view.definition() {
                continue;
            }
            let (binding, content, default_enabled) = match rule.binding() {
                Some(binding) => {
                    let Some(content) = view.binding(binding.as_str()) else {
                        continue;
                    };
                    let classification =
                        classifier.classify(content, contents).ok_or_else(|| {
                            ModeResolutionError::MissingContent {
                                view: view_id,
                                binding: binding.clone(),
                                content,
                            }
                        })?;
                    let structural_match = registry
                        .resolve_mode(name)
                        .is_some_and(|mode| registry.adapter(mode, classification.kind).is_some());
                    if !structural_match {
                        continue;
                    }
                    (
                        Some(binding.clone()),
                        Some(content),
                        rule.matches_language(classification.language.as_ref()),
                    )
                }
                None => (None, None, rule.languages().is_none()),
            };
            let enabled = self
                .view_overrides
                .get(&(view_id, name.clone()))
                .or_else(|| {
                    content.and_then(|content| self.content_overrides.get(&(content, name.clone())))
                })
                .map_or(default_enabled, |value| *value == ModeOverride::Enable);
            if enabled {
                entries.push(ModeAttachmentSpec {
                    mode: name.clone(),
                    binding,
                    content,
                });
            }
        }
        if let Some(order) = self.view_order_overrides.get(&view_id) {
            let mut seen = HashSet::new();
            for mode in order {
                if !definitions.contains_key(mode) {
                    return Err(ModeResolutionError::UnknownOrderOverride {
                        view: view_id,
                        mode: mode.clone(),
                    });
                }
                if !seen.insert(mode.clone()) {
                    return Err(ModeResolutionError::DuplicateOrderOverride {
                        view: view_id,
                        mode: mode.clone(),
                    });
                }
            }
            let mut reordered = Vec::with_capacity(entries.len());
            for mode in order {
                if let Some(index) = entries.iter().position(|entry| &entry.mode == mode) {
                    reordered.push(entries.remove(index));
                }
            }
            reordered.append(&mut entries);
            entries = reordered;
        }
        Ok(ModeAttachmentPlan {
            view: view_id,
            binding_revision: view.binding_revision(),
            entries,
        })
    }

    pub(super) fn set_content_override(
        &mut self,
        content: ContentId,
        mode: ModeName,
        value: ModeOverride,
    ) {
        self.content_overrides.insert((content, mode), value);
    }

    pub(super) fn set_view_override(&mut self, view: ViewId, mode: ModeName, value: ModeOverride) {
        self.view_overrides.insert((view, mode), value);
    }

    pub(super) fn set_view_order_override(&mut self, view: ViewId, order: Vec<ModeName>) {
        self.view_order_overrides.insert(view, order);
    }

    pub(super) fn forget_content(&mut self, content: ContentId) {
        self.content_overrides
            .retain(|(candidate, _), _| *candidate != content);
    }

    pub(super) fn forget_view(&mut self, view: ViewId) {
        self.view_overrides
            .retain(|(candidate, _), _| *candidate != view);
        self.view_order_overrides.remove(&view);
    }
}

fn resolve_order(registry: &ModeRegistry) -> Result<Vec<ModeName>, ModeResolutionError> {
    let definitions = registry.resolution_definitions().collect::<Vec<_>>();
    let indexes = definitions
        .iter()
        .enumerate()
        .map(|(index, definition)| (definition.name().clone(), index))
        .collect::<HashMap<_, _>>();
    let mut outgoing = vec![Vec::new(); definitions.len()];
    let mut indegree = vec![0usize; definitions.len()];
    for (source, definition) in definitions.iter().enumerate() {
        let Some(before) = definition.before() else {
            continue;
        };
        let Some(&target) = indexes.get(before) else {
            return Err(ModeResolutionError::UnknownBefore {
                mode: definition.name().clone(),
                before: before.clone(),
            });
        };
        outgoing[source].push(target);
        indegree[target] += 1;
    }
    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(definitions.len());
    while let Some(index) = ready.pop_first() {
        ordered.push(definitions[index].name().clone());
        for target in &outgoing[index] {
            indegree[*target] -= 1;
            if indegree[*target] == 0 {
                ready.insert(*target);
            }
        }
    }
    if ordered.len() != definitions.len() {
        let blocked = indegree
            .iter()
            .enumerate()
            .filter(|(_, degree)| **degree != 0)
            .map(|(index, _)| definitions[index].name().clone())
            .collect();
        return Err(ModeResolutionError::Cycle { blocked });
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::{LanguageId, Mode, ModeAdapters, ModeAttachmentRule};
    use vell_core::buffer::Buffer;
    use vell_core::content::Content;
    use vell_core::content_view_state::ContentViewState;
    use vell_protocol::view::{BindingKey, ViewDefinition, ViewDefinitionId};

    struct TestMode {
        name: ModeName,
        before: Option<ModeName>,
        rule: ModeAttachmentRule,
    }

    impl Mode for TestMode {
        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[crate::mode_name::ModeActionName] {
            &[]
        }

        fn adapters(&self) -> ModeAdapters {
            ModeAdapters::buffer()
        }

        fn before(&self) -> Option<&ModeName> {
            self.before.as_ref()
        }

        fn attachment(&self) -> ModeAttachmentRule {
            self.rule.clone()
        }
    }

    fn test_mode(name: &str, languages: &[&str]) -> TestMode {
        let rule = if languages.is_empty() {
            ModeAttachmentRule::buffer_document()
        } else {
            ModeAttachmentRule::buffer_document()
                .with_languages(languages.iter().map(|language| LanguageId::new(*language)))
        };
        TestMode {
            name: ModeName::new(name),
            before: None,
            rule,
        }
    }

    fn buffer_fixture() -> (ContentStore, ContentId, View) {
        let content = ContentId(3);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let view = View::buffer(content, ContentViewState::buffer());
        (contents, content, view)
    }

    #[test]
    fn resolver_matches_view_binding_and_classification() {
        let (contents, content, view) = buffer_fixture();
        let mut registry = ModeRegistry::new();
        registry.register(test_mode("rust", &["rust"])).unwrap();
        registry
            .register(test_mode("typescript", &["typescript"]))
            .unwrap();
        let mut classifier = ContentClassifier::default();
        classifier.set_language_override(content, Some(LanguageId::new("rust")));
        let resolver = ModeResolver::new(&registry).unwrap();

        let plan = resolver
            .resolve(ViewId(9), &view, &registry, &classifier, &contents)
            .unwrap();

        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.mode.as_str())
                .collect::<Vec<_>>(),
            ["rust"]
        );
    }

    #[test]
    fn per_view_override_can_diverge_two_views_of_one_content() {
        let (contents, content, view) = buffer_fixture();
        let mut registry = ModeRegistry::new();
        registry.register(test_mode("rust", &["rust"])).unwrap();
        let mut classifier = ContentClassifier::default();
        classifier.set_language_override(content, Some(LanguageId::new("rust")));
        let mut resolver = ModeResolver::new(&registry).unwrap();
        resolver.set_view_override(ViewId(2), ModeName::new("rust"), ModeOverride::Disable);

        let first = resolver
            .resolve(ViewId(1), &view, &registry, &classifier, &contents)
            .unwrap();
        let second = resolver
            .resolve(ViewId(2), &view, &registry, &classifier, &contents)
            .unwrap();

        assert_eq!(first.entries.len(), 1);
        assert!(second.entries.is_empty());
    }

    #[test]
    fn resolver_orders_forward_references_stably() {
        let mut registry = ModeRegistry::new();
        let mut overlay = test_mode("overlay", &[]);
        overlay.before = Some(ModeName::new("base"));
        registry.register(overlay).unwrap();
        registry.register(test_mode("base", &[])).unwrap();
        registry.register(test_mode("tail", &[])).unwrap();

        assert_eq!(
            resolve_order(&registry)
                .unwrap()
                .iter()
                .map(ModeName::as_str)
                .collect::<Vec<_>>(),
            ["overlay", "base", "tail"]
        );
    }

    #[test]
    fn view_order_override_can_replace_static_mode_precedence() {
        let (contents, _, view) = buffer_fixture();
        let mut registry = ModeRegistry::new();
        let mut overlay = test_mode("overlay", &[]);
        overlay.before = Some(ModeName::new("base"));
        registry.register(overlay).unwrap();
        registry.register(test_mode("base", &[])).unwrap();
        registry.register(test_mode("tail", &[])).unwrap();
        let mut resolver = ModeResolver::new(&registry).unwrap();
        resolver.set_view_order_override(
            ViewId(1),
            vec![ModeName::new("base"), ModeName::new("overlay")],
        );

        let plan = resolver
            .resolve(
                ViewId(1),
                &view,
                &registry,
                &ContentClassifier::default(),
                &contents,
            )
            .unwrap();

        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.mode.as_str())
                .collect::<Vec<_>>(),
            ["base", "overlay", "tail"]
        );
    }

    #[test]
    fn partial_view_order_override_keeps_unlisted_modes_stable() {
        let (contents, _, view) = buffer_fixture();
        let mut registry = ModeRegistry::new();
        registry.register(test_mode("first", &[])).unwrap();
        registry.register(test_mode("second", &[])).unwrap();
        registry.register(test_mode("third", &[])).unwrap();
        let mut resolver = ModeResolver::new(&registry).unwrap();
        resolver.set_view_order_override(ViewId(1), vec![ModeName::new("third")]);

        let plan = resolver
            .resolve(
                ViewId(1),
                &view,
                &registry,
                &ContentClassifier::default(),
                &contents,
            )
            .unwrap();

        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.mode.as_str())
                .collect::<Vec<_>>(),
            ["third", "first", "second"]
        );
    }

    #[test]
    fn view_order_override_rejects_unknown_and_duplicate_modes() {
        let (contents, _, view) = buffer_fixture();
        let mut registry = ModeRegistry::new();
        registry.register(test_mode("known", &[])).unwrap();
        let mut resolver = ModeResolver::new(&registry).unwrap();
        resolver.set_view_order_override(ViewId(1), vec![ModeName::new("missing")]);

        assert_eq!(
            resolver.resolve(
                ViewId(1),
                &view,
                &registry,
                &ContentClassifier::default(),
                &contents,
            ),
            Err(ModeResolutionError::UnknownOrderOverride {
                view: ViewId(1),
                mode: ModeName::new("missing"),
            })
        );

        resolver.set_view_order_override(
            ViewId(1),
            vec![ModeName::new("known"), ModeName::new("known")],
        );
        assert_eq!(
            resolver.resolve(
                ViewId(1),
                &view,
                &registry,
                &ContentClassifier::default(),
                &contents,
            ),
            Err(ModeResolutionError::DuplicateOrderOverride {
                view: ViewId(1),
                mode: ModeName::new("known"),
            })
        );
    }

    #[test]
    fn view_order_override_does_not_enable_a_filtered_mode() {
        let (contents, _, view) = buffer_fixture();
        let mut registry = ModeRegistry::new();
        registry.register(test_mode("rust", &["rust"])).unwrap();
        registry.register(test_mode("fallback", &[])).unwrap();
        let mut resolver = ModeResolver::new(&registry).unwrap();
        resolver.set_view_order_override(ViewId(1), vec![ModeName::new("rust")]);

        let plan = resolver
            .resolve(
                ViewId(1),
                &view,
                &registry,
                &ContentClassifier::default(),
                &contents,
            )
            .unwrap();

        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.mode.as_str())
                .collect::<Vec<_>>(),
            ["fallback"]
        );
    }

    #[test]
    fn rule_for_another_view_definition_does_not_attach() {
        let (contents, _, view) = buffer_fixture();
        let mut registry = ModeRegistry::new();
        let mode = TestMode {
            name: ModeName::new("diff"),
            before: None,
            rule: ModeAttachmentRule::new(
                ViewDefinitionId::new("test.diff"),
                BindingKey::new("left"),
            ),
        };
        registry.register(mode).unwrap();
        let classifier = ContentClassifier::default();
        let resolver = ModeResolver::new(&registry).unwrap();

        let plan = resolver
            .resolve(ViewId(1), &view, &registry, &classifier, &contents)
            .unwrap();

        assert!(plan.entries.is_empty());
    }

    #[test]
    fn view_only_rule_is_represented_without_inventing_a_content_binding() {
        let definition_id = ViewDefinitionId::new("test.diff");
        let definition = ViewDefinition::new(definition_id.clone(), []).unwrap();
        let view = View::with_definition(&definition, [], None).unwrap();
        let contents = ContentStore::default();
        let mut registry = ModeRegistry::new();
        registry
            .register(TestMode {
                name: ModeName::new("diff-navigation"),
                before: None,
                rule: ModeAttachmentRule::for_view(definition_id),
            })
            .unwrap();
        let resolver = ModeResolver::new(&registry).unwrap();

        let plan = resolver
            .resolve(
                ViewId(4),
                &view,
                &registry,
                &ContentClassifier::default(),
                &contents,
            )
            .unwrap();

        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].binding, None);
        assert_eq!(plan.entries[0].content, None);
    }
}
