use std::fmt;

pub const MAX_STRATEGY_TEXT_CHARS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndentationDecision {
    pub indent: String,
    pub closing_indent: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineCommentStrategy {
    pub delimiter: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockCommentStrategy {
    pub open: String,
    pub close: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenClosePair {
    pub open: String,
    pub close: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditingStrategyError {
    field: &'static str,
    reason: &'static str,
}

impl fmt::Display for EditingStrategyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.reason)
    }
}

impl std::error::Error for EditingStrategyError {}

impl IndentationDecision {
    pub fn validated(self) -> Result<Self, EditingStrategyError> {
        validate_indentation("indent", &self.indent)?;
        if let Some(indent) = &self.closing_indent {
            validate_indentation("closingIndent", indent)?;
        }
        Ok(self)
    }
}

impl LineCommentStrategy {
    pub fn validated(self) -> Result<Self, EditingStrategyError> {
        validate_token("delimiter", &self.delimiter)?;
        Ok(self)
    }
}

impl BlockCommentStrategy {
    pub fn validated(self) -> Result<Self, EditingStrategyError> {
        validate_token("open", &self.open)?;
        validate_token("close", &self.close)?;
        Ok(self)
    }
}

impl OpenClosePair {
    pub fn validated(self) -> Result<Self, EditingStrategyError> {
        validate_token("open", &self.open)?;
        validate_token("close", &self.close)?;
        Ok(self)
    }
}

fn validate_indentation(field: &'static str, value: &str) -> Result<(), EditingStrategyError> {
    if value.chars().count() > MAX_STRATEGY_TEXT_CHARS {
        return Err(EditingStrategyError {
            field,
            reason: "is too long",
        });
    }
    if !value
        .chars()
        .all(|character| matches!(character, ' ' | '\t'))
    {
        return Err(EditingStrategyError {
            field,
            reason: "must contain only spaces and tabs",
        });
    }
    Ok(())
}

fn validate_token(field: &'static str, value: &str) -> Result<(), EditingStrategyError> {
    let len = value.chars().count();
    if len == 0 {
        return Err(EditingStrategyError {
            field,
            reason: "must not be empty",
        });
    }
    if len > MAX_STRATEGY_TEXT_CHARS {
        return Err(EditingStrategyError {
            field,
            reason: "is too long",
        });
    }
    if value.contains(['\r', '\n']) {
        return Err(EditingStrategyError {
            field,
            reason: "must not contain a line break",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_owned_editing_strategy_data() {
        assert!(
            IndentationDecision {
                indent: "\t  ".into(),
                closing_indent: Some("  ".into()),
            }
            .validated()
            .is_ok()
        );
        assert!(
            IndentationDecision {
                indent: "nope".into(),
                closing_indent: None,
            }
            .validated()
            .is_err()
        );
        assert!(
            OpenClosePair {
                open: "".into(),
                close: ")".into(),
            }
            .validated()
            .is_err()
        );
    }
}
