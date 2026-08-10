use std::collections::HashMap;

use vell_protocol::view::{BUFFER_VIEW_DEFINITION, ViewDefinition, ViewDefinitionId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ViewDefinitionRegistrationError {
    Conflict(ViewDefinitionId),
}

/// Kernel-owned catalog of immutable View definitions.
pub(super) struct ViewDefinitionRegistry {
    definitions: HashMap<ViewDefinitionId, ViewDefinition>,
}

impl ViewDefinitionRegistry {
    pub(super) fn new() -> Self {
        let mut registry = Self {
            definitions: HashMap::new(),
        };
        registry
            .register(ViewDefinition::buffer())
            .expect("built-in BufferView definition is unique");
        registry
    }

    pub(super) fn register(
        &mut self,
        definition: ViewDefinition,
    ) -> Result<(), ViewDefinitionRegistrationError> {
        match self.definitions.get(definition.id()) {
            Some(existing) if existing == &definition => Ok(()),
            Some(_) => Err(ViewDefinitionRegistrationError::Conflict(
                definition.id().clone(),
            )),
            None => {
                self.definitions.insert(definition.id().clone(), definition);
                Ok(())
            }
        }
    }

    pub(super) fn get(&self, id: &ViewDefinitionId) -> Option<&ViewDefinition> {
        self.definitions.get(id)
    }

    pub(super) fn buffer(&self) -> &ViewDefinition {
        self.get(&ViewDefinitionId::new(BUFFER_VIEW_DEFINITION))
            .expect("registry contains built-in BufferView definition")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vell_protocol::view::BindingKey;

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
}
