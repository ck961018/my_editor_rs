use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::fmt;

pub const BUFFER_VIEW_DEFINITION: &str = "core.buffer";
pub const DIFF_VIEW_DEFINITION: &str = "core.diff";
pub const DOCUMENT_BINDING: &str = "document";
pub const LEFT_BINDING: &str = "left";
pub const RIGHT_BINDING: &str = "right";

/// View definition 内稳定的 Content 角色名。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingKey(String);

impl BindingKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for BindingKey {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for BindingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 定义 View 行为和 binding schema 的稳定身份。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewDefinitionId(String);

impl ViewDefinitionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ViewDefinitionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewDefinitionError {
    DuplicateBinding(BindingKey),
}

/// View definition 的稳定身份与 binding schema。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewDefinition {
    id: ViewDefinitionId,
    bindings: BTreeSet<BindingKey>,
}

impl ViewDefinition {
    pub fn new(
        id: ViewDefinitionId,
        bindings: impl IntoIterator<Item = BindingKey>,
    ) -> Result<Self, ViewDefinitionError> {
        let mut declared = BTreeSet::new();
        for binding in bindings {
            if !declared.insert(binding.clone()) {
                return Err(ViewDefinitionError::DuplicateBinding(binding));
            }
        }
        Ok(Self {
            id,
            bindings: declared,
        })
    }

    pub fn buffer() -> Self {
        Self {
            id: ViewDefinitionId::new(BUFFER_VIEW_DEFINITION),
            bindings: BTreeSet::from([BindingKey::new(DOCUMENT_BINDING)]),
        }
    }

    pub fn diff() -> Self {
        Self {
            id: ViewDefinitionId::new(DIFF_VIEW_DEFINITION),
            bindings: BTreeSet::from([
                BindingKey::new(LEFT_BINDING),
                BindingKey::new(RIGHT_BINDING),
            ]),
        }
    }

    pub fn id(&self) -> &ViewDefinitionId {
        &self.id
    }

    pub fn bindings(&self) -> impl Iterator<Item = &BindingKey> {
        self.bindings.iter()
    }

    pub fn declares(&self, binding: &str) -> bool {
        self.bindings.contains(binding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn binding_keys_support_borrowed_lookup() {
        let bindings = BTreeMap::from([(BindingKey::new(DOCUMENT_BINDING), 7)]);

        assert_eq!(bindings.get(DOCUMENT_BINDING), Some(&7));
    }

    #[test]
    fn view_definition_rejects_duplicate_binding_roles() {
        let result = ViewDefinition::new(
            ViewDefinitionId::new("test.diff"),
            [BindingKey::new("left"), BindingKey::new("left")],
        );

        assert_eq!(
            result,
            Err(ViewDefinitionError::DuplicateBinding(BindingKey::new(
                "left"
            )))
        );
    }

    #[test]
    fn built_in_diff_definition_declares_only_left_and_right() {
        let definition = ViewDefinition::diff();

        assert_eq!(definition.id().as_str(), DIFF_VIEW_DEFINITION);
        assert!(definition.declares(LEFT_BINDING));
        assert!(definition.declares(RIGHT_BINDING));
        assert!(!definition.declares(DOCUMENT_BINDING));
    }
}
