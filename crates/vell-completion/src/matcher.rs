use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use crate::model::{CandidateId, CompletionItem};

pub(crate) struct MatchInput<'a> {
    pub source: &'a crate::model::CompletionSourceKey,
    pub batch: crate::model::SourceBatchVersion,
    pub ordinal: usize,
    pub item: &'a CompletionItem,
    pub source_order: usize,
}

#[derive(Clone)]
pub(crate) struct Matched {
    pub id: CandidateId,
    pub positions: Arc<[usize]>,
}

/// Request-scoped capability for choosing a bounded, visible preview item.
///
/// Sources cannot construct or configure this probe. Authoritative filtering
/// and ranking still happen when the engine installs a batch.
pub struct CompletionPreviewProbe {
    query: FoldedQuery,
}

impl CompletionPreviewProbe {
    pub(crate) fn new(query: &str) -> Self {
        Self {
            query: FoldedQuery::new(query),
        }
    }

    pub fn matches(&mut self, candidate: &str) -> bool {
        fuzzy_match(&mut self.query, candidate).is_some()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RankKey<'a> {
    score: i64,
    sort_text: &'a str,
    label: &'a str,
    source_order: usize,
    source: &'a crate::model::CompletionSourceKey,
    batch: crate::model::SourceBatchVersion,
    ordinal: usize,
}

impl Ord for RankKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .cmp(&other.score)
            .then_with(|| other.sort_text.cmp(self.sort_text))
            .then_with(|| other.label.cmp(self.label))
            .then_with(|| other.source_order.cmp(&self.source_order))
            .then_with(|| other.source.cmp(self.source))
            .then_with(|| other.batch.cmp(&self.batch))
            .then_with(|| other.ordinal.cmp(&self.ordinal))
    }
}

impl PartialOrd for RankKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) fn match_top_k<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = MatchInput<'a>>,
    limit: usize,
) -> Vec<Matched> {
    if limit == 0 {
        return Vec::new();
    }
    let mut folded_query = FoldedQuery::new(query);
    let mut heap = BinaryHeap::<Reverse<(RankKey, Vec<usize>)>>::with_capacity(limit + 1);
    for input in candidates {
        let filter_text = input
            .item
            .filter_text
            .as_deref()
            .unwrap_or(&input.item.label);
        let Some((score, positions)) = fuzzy_match(&mut folded_query, filter_text) else {
            continue;
        };
        let rank = RankKey {
            score: score + i64::from(input.item.source_bias),
            sort_text: input.item.sort_text.as_deref().unwrap_or(&input.item.label),
            label: &input.item.label,
            source_order: input.source_order,
            source: input.source,
            batch: input.batch,
            ordinal: input.ordinal,
        };
        heap.push(Reverse((rank, positions)));
        if heap.len() > limit {
            heap.pop();
        }
    }
    heap.into_sorted_vec()
        .into_iter()
        .map(|Reverse((rank, positions))| Matched {
            id: CandidateId {
                source: rank.source.clone(),
                batch: rank.batch,
                ordinal: rank.ordinal,
            },
            positions: positions.into(),
        })
        .collect::<Vec<_>>()
}

struct FoldedQuery {
    unicode: Vec<char>,
    unicode_upper: Vec<Option<char>>,
    ascii: Option<Vec<u8>>,
    unicode_fold_cache: HashMap<char, Box<[char]>>,
}

impl FoldedQuery {
    fn new(value: &str) -> Self {
        let unicode = value
            .chars()
            .flat_map(char::to_lowercase)
            .collect::<Vec<_>>();
        Self {
            unicode_upper: unicode
                .iter()
                .map(|character| {
                    let mut uppercase = character.to_uppercase();
                    let first = uppercase.next()?;
                    uppercase.next().is_none().then_some(first)
                })
                .collect(),
            unicode,
            ascii: value.is_ascii().then(|| {
                value
                    .bytes()
                    .map(|byte| byte.to_ascii_lowercase())
                    .collect()
            }),
            unicode_fold_cache: HashMap::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.unicode.is_empty()
    }
}

fn fuzzy_match(needle: &mut FoldedQuery, candidate: &str) -> Option<(i64, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    if let Some(ascii) = &needle.ascii
        && candidate.is_ascii()
    {
        return fuzzy_match_ascii(ascii, candidate.as_bytes());
    }
    fuzzy_match_unicode(
        &needle.unicode,
        &needle.unicode_upper,
        &mut needle.unicode_fold_cache,
        candidate,
    )
}

fn fuzzy_match_ascii(needle: &[u8], candidate: &[u8]) -> Option<(i64, Vec<usize>)> {
    let mut positions = Vec::with_capacity(needle.len());
    let mut next = 0_usize;
    for (index, byte) in candidate.iter().enumerate() {
        if byte.to_ascii_lowercase() == needle[next] {
            positions.push(index);
            next += 1;
            if next == needle.len() {
                return Some((match_score(&positions), positions));
            }
        }
    }
    None
}

fn fuzzy_match_unicode(
    needle: &[char],
    uppercase: &[Option<char>],
    fold_cache: &mut HashMap<char, Box<[char]>>,
    candidate: &str,
) -> Option<(i64, Vec<usize>)> {
    let mut positions = Vec::with_capacity(needle.len());
    let mut next = 0_usize;
    'candidate: for (index, character) in candidate.chars().enumerate() {
        if character == needle[next] || uppercase[next] == Some(character) {
            positions.push(index);
            next += 1;
            if next == needle.len() {
                break 'candidate;
            }
            continue;
        }
        if !character.is_uppercase() {
            continue;
        }
        let folded = fold_cache.entry(character).or_insert_with(|| {
            character
                .to_lowercase()
                .collect::<Vec<_>>()
                .into_boxed_slice()
        });
        for &folded in folded.iter() {
            if folded == needle[next] {
                positions.push(index);
                next += 1;
                if next == needle.len() {
                    break 'candidate;
                }
            }
        }
    }
    if next != needle.len() {
        return None;
    }
    positions.dedup();
    Some((match_score(&positions), positions))
}

fn match_score(positions: &[usize]) -> i64 {
    let prefix = positions.first() == Some(&0);
    let consecutive = positions
        .windows(2)
        .filter(|window| window[1] == window[0] + 1)
        .count();
    let span = positions
        .last()
        .zip(positions.first())
        .map_or(0, |(last, first)| last - first);
    (if prefix { 1_000 } else { 0 }) + consecutive as i64 * 40 - span as i64
}

#[cfg(test)]
mod tests {
    use super::{FoldedQuery, MatchInput, fuzzy_match, match_top_k};
    use crate::{CompletionItem, CompletionSourceKey, SourceBatchVersion};

    #[test]
    fn unicode_case_fold_keeps_original_character_positions() {
        let (_, positions) = fuzzy_match(&mut FoldedQuery::new("äb"), "ÄxxB").unwrap();
        assert_eq!(positions, vec![0, 3]);
    }

    #[test]
    fn query_must_be_a_subsequence() {
        assert!(fuzzy_match(&mut FoldedQuery::new("abc"), "acb").is_none());
    }

    #[test]
    fn top_k_is_stable_when_scores_tie() {
        let source = CompletionSourceKey::from("test");
        let items = ["ax", "ab", "aa"].map(|label| CompletionItem::new(label, label));
        let matched = match_top_k(
            "a",
            items.iter().enumerate().map(|(ordinal, item)| MatchInput {
                source: &source,
                batch: SourceBatchVersion(1),
                ordinal,
                item,
                source_order: 0,
            }),
            2,
        );
        assert_eq!(
            matched
                .iter()
                .map(|candidate| items[candidate.id.ordinal].label.as_ref())
                .collect::<Vec<_>>(),
            ["aa", "ab"]
        );
    }
}
