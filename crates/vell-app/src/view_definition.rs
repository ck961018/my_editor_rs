use std::collections::HashMap;
use std::fmt;

use vell_mode::{
    CompoundViewDefinition, CompoundViewDirection, ViewChildBinding, ViewChildDefinition,
    ViewDefinitionOwner,
};
use vell_protocol::view::{
    BUFFER_VIEW_DEFINITION, BindingKey, DIFF_VIEW_DEFINITION, DOCUMENT_BINDING, LEFT_BINDING,
    RIGHT_BINDING, ViewDefinition, ViewDefinitionId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ViewDefinitionRegistrationError {
    Conflict(ViewDefinitionId),
    UnknownChild {
        definition: ViewDefinitionId,
        child: String,
        target: ViewDefinitionId,
    },
    InvalidChildBindings {
        definition: ViewDefinitionId,
        child: String,
    },
}

impl fmt::Display for ViewDefinitionRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict(definition) => {
                write!(
                    formatter,
                    "View definition '{definition}' conflicts with an existing definition"
                )
            }
            Self::UnknownChild {
                definition,
                child,
                target,
            } => write!(
                formatter,
                "compound View '{definition}' child '{child}' targets unknown View definition \
                 '{target}'"
            ),
            Self::InvalidChildBindings { definition, child } => write!(
                formatter,
                "compound View '{definition}' child '{child}' must map every binding of a \
                 document leaf View exactly once"
            ),
        }
    }
}

impl std::error::Error for ViewDefinitionRegistrationError {}

/// Kernel-owned catalog of immutable View definitions.
#[derive(Clone)]
pub(super) struct ViewDefinitionRegistry {
    definitions: HashMap<ViewDefinitionId, RegisteredViewDefinition>,
}

#[derive(Clone)]
struct RegisteredViewDefinition {
    definition: ViewDefinition,
    compound: Option<CompoundViewDefinition>,
}

impl ViewDefinitionRegistry {
    pub(super) fn new() -> Self {
        let mut registry = Self {
            definitions: HashMap::new(),
        };
        registry
            .register(ViewDefinition::buffer())
            .expect("built-in BufferView definition is unique");
        let child = |key: &str, parent: &str| {
            ViewChildDefinition::new(
                key,
                ViewDefinitionId::new(BUFFER_VIEW_DEFINITION),
                vec![ViewChildBinding::new(DOCUMENT_BINDING, parent)],
            )
            .expect("built-in DiffView child is valid")
        };
        registry
            .register_compound(
                CompoundViewDefinition::new(
                    ViewDefinitionId::new(DIFF_VIEW_DEFINITION),
                    ViewDefinitionOwner::new("core"),
                    [
                        BindingKey::new(LEFT_BINDING),
                        BindingKey::new(RIGHT_BINDING),
                    ],
                    CompoundViewDirection::Horizontal,
                    [child("left", LEFT_BINDING), child("right", RIGHT_BINDING)],
                )
                .expect("built-in DiffView definition is valid"),
            )
            .expect("built-in DiffView definition is unique");
        registry
    }

    pub(super) fn register(
        &mut self,
        definition: ViewDefinition,
    ) -> Result<(), ViewDefinitionRegistrationError> {
        match self.definitions.get(definition.id()) {
            Some(existing) if existing.definition == definition => Ok(()),
            Some(_) => Err(ViewDefinitionRegistrationError::Conflict(
                definition.id().clone(),
            )),
            None => {
                self.definitions.insert(
                    definition.id().clone(),
                    RegisteredViewDefinition {
                        definition,
                        compound: None,
                    },
                );
                Ok(())
            }
        }
    }

    pub(super) fn register_compounds(
        &mut self,
        definitions: impl IntoIterator<Item = CompoundViewDefinition>,
    ) -> Result<(), ViewDefinitionRegistrationError> {
        let mut candidate = self.clone();
        for definition in definitions {
            candidate.register_compound(definition)?;
        }
        *self = candidate;
        Ok(())
    }

    pub(super) fn register_compound(
        &mut self,
        compound: CompoundViewDefinition,
    ) -> Result<(), ViewDefinitionRegistrationError> {
        let definition = compound.definition();
        if let Some(existing) = self.definitions.get(definition.id()) {
            if existing.compound.as_ref() == Some(&compound) {
                return Ok(());
            }
            return Err(ViewDefinitionRegistrationError::Conflict(
                definition.id().clone(),
            ));
        }
        for child in compound.children() {
            let target = self.definitions.get(child.definition()).ok_or_else(|| {
                ViewDefinitionRegistrationError::UnknownChild {
                    definition: definition.id().clone(),
                    child: child.key().to_owned(),
                    target: child.definition().clone(),
                }
            })?;
            let mapped = child
                .bindings()
                .iter()
                .map(|binding| binding.child().clone())
                .collect::<std::collections::BTreeSet<_>>();
            let expected = target
                .definition
                .bindings()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            if target.compound.is_some()
                || !target.definition.declares(DOCUMENT_BINDING)
                || expected.len() != 1
                || mapped != expected
            {
                return Err(ViewDefinitionRegistrationError::InvalidChildBindings {
                    definition: definition.id().clone(),
                    child: child.key().to_owned(),
                });
            }
        }
        self.definitions.insert(
            definition.id().clone(),
            RegisteredViewDefinition {
                definition: definition.clone(),
                compound: Some(compound),
            },
        );
        Ok(())
    }

    pub(super) fn get(&self, id: &ViewDefinitionId) -> Option<&ViewDefinition> {
        self.definitions.get(id).map(|entry| &entry.definition)
    }

    pub(super) fn compound(&self, id: &ViewDefinitionId) -> Option<&CompoundViewDefinition> {
        self.definitions
            .get(id)
            .and_then(|entry| entry.compound.as_ref())
    }

    pub(super) fn ids_for_owner(
        &self,
        owner: &ViewDefinitionOwner,
    ) -> std::collections::HashSet<ViewDefinitionId> {
        self.definitions
            .values()
            .filter_map(|entry| {
                entry
                    .compound
                    .as_ref()
                    .filter(|definition| definition.owner() == owner)
                    .map(|definition| definition.definition().id().clone())
            })
            .collect()
    }

    pub(super) fn remove_owner(&mut self, owner: &ViewDefinitionOwner) -> usize {
        if owner.as_str() == "core" {
            return 0;
        }
        let previous = self.definitions.len();
        self.definitions.retain(|_, entry| {
            entry
                .compound
                .as_ref()
                .is_none_or(|definition| definition.owner() != owner)
        });
        previous - self.definitions.len()
    }

    pub(super) fn buffer(&self) -> &ViewDefinition {
        self.get(&ViewDefinitionId::new(BUFFER_VIEW_DEFINITION))
            .expect("registry contains built-in BufferView definition")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_accepts_idempotent_registration_and_rejects_schema_conflicts() {
        let id = ViewDefinitionId::new("test.diff");
        let original = ViewDefinition::new(
            id.clone(),
            [BindingKey::new("left"), BindingKey::new("right")],
        )
        .unwrap();
        let conflict = ViewDefinition::new(id.clone(), [BindingKey::new("document")]).unwrap();
        let mut registry = ViewDefinitionRegistry::new();

        registry.register(original.clone()).unwrap();
        registry.register(original).unwrap();

        assert_eq!(
            registry.register(conflict),
            Err(ViewDefinitionRegistrationError::Conflict(id))
        );
    }

    #[test]
    fn compound_registration_is_atomic_and_requires_complete_leaf_bindings() {
        let child = |key: &str, binding: &str| {
            ViewChildDefinition::new(
                key,
                ViewDefinitionId::new(BUFFER_VIEW_DEFINITION),
                vec![ViewChildBinding::new(binding, key)],
            )
            .unwrap()
        };
        let valid = CompoundViewDefinition::new(
            ViewDefinitionId::new("test.valid"),
            ViewDefinitionOwner::new("test"),
            [BindingKey::new("left"), BindingKey::new("right")],
            CompoundViewDirection::Horizontal,
            [
                child("left", DOCUMENT_BINDING),
                child("right", DOCUMENT_BINDING),
            ],
        )
        .unwrap();
        let invalid = CompoundViewDefinition::new(
            ViewDefinitionId::new("test.invalid"),
            ViewDefinitionOwner::new("test"),
            [BindingKey::new("left"), BindingKey::new("right")],
            CompoundViewDirection::Horizontal,
            [child("left", "missing"), child("right", DOCUMENT_BINDING)],
        )
        .unwrap();
        let mut registry = ViewDefinitionRegistry::new();

        assert!(
            registry
                .register_compounds([valid.clone(), invalid])
                .is_err()
        );
        assert!(registry.get(valid.definition().id()).is_none());
    }
}
