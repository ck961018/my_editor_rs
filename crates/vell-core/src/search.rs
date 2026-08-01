use std::fmt;
use std::ops::Range;

use regex::{Regex, RegexBuilder};
use vell_protocol::revision::Revision;

use crate::core::text_snapshot::TextSnapshot;
use crate::core::transaction::{TextChangeSet, TextEdit, TextTransactionError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchPattern {
    Literal(String),
    Regex(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseSensitivity {
    Sensitive,
    Insensitive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    pub case: CaseSensitivity,
    pub direction: SearchDirection,
    pub wrap: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchEdit {
    pub change: TextChangeSet,
    pub selection: SearchMatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    InvalidRegex(String),
    InvalidStart {
        start: usize,
        len: usize,
    },
    StaleSnapshot {
        expected: Revision,
        actual: Revision,
    },
    InvalidEdit(TextTransactionError),
}

#[derive(Clone)]
pub struct SearchSnapshot {
    revision: Revision,
    text: TextSnapshot,
}

struct CompiledPattern {
    regex: Regex,
    captures_in_replacement: bool,
}

struct FoundMatch {
    bytes: Range<usize>,
    chars: Range<usize>,
}

impl SearchSnapshot {
    pub fn new(revision: Revision, text: TextSnapshot) -> Self {
        Self { revision, text }
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn text(&self) -> &TextSnapshot {
        &self.text
    }

    pub fn ensure_current(&self, actual: Revision) -> Result<(), SearchError> {
        if self.revision == actual {
            Ok(())
        } else {
            Err(SearchError::StaleSnapshot {
                expected: self.revision,
                actual,
            })
        }
    }

    pub fn find_from(
        &self,
        pattern: &SearchPattern,
        options: SearchOptions,
        start: usize,
    ) -> Result<Option<SearchMatch>, SearchError> {
        let text = self.text.to_owned_string();
        let compiled = compile(pattern, options.case)?;
        find(
            &text,
            &compiled.regex,
            options.direction,
            options.wrap,
            start,
        )
        .map(|found| found.map(|found| SearchMatch { range: found.chars }))
    }

    pub fn replace_next(
        &self,
        pattern: &SearchPattern,
        replacement: &str,
        options: SearchOptions,
        start: usize,
    ) -> Result<Option<SearchEdit>, SearchError> {
        let text = self.text.to_owned_string();
        let compiled = compile(pattern, options.case)?;
        let Some(found) = find(
            &text,
            &compiled.regex,
            options.direction,
            options.wrap,
            start,
        )?
        else {
            return Ok(None);
        };
        let insert = replacement_for(&compiled, &text, &found.bytes, replacement);
        let inserted_len = insert.chars().count();
        let selection = SearchMatch {
            range: found.chars.start..found.chars.start + inserted_len,
        };
        let change = TextChangeSet::from_edits(
            self.text.len_chars(),
            vec![TextEdit::new(found.chars, insert)],
        )?;
        Ok(Some(SearchEdit { change, selection }))
    }

    pub fn replace_all(
        &self,
        pattern: &SearchPattern,
        replacement: &str,
        case: CaseSensitivity,
    ) -> Result<TextChangeSet, SearchError> {
        let text = self.text.to_owned_string();
        let compiled = compile(pattern, case)?;
        let edits = compiled
            .regex
            .captures_iter(&text)
            .map(|captures| {
                let matched = captures.get(0).expect("regex capture zero always exists");
                let range = byte_range_to_char(&text, matched.start()..matched.end());
                let insert = if compiled.captures_in_replacement {
                    let mut expanded = String::new();
                    captures.expand(replacement, &mut expanded);
                    expanded
                } else {
                    replacement.to_owned()
                };
                TextEdit::new(range, insert)
            })
            .collect();
        Ok(TextChangeSet::from_edits(self.text.len_chars(), edits)?)
    }
}

impl fmt::Display for SearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegex(message) => write!(formatter, "invalid regex: {message}"),
            Self::InvalidStart { start, len } => {
                write!(formatter, "search start {start} exceeds text length {len}")
            }
            Self::StaleSnapshot { expected, actual } => write!(
                formatter,
                "stale search snapshot: expected revision {}, actual {}",
                expected.0, actual.0
            ),
            Self::InvalidEdit(error) => write!(formatter, "invalid search edit: {error:?}"),
        }
    }
}

impl std::error::Error for SearchError {}

impl From<TextTransactionError> for SearchError {
    fn from(error: TextTransactionError) -> Self {
        Self::InvalidEdit(error)
    }
}

fn compile(pattern: &SearchPattern, case: CaseSensitivity) -> Result<CompiledPattern, SearchError> {
    let (source, captures_in_replacement) = match pattern {
        SearchPattern::Literal(source) => (regex::escape(source), false),
        SearchPattern::Regex(source) => (source.clone(), true),
    };
    let regex = RegexBuilder::new(&source)
        .case_insensitive(case == CaseSensitivity::Insensitive)
        .build()
        .map_err(|error| SearchError::InvalidRegex(error.to_string()))?;
    Ok(CompiledPattern {
        regex,
        captures_in_replacement,
    })
}

fn find(
    text: &str,
    regex: &Regex,
    direction: SearchDirection,
    wrap: bool,
    start: usize,
) -> Result<Option<FoundMatch>, SearchError> {
    let len = text.chars().count();
    if start > len {
        return Err(SearchError::InvalidStart { start, len });
    }
    let start_byte = char_to_byte(text, start);
    let matched = match direction {
        SearchDirection::Forward => regex.find_at(text, start_byte).or_else(|| {
            wrap.then(|| {
                regex
                    .find_iter(text)
                    .find(|found| found.start() < start_byte)
            })?
        }),
        SearchDirection::Backward => regex
            .find_iter(text)
            .take_while(|found| found.end() <= start_byte)
            .last()
            .or_else(|| wrap.then(|| regex.find_iter(text).last())?),
    };
    Ok(matched.map(|matched| FoundMatch {
        bytes: matched.start()..matched.end(),
        chars: byte_range_to_char(text, matched.start()..matched.end()),
    }))
}

fn replacement_for(
    compiled: &CompiledPattern,
    text: &str,
    matched: &Range<usize>,
    replacement: &str,
) -> String {
    if !compiled.captures_in_replacement {
        return replacement.to_owned();
    }
    let captures = compiled
        .regex
        .captures_at(text, matched.start)
        .expect("a previously found regex match still has captures");
    debug_assert_eq!(
        captures.get(0).map(|capture| capture.range()),
        Some(matched.clone())
    );
    let mut expanded = String::new();
    captures.expand(replacement, &mut expanded);
    expanded
}

fn char_to_byte(text: &str, offset: usize) -> usize {
    text.char_indices()
        .nth(offset)
        .map_or(text.len(), |(byte, _)| byte)
}

fn byte_range_to_char(text: &str, range: Range<usize>) -> Range<usize> {
    text[..range.start].chars().count()..text[..range.end].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(text: &str) -> SearchSnapshot {
        SearchSnapshot::new(Revision(7), TextSnapshot::from_text(text))
    }

    fn options(case: CaseSensitivity, direction: SearchDirection, wrap: bool) -> SearchOptions {
        SearchOptions {
            case,
            direction,
            wrap,
        }
    }

    #[test]
    fn literal_search_supports_direction_case_and_wrap() {
        let snapshot = snapshot("Alpha beta alpha");
        let sensitive = SearchPattern::Literal("alpha".into());
        let insensitive = SearchPattern::Literal("ALPHA".into());

        assert_eq!(
            snapshot
                .find_from(
                    &sensitive,
                    options(CaseSensitivity::Sensitive, SearchDirection::Forward, false),
                    1,
                )
                .unwrap(),
            Some(SearchMatch { range: 11..16 })
        );
        assert_eq!(
            snapshot
                .find_from(
                    &insensitive,
                    options(
                        CaseSensitivity::Insensitive,
                        SearchDirection::Backward,
                        false,
                    ),
                    10,
                )
                .unwrap(),
            Some(SearchMatch { range: 0..5 })
        );
        assert_eq!(
            snapshot
                .find_from(
                    &sensitive,
                    options(CaseSensitivity::Sensitive, SearchDirection::Backward, false,),
                    0,
                )
                .unwrap(),
            None
        );
        assert_eq!(
            snapshot
                .find_from(
                    &sensitive,
                    options(CaseSensitivity::Sensitive, SearchDirection::Backward, true,),
                    0,
                )
                .unwrap(),
            Some(SearchMatch { range: 11..16 })
        );
        assert_eq!(
            snapshot
                .find_from(
                    &sensitive,
                    options(CaseSensitivity::Sensitive, SearchDirection::Forward, false),
                    16,
                )
                .unwrap(),
            None
        );
        assert_eq!(
            snapshot
                .find_from(
                    &insensitive,
                    options(CaseSensitivity::Insensitive, SearchDirection::Forward, true),
                    16,
                )
                .unwrap(),
            Some(SearchMatch { range: 0..5 })
        );
    }

    #[test]
    fn invalid_regex_is_structured() {
        let error = snapshot("text")
            .find_from(
                &SearchPattern::Regex("(".into()),
                options(CaseSensitivity::Sensitive, SearchDirection::Forward, false),
                0,
            )
            .unwrap_err();

        assert!(matches!(error, SearchError::InvalidRegex(_)));
    }

    #[test]
    fn regex_replacements_expand_captures_in_one_change_set() {
        let snapshot = snapshot("one,two three,four");
        let pattern = SearchPattern::Regex(r"(\w+),(\w+)".into());
        let options = options(CaseSensitivity::Sensitive, SearchDirection::Forward, false);

        let next = snapshot
            .replace_next(&pattern, "$2:$1", options, 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot
                .text()
                .apply(&next.change)
                .unwrap()
                .to_owned_string(),
            "two:one three,four"
        );
        assert_eq!(next.selection.range, 0..7);

        let all = snapshot
            .replace_all(&pattern, "$2:$1", CaseSensitivity::Sensitive)
            .unwrap();
        assert_eq!(all.to_edits().unwrap().len(), 2);
        assert_eq!(
            snapshot.text().apply(&all).unwrap().to_owned_string(),
            "two:one four:three"
        );
    }

    #[test]
    fn literal_replacement_does_not_expand_capture_syntax() {
        let snapshot = snapshot("needle");
        let change = snapshot
            .replace_all(
                &SearchPattern::Literal("needle".into()),
                "$1",
                CaseSensitivity::Sensitive,
            )
            .unwrap();

        assert_eq!(
            snapshot.text().apply(&change).unwrap().to_owned_string(),
            "$1"
        );
    }

    #[test]
    fn zero_width_regex_replacement_progresses() {
        let snapshot = snapshot("ab cd");
        let change = snapshot
            .replace_all(
                &SearchPattern::Regex(r"\b".into()),
                "_",
                CaseSensitivity::Sensitive,
            )
            .unwrap();

        assert_eq!(
            snapshot.text().apply(&change).unwrap().to_owned_string(),
            "_ab_ _cd_"
        );
    }

    #[test]
    fn unicode_and_crlf_ranges_are_character_offsets() {
        let snapshot = snapshot("aé\r\ne\u{301}x");
        let options = options(CaseSensitivity::Sensitive, SearchDirection::Forward, false);

        assert_eq!(
            snapshot
                .find_from(&SearchPattern::Regex(r"\r?\n".into()), options, 0)
                .unwrap(),
            Some(SearchMatch { range: 2..4 })
        );
        assert_eq!(
            snapshot
                .find_from(&SearchPattern::Literal("\u{301}".into()), options, 0)
                .unwrap(),
            Some(SearchMatch { range: 5..6 })
        );
    }

    #[test]
    fn snapshot_revision_rejects_stale_use() {
        assert_eq!(
            snapshot("text").ensure_current(Revision(8)),
            Err(SearchError::StaleSnapshot {
                expected: Revision(7),
                actual: Revision(8),
            })
        );
    }
}
