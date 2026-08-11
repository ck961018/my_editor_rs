use std::fmt;

use vell_protocol::content_query::FaceName;
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::view::{BindingKey, ViewDefinitionId};

pub const MAX_VIEW_EXTENSION_PANES: usize = 8;
pub const MAX_VIEW_EXTENSION_PANE_SIZE: u16 = 256;
pub const MAX_VIEW_EXTENSION_ROWS: usize = 65_536;
pub const MAX_VIEW_EXTENSION_SEGMENTS: usize = 65_536;
pub const MAX_VIEW_EXTENSION_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_VIEW_EXTENSION_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewExtensionId(String);

impl ViewExtensionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ViewExtensionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewExtensionOwner(String);

impl ViewExtensionOwner {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ViewExtensionOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewExtensionPaneSide {
    Left,
    Right,
    Above,
    Below,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewExtensionPaneDefinition {
    key: String,
    side: ViewExtensionPaneSide,
    size: u16,
}

impl ViewExtensionPaneDefinition {
    pub fn new(key: impl Into<String>, side: ViewExtensionPaneSide, size: u16) -> Self {
        Self {
            key: key.into(),
            side,
            size,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn side(&self) -> ViewExtensionPaneSide {
        self.side
    }

    pub fn size(&self) -> u16 {
        self.size
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewExtensionDefinition {
    id: ViewExtensionId,
    owner: ViewExtensionOwner,
    target: ViewDefinitionId,
    panes: Vec<ViewExtensionPaneDefinition>,
}

impl ViewExtensionDefinition {
    pub fn new(
        id: ViewExtensionId,
        owner: ViewExtensionOwner,
        target: ViewDefinitionId,
        panes: Vec<ViewExtensionPaneDefinition>,
    ) -> Result<Self, ViewExtensionContractError> {
        validate_identifier("view extension id", id.as_str(), 128)?;
        validate_identifier("view extension owner", owner.as_str(), 256)?;
        if target.as_str().is_empty() {
            return Err(ViewExtensionContractError::new(
                "view extension target must not be empty",
            ));
        }
        if panes.is_empty() {
            return Err(ViewExtensionContractError::new(
                "view extension must declare at least one pane",
            ));
        }
        if panes.len() > MAX_VIEW_EXTENSION_PANES {
            return Err(ViewExtensionContractError::new(format!(
                "view extension declares {} panes; maximum is {MAX_VIEW_EXTENSION_PANES}",
                panes.len()
            )));
        }
        let mut keys = std::collections::BTreeSet::new();
        for pane in &panes {
            validate_identifier("view extension pane key", pane.key(), 64)?;
            if pane.size == 0 || pane.size > MAX_VIEW_EXTENSION_PANE_SIZE {
                return Err(ViewExtensionContractError::new(format!(
                    "view extension pane '{}' size must be between 1 and \
                     {MAX_VIEW_EXTENSION_PANE_SIZE}",
                    pane.key
                )));
            }
            if !keys.insert(pane.key.clone()) {
                return Err(ViewExtensionContractError::new(format!(
                    "view extension pane key '{}' is duplicated",
                    pane.key
                )));
            }
        }
        Ok(Self {
            id,
            owner,
            target,
            panes,
        })
    }

    pub fn id(&self) -> &ViewExtensionId {
        &self.id
    }

    pub fn owner(&self) -> &ViewExtensionOwner {
        &self.owner
    }

    pub fn target(&self) -> &ViewDefinitionId {
        &self.target
    }

    pub fn panes(&self) -> &[ViewExtensionPaneDefinition] {
        &self.panes
    }

    pub fn pane_key(&self, local: &str) -> String {
        format!("plugin.{}.{}", self.id.as_str(), local)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewExtensionPosition {
    pub line: usize,
    pub character: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewExtensionSelection {
    pub anchor: ViewExtensionPosition,
    pub head: ViewExtensionPosition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewExtensionDocument {
    pub content_id: ContentId,
    pub revision: Revision,
    pub text: String,
    pub resource_name: Option<String>,
    pub selections: Vec<ViewExtensionSelection>,
    pub primary_selection: ViewExtensionSelection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewExtensionContext {
    pub view_id: ViewId,
    pub definition: ViewDefinitionId,
    pub revision: Revision,
    pub bindings: Vec<(BindingKey, ContentId)>,
    pub document: Option<ViewExtensionDocument>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamedLineSegment {
    pub text: String,
    pub face: Option<FaceName>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamedLinesPresentation {
    pub base_face: Option<FaceName>,
    pub rows: Vec<Vec<NamedLineSegment>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewExtensionPresentation {
    Lines(NamedLinesPresentation),
}

impl ViewExtensionPresentation {
    pub fn validate(&self) -> Result<(), ViewExtensionContractError> {
        match self {
            Self::Lines(lines) => {
                if lines.rows.len() > MAX_VIEW_EXTENSION_ROWS {
                    return Err(ViewExtensionContractError::new(format!(
                        "view extension presentation has {} rows; maximum is \
                         {MAX_VIEW_EXTENSION_ROWS}",
                        lines.rows.len()
                    )));
                }
                let segment_count = lines.rows.iter().map(Vec::len).sum::<usize>();
                if segment_count > MAX_VIEW_EXTENSION_SEGMENTS {
                    return Err(ViewExtensionContractError::new(format!(
                        "view extension presentation has {segment_count} segments; maximum is \
                         {MAX_VIEW_EXTENSION_SEGMENTS}"
                    )));
                }
                let text_bytes = lines
                    .rows
                    .iter()
                    .flatten()
                    .map(|segment| segment.text.len())
                    .sum::<usize>();
                if text_bytes > MAX_VIEW_EXTENSION_TEXT_BYTES {
                    return Err(ViewExtensionContractError::new(format!(
                        "view extension presentation has {text_bytes} text bytes; maximum is \
                         {MAX_VIEW_EXTENSION_TEXT_BYTES}"
                    )));
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewExtensionContractError {
    message: String,
}

impl ViewExtensionContractError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ViewExtensionContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ViewExtensionContractError {}

pub trait ViewExtension {
    fn definition(&self) -> &ViewExtensionDefinition;

    fn present(
        &mut self,
        pane: &str,
        context: &ViewExtensionContext,
    ) -> Result<ViewExtensionPresentation, ViewExtensionContractError>;

    fn unload(&mut self) {}
}

fn validate_identifier(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ViewExtensionContractError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(ViewExtensionContractError::new(format!(
            "{label} must contain between 1 and {max_bytes} bytes"
        )));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
    {
        return Err(ViewExtensionContractError::new(format!(
            "{label} may contain only ASCII letters, digits, '.', '_' and '-'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_rejects_duplicate_panes_and_invalid_size() {
        let definition = |panes| {
            ViewExtensionDefinition::new(
                ViewExtensionId::new("example.minimap"),
                ViewExtensionOwner::new("example"),
                ViewDefinitionId::new("core.buffer"),
                panes,
            )
        };

        assert!(
            definition(vec![
                ViewExtensionPaneDefinition::new("map", ViewExtensionPaneSide::Right, 8,),
                ViewExtensionPaneDefinition::new("map", ViewExtensionPaneSide::Left, 4,),
            ])
            .is_err()
        );
        assert!(
            definition(vec![ViewExtensionPaneDefinition::new(
                "map",
                ViewExtensionPaneSide::Right,
                0,
            )])
            .is_err()
        );
    }

    #[test]
    fn presentation_limits_owned_output() {
        let presentation = ViewExtensionPresentation::Lines(NamedLinesPresentation {
            base_face: None,
            rows: vec![vec![NamedLineSegment {
                text: "x".repeat(MAX_VIEW_EXTENSION_TEXT_BYTES + 1),
                face: None,
            }]],
        });

        assert!(presentation.validate().is_err());
    }
}
