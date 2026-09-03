use crate::matcher::{MatchInput, match_top_k};
use crate::{CompletionItem, CompletionSourceKey, SourceBatchVersion};

#[derive(Clone, Copy, Debug)]
pub enum CorpusKind {
    Ascii,
    Unicode,
    LongIdentifiers,
}

pub fn corpus(size: usize, kind: CorpusKind) -> Vec<CompletionItem> {
    (0..size)
        .map(|index| {
            let label = match kind {
                CorpusKind::Ascii => format!("project_symbol_{index:06}"),
                CorpusKind::Unicode => format!("项目_Δοκιμή_символ_{index:06}"),
                CorpusKind::LongIdentifiers => {
                    format!("project_subsystem_completion_provider_generated_identifier_{index:06}")
                }
            };
            CompletionItem::new(label.clone(), label)
        })
        .collect()
}

#[doc(hidden)]
pub fn match_count(query: &str, items: &[CompletionItem], limit: usize) -> usize {
    let source = CompletionSourceKey::from("benchmark");
    match_top_k(
        query,
        items.iter().enumerate().map(|(ordinal, item)| MatchInput {
            source: &source,
            batch: SourceBatchVersion(1),
            ordinal,
            item,
            source_order: 0,
        }),
        limit,
    )
    .len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_corpora_have_required_scales_and_shapes() {
        for size in [1_000, 10_000, 100_000] {
            assert_eq!(corpus(size, CorpusKind::Ascii).len(), size);
            assert_eq!(corpus(size, CorpusKind::Unicode).len(), size);
            let long = corpus(size, CorpusKind::LongIdentifiers);
            assert_eq!(long.len(), size);
            assert!(long[0].label.len() > 48);
        }
    }
}
