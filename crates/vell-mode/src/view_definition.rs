use std::collections::BTreeSet;
use std::fmt;

use vell_protocol::view::{BindingKey, ViewDefinition, ViewDefinitionError, ViewDefinitionId};

pub const MAX_COMPOUND_VIEW_BINDINGS: usize = 16;
pub const MAX_COMPOUND_VIEW_CHILD_BINDINGS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewDefinitionOwner(String);

impl ViewDefinitionOwner {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ViewDefinitionOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompoundViewDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewChildBinding {
    child: BindingKey,
    parent: BindingKey,
}

impl ViewChildBinding {
    pub fn new(child: impl Into<String>, parent: impl Into<String>) -> Self {
        Self {
            child: BindingKey::new(child),
            parent: BindingKey::new(parent),
        }
    }

    pub fn child(&self) -> &BindingKey {
        &self.child
    }

    pub fn parent(&self) -> &BindingKey {
        &self.parent
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewChildDefinition {
    key: String,
    definition: ViewDefinitionId,
    bindings: Vec<ViewChildBinding>,
}

impl ViewChildDefinition {
    pub fn new(
        key: impl Into<String>,
        definition: ViewDefinitionId,
        bindings: Vec<ViewChildBinding>,
    ) -> Result<Self, CompoundViewDefinitionError> {
        let key = key.into();
        validate_identifier("compound View child key", &key, 64)?;
        validate_identifier("compound View child definition", definition.as_str(), 128)?;
        if bindings.is_empty() {
            return Err(CompoundViewDefinitionError::new(format!(
                "compound View child '{key}' must map at least one binding"
            )));
        }
        if bindings.len() > MAX_COMPOUND_VIEW_CHILD_BINDINGS {
            return Err(CompoundViewDefinitionError::new(format!(
                "compound View child '{key}' maps {} bindings; maximum is \
                 {MAX_COMPOUND_VIEW_CHILD_BINDINGS}",
                bindings.len()
            )));
        }
        let mut child_bindings = BTreeSet::new();
        for binding in &bindings {
            validate_identifier("compound View child binding", binding.child().as_str(), 64)?;
            validate_identifier(
                "compound View parent binding",
                binding.parent().as_str(),
                64,
            )?;
            if !child_bindings.insert(binding.child().clone()) {
                return Err(CompoundViewDefinitionError::new(format!(
                    "compound View child '{key}' maps binding '{}' more than once",
                    binding.child()
                )));
            }
        }
        Ok(Self {
            key,
            definition,
            bindings,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn definition(&self) -> &ViewDefinitionId {
        &self.definition
    }

    pub fn bindings(&self) -> &[ViewChildBinding] {
        &self.bindings
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompoundViewDefinition {
    definition: ViewDefinition,
    owner: ViewDefinitionOwner,
    direction: CompoundViewDirection,
    children: [ViewChildDefinition; 2],
}

impl CompoundViewDefinition {
    pub fn new(
        id: ViewDefinitionId,
        owner: ViewDefinitionOwner,
        bindings: impl IntoIterator<Item = BindingKey>,
        direction: CompoundViewDirection,
        children: [ViewChildDefinition; 2],
    ) -> Result<Self, CompoundViewDefinitionError> {
        validate_identifier("compound View definition", id.as_str(), 128)?;
        validate_identifier("compound View owner", owner.as_str(), usize::MAX)?;
        if children[0].key() == children[1].key() {
            return Err(CompoundViewDefinitionError::new(format!(
                "compound View child key '{}' is duplicated",
                children[0].key()
            )));
        }
        let bindings = bindings.into_iter().collect::<Vec<_>>();
        if bindings.is_empty() {
            return Err(CompoundViewDefinitionError::new(
                "compound View must declare at least one binding",
            ));
        }
        if bindings.len() > MAX_COMPOUND_VIEW_BINDINGS {
            return Err(CompoundViewDefinitionError::new(format!(
                "compound View declares {} bindings; maximum is \
                 {MAX_COMPOUND_VIEW_BINDINGS}",
                bindings.len()
            )));
        }
        for binding in &bindings {
            validate_identifier("compound View binding", binding.as_str(), 64)?;
        }
        let definition = ViewDefinition::new(id, bindings).map_err(|error| match error {
            ViewDefinitionError::DuplicateBinding(binding) => CompoundViewDefinitionError::new(
                format!("compound View binding '{binding}' is duplicated"),
            ),
        })?;
        let mut mapped_parent_bindings = BTreeSet::new();
        for child in &children {
            for binding in child.bindings() {
                if !definition.declares(binding.parent().as_str()) {
                    return Err(CompoundViewDefinitionError::new(format!(
                        "compound View child '{}' maps unknown parent binding '{}'",
                        child.key(),
                        binding.parent()
                    )));
                }
                if !mapped_parent_bindings.insert(binding.parent().clone()) {
                    return Err(CompoundViewDefinitionError::new(format!(
                        "compound View parent binding '{}' is mapped more than once",
                        binding.parent()
                    )));
                }
            }
        }
        if mapped_parent_bindings != definition.bindings().cloned().collect() {
            return Err(CompoundViewDefinitionError::new(
                "compound View must map every parent binding exactly once",
            ));
        }
        Ok(Self {
            definition,
            owner,
            direction,
            children,
        })
    }

    pub fn definition(&self) -> &ViewDefinition {
        &self.definition
    }

    pub fn owner(&self) -> &ViewDefinitionOwner {
        &self.owner
    }

    pub fn direction(&self) -> CompoundViewDirection {
        self.direction
    }

    pub fn children(&self) -> &[ViewChildDefinition; 2] {
        &self.children
    }

    pub fn child_binding_for_parent(&self, parent: &BindingKey) -> Option<(usize, &BindingKey)> {
        self.children.iter().enumerate().find_map(|(index, child)| {
            child
                .bindings()
                .iter()
                .find(|binding| binding.parent() == parent)
                .map(|binding| (index, binding.child()))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompoundViewDefinitionError {
    message: String,
}

impl CompoundViewDefinitionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CompoundViewDefinitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CompoundViewDefinitionError {}

fn validate_identifier(
    label: &str,
    value: &str,
    maximum: usize,
) -> Result<(), CompoundViewDefinitionError> {
    if value.is_empty() || value.len() > maximum {
        return Err(CompoundViewDefinitionError::new(format!(
            "{label} must contain between 1 and {maximum} bytes"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CompoundViewDefinitionError::new(format!(
            "{label} '{value}' contains an invalid character"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(key: &str, parent: &str) -> ViewChildDefinition {
        ViewChildDefinition::new(
            key,
            ViewDefinitionId::new("core.buffer"),
            vec![ViewChildBinding::new("document", parent)],
        )
        .unwrap()
    }

    #[test]
    fn compound_definition_keeps_binding_roles_separate_from_child_keys() {
        let definition = CompoundViewDefinition::new(
            ViewDefinitionId::new("example.diff"),
            ViewDefinitionOwner::new("file.01"),
            [BindingKey::new("left"), BindingKey::new("right")],
            CompoundViewDirection::Horizontal,
            [child("before", "left"), child("after", "right")],
        )
        .unwrap();

        assert_eq!(definition.definition().id().as_str(), "example.diff");
        assert_eq!(definition.children()[0].key(), "before");
        assert_eq!(
            definition.children()[0].bindings()[0].parent().as_str(),
            "left"
        );
        assert_eq!(
            definition
                .child_binding_for_parent(&BindingKey::new("right"))
                .map(|(index, binding)| (index, binding.as_str())),
            Some((1, "document"))
        );
    }

    #[test]
    fn compound_definition_rejects_unknown_parent_binding() {
        let error = CompoundViewDefinition::new(
            ViewDefinitionId::new("example.diff"),
            ViewDefinitionOwner::new("file.01"),
            [BindingKey::new("left"), BindingKey::new("right")],
            CompoundViewDirection::Horizontal,
            [child("before", "left"), child("after", "missing")],
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unknown parent binding 'missing'")
        );
    }

    #[test]
    fn compound_definition_requires_every_parent_binding_to_be_mapped() {
        let error = CompoundViewDefinition::new(
            ViewDefinitionId::new("example.diff"),
            ViewDefinitionOwner::new("filesystem.".to_owned() + &"01".repeat(300)),
            [
                BindingKey::new("left"),
                BindingKey::new("right"),
                BindingKey::new("base"),
            ],
            CompoundViewDirection::Horizontal,
            [child("before", "left"), child("after", "right")],
        )
        .unwrap_err();

        assert!(error.to_string().contains("map every parent binding"));
    }
}
