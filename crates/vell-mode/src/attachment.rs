use std::collections::BTreeSet;
use std::fmt;

use vell_protocol::view::{BUFFER_VIEW_DEFINITION, BindingKey, DOCUMENT_BINDING, ViewDefinitionId};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageId(String);

impl LanguageId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Declarative rule used by the app's ModeResolver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeAttachmentRule {
    view: ViewDefinitionId,
    binding: Option<BindingKey>,
    languages: Option<BTreeSet<LanguageId>>,
}

impl ModeAttachmentRule {
    pub fn for_view(view: ViewDefinitionId) -> Self {
        Self {
            view,
            binding: None,
            languages: None,
        }
    }

    pub fn new(view: ViewDefinitionId, binding: BindingKey) -> Self {
        Self {
            view,
            binding: Some(binding),
            languages: None,
        }
    }

    pub fn buffer_document() -> Self {
        Self::new(
            ViewDefinitionId::new(BUFFER_VIEW_DEFINITION),
            BindingKey::new(DOCUMENT_BINDING),
        )
    }

    pub fn with_languages(mut self, languages: impl IntoIterator<Item = LanguageId>) -> Self {
        self.languages = Some(languages.into_iter().collect());
        self
    }

    pub fn view(&self) -> &ViewDefinitionId {
        &self.view
    }

    pub fn binding(&self) -> Option<&BindingKey> {
        self.binding.as_ref()
    }

    pub fn languages(&self) -> Option<impl Iterator<Item = &LanguageId>> {
        self.languages.as_ref().map(|values| values.iter())
    }

    pub fn matches_language(&self, language: Option<&LanguageId>) -> bool {
        self.languages
            .as_ref()
            .is_none_or(|languages| language.is_some_and(|value| languages.contains(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_languages_match_any_classification() {
        let rule = ModeAttachmentRule::buffer_document();

        assert_eq!(rule.binding().unwrap().as_str(), DOCUMENT_BINDING);
        assert!(rule.matches_language(None));
        assert!(rule.matches_language(Some(&LanguageId::new("rust"))));
    }

    #[test]
    fn declared_languages_match_only_members() {
        let rule = ModeAttachmentRule::buffer_document()
            .with_languages([LanguageId::new("rust"), LanguageId::new("markdown")]);

        assert!(rule.matches_language(Some(&LanguageId::new("rust"))));
        assert!(!rule.matches_language(Some(&LanguageId::new("typescript"))));
        assert!(!rule.matches_language(None));
    }

    #[test]
    fn view_only_rule_has_no_content_binding() {
        let rule = ModeAttachmentRule::for_view(ViewDefinitionId::new("test.diff"));

        assert!(rule.binding().is_none());
        assert!(rule.languages().is_none());
    }
}
