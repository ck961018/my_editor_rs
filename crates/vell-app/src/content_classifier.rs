use std::collections::HashMap;
use std::path::Path;

use crate::mode::LanguageId;
use vell_core::content::ContentKind;
use vell_core::content_store::ContentStore;
use vell_protocol::content_query::{ContentData, ContentQuery};
use vell_protocol::ids::ContentId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClassificationSource {
    Explicit,
    ResourceName,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ContentClassification {
    pub content: ContentId,
    pub kind: ContentKind,
    pub language: Option<LanguageId>,
    pub source: ClassificationSource,
}

#[derive(Default)]
pub(super) struct ContentClassifier {
    language_overrides: HashMap<ContentId, Option<LanguageId>>,
}

impl ContentClassifier {
    pub(super) fn classify(
        &self,
        content: ContentId,
        contents: &ContentStore,
    ) -> Option<ContentClassification> {
        let kind = contents.kind(content)?;
        if let Some(language) = self.language_overrides.get(&content) {
            return Some(ContentClassification {
                content,
                kind,
                language: language.clone(),
                source: ClassificationSource::Explicit,
            });
        }
        let language = match contents.query(content, ContentQuery::ResourceName) {
            ContentData::ResourceName(Some(name)) => language_for_resource_name(&name),
            _ => None,
        };
        Some(ContentClassification {
            content,
            kind,
            source: if language.is_some() {
                ClassificationSource::ResourceName
            } else {
                ClassificationSource::Unknown
            },
            language,
        })
    }

    #[allow(
        dead_code,
        reason = "explicit language selection is an app extension seam"
    )]
    pub(super) fn set_language_override(
        &mut self,
        content: ContentId,
        language: Option<LanguageId>,
    ) {
        self.language_overrides.insert(content, language);
    }

    pub(super) fn forget(&mut self, content: ContentId) {
        self.language_overrides.remove(&content);
    }
}

fn language_for_resource_name(name: &str) -> Option<LanguageId> {
    let extension = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    let language = match extension.as_str() {
        "rs" => "rust",
        "md" | "markdown" => "markdown",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        _ => return None,
    };
    Some(LanguageId::new(language))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use vell_core::content::Content;

    fn contents_with_path(path: &str) -> (ContentStore, ContentId) {
        let content = ContentId(7);
        let mut contents = ContentStore::default();
        contents
            .insert(
                content,
                Content::buffer_from_file(PathBuf::from(path), String::new()),
            )
            .unwrap();
        (contents, content)
    }

    #[test]
    fn classifier_uses_resource_extension_as_one_signal() {
        let (contents, content) = contents_with_path("notes.README.MD");
        let classifier = ContentClassifier::default();

        let classification = classifier.classify(content, &contents).unwrap();

        assert_eq!(classification.language, Some(LanguageId::new("markdown")));
        assert_eq!(classification.source, ClassificationSource::ResourceName);
    }

    #[test]
    fn explicit_language_override_wins_and_can_select_plain_text() {
        let (contents, content) = contents_with_path("main.rs");
        let mut classifier = ContentClassifier::default();
        classifier.set_language_override(content, Some(LanguageId::new("typescript")));
        assert_eq!(
            classifier.classify(content, &contents).unwrap().language,
            Some(LanguageId::new("typescript"))
        );

        classifier.set_language_override(content, None);
        let classification = classifier.classify(content, &contents).unwrap();
        assert_eq!(classification.language, None);
        assert_eq!(classification.source, ClassificationSource::Explicit);
    }
}
