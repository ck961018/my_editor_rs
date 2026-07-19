use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tree_sitter::{InputEdit, Language, ParseOptions, Parser, Point};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use super::{
    Mode, ModeContentContext, ModeError, ModeJobRequest, ModeJobResult, ModeState, ModeViewContext,
};
use crate::app::mode_name::{ModeActionName, ModeName};
use crate::core::content::ContentChange;
use crate::core::text_snapshot::{TextBytePoint, TextSnapshot};
use crate::protocol::content_query::{
    Color, ContentData, ContentQuery, Face, FaceName, NamedTextDecoration, RowRange,
};
use crate::protocol::revision::Revision;
use crate::protocol::selection::TextOffset;

const PARSE_BUDGET: Duration = Duration::from_millis(500);

pub(super) struct TreeSitterMode {
    name: ModeName,
    languages: Arc<TreeSitterLanguages>,
}

#[derive(Clone)]
struct TreeSitterState {
    source: TextSnapshot,
    revision: Revision,
    language: Option<String>,
    tree: Option<tree_sitter::Tree>,
    highlights: Arc<HighlightSnapshot>,
    generation: u64,
    scheduled_generation: Option<u64>,
}

/// Non-overlapping byte spans backed by flat outer-to-inner face stacks.
#[derive(Default)]
struct HighlightSnapshot {
    spans: Vec<HighlightSpan>,
    faces: Vec<usize>,
}

#[derive(Clone, Copy)]
struct HighlightSpan {
    start_byte: usize,
    end_byte: usize,
    face_start: usize,
    face_end: usize,
}

struct ParseOutput {
    source: TextSnapshot,
    revision: Revision,
    tree: tree_sitter::Tree,
    highlights: Arc<HighlightSnapshot>,
}

struct TreeSitterLanguages {
    configurations: HashMap<String, HighlightConfiguration>,
    aliases: HashMap<String, String>,
    highlight_names: Vec<String>,
}

impl TreeSitterLanguages {
    fn builtin() -> Self {
        let mut configurations = HashMap::new();
        configurations.insert(
            "rust".to_string(),
            HighlightConfiguration::new(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                tree_sitter_rust::INJECTIONS_QUERY,
                "",
            )
            .expect("bundled Rust tree-sitter queries must compile"),
        );
        configurations.insert(
            "markdown".to_string(),
            HighlightConfiguration::new(
                tree_sitter_md::LANGUAGE.into(),
                "markdown",
                tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
                tree_sitter_md::INJECTION_QUERY_BLOCK,
                "",
            )
            .expect("bundled Markdown tree-sitter queries must compile"),
        );
        configurations.insert(
            "markdown_inline".to_string(),
            HighlightConfiguration::new(
                tree_sitter_md::INLINE_LANGUAGE.into(),
                "markdown_inline",
                tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
                tree_sitter_md::INJECTION_QUERY_INLINE,
                "",
            )
            .expect("bundled inline Markdown tree-sitter queries must compile"),
        );

        let mut highlight_names: Vec<_> = configurations
            .values()
            .flat_map(|configuration| configuration.names())
            .filter(|name| !name.starts_with('_'))
            .filter(|name| **name != "none")
            .filter(|name| !name.starts_with("injection."))
            .filter(|name| !name.starts_with("local."))
            .map(|name| (*name).to_string())
            .collect();
        highlight_names.sort_unstable();
        highlight_names.dedup();
        for configuration in configurations.values_mut() {
            configuration.configure(&highlight_names);
        }

        Self {
            configurations,
            aliases: HashMap::from([
                ("rs".to_string(), "rust".to_string()),
                ("md".to_string(), "markdown".to_string()),
                ("mdown".to_string(), "markdown".to_string()),
            ]),
            highlight_names,
        }
    }

    fn canonical_name(&self, name: &str) -> Option<&str> {
        let name = name.trim();
        if let Some((registered, _)) = self.configurations.get_key_value(name) {
            return Some(registered);
        }
        self.aliases.get(name).map(String::as_str)
    }

    fn configuration(&self, name: &str) -> Option<&HighlightConfiguration> {
        self.configurations.get(self.canonical_name(name)?)
    }

    fn language_for_file_name(&self, file_name: &str) -> Option<String> {
        let extension = Path::new(file_name)
            .extension()?
            .to_str()?
            .to_ascii_lowercase();
        self.canonical_name(&extension).map(str::to_string)
    }
}

impl TreeSitterMode {
    pub(super) fn new() -> Self {
        Self {
            name: ModeName::new("tree-sitter"),
            languages: Arc::new(TreeSitterLanguages::builtin()),
        }
    }

    fn state<'a>(&self, state: &'a dyn ModeState) -> &'a TreeSitterState {
        state
            .as_any()
            .downcast_ref()
            .expect("tree-sitter mode owns its content state")
    }

    fn state_mut<'a>(&self, state: &'a mut dyn ModeState) -> &'a mut TreeSitterState {
        state
            .as_any_mut()
            .downcast_mut()
            .expect("tree-sitter mode owns its content state")
    }
}

impl Mode for TreeSitterMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn faces(&self) -> Vec<(FaceName, Face)> {
        self.languages
            .highlight_names
            .iter()
            .map(|capture| {
                (
                    FaceName::new(format!("syntax.{capture}")),
                    face_for_capture(capture),
                )
            })
            .collect()
    }

    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        let source = context
            .text_snapshot()
            .ok_or_else(|| callback_error(self, "tree-sitter mode requires text content"))?;
        let language = match context.query_content(ContentQuery::DocumentStatus) {
            ContentData::DocumentStatus(status) => status
                .file_name
                .as_deref()
                .and_then(|file_name| self.languages.language_for_file_name(file_name)),
            _ => None,
        };
        Ok(Box::new(TreeSitterState {
            source,
            revision: context.content_revision().unwrap_or_default(),
            language,
            tree: None,
            highlights: Arc::new(HighlightSnapshot::default()),
            generation: 0,
            scheduled_generation: None,
        }))
    }

    fn on_content_changed(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        change: &ContentChange,
    ) -> Result<(), ModeError> {
        let ContentChange::Text(change) = change;
        let state = self.state_mut(state);
        if let Some(tree) = state.tree.as_mut() {
            edit_tree(tree, &state.source, change)
                .map_err(|message| callback_error(self, message))?;
        }
        state.source = state
            .source
            .apply(change)
            .map_err(|error| callback_error(self, format!("invalid text delta: {error:?}")))?;
        state.highlights = Arc::new(HighlightSnapshot::default());
        state.revision = context.content_revision().unwrap_or(state.revision);
        state.generation = state
            .generation
            .checked_add(1)
            .expect("tree-sitter generation overflow");
        Ok(())
    }

    fn take_background_job(
        &self,
        state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
    ) -> Option<ModeJobRequest> {
        let languages = self.languages.clone();
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let state = self.state_mut(state);
        let language_name = state.language.clone()?;
        let language: Language = languages.configuration(&language_name)?.language.clone();
        if state.scheduled_generation == Some(state.generation) {
            return None;
        }
        state.scheduled_generation = Some(state.generation);
        let generation = state.generation;
        let source = state.source.clone();
        let revision = state.revision;
        let old_tree = state.tree.clone();
        Some(ModeJobRequest::new(
            "parse",
            generation,
            move |cancellation| {
                let text = source.to_owned_string();
                let mut parser = Parser::new();
                parser
                    .set_language(&language)
                    .map_err(|error| error.to_string())?;
                let started = Instant::now();
                let mut progress = |_: &tree_sitter::ParseState| {
                    if cancellation.is_cancelled() || started.elapsed() >= PARSE_BUDGET {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                };
                let options = ParseOptions::new().progress_callback(&mut progress);
                let bytes = text.as_bytes();
                let tree = parser
                    .parse_with_options(
                        &mut |offset, _| bytes.get(offset..).unwrap_or_default(),
                        old_tree.as_ref(),
                        Some(options),
                    )
                    .ok_or_else(|| "tree-sitter parsing was cancelled or timed out".to_string())?;
                if cancellation.is_cancelled() {
                    return Err("tree-sitter highlighting was cancelled".to_string());
                }
                // ponytail: the official highlighter reparses the root while building
                // injection layers; share a language tree only if profiling justifies it.
                let cancelled = Arc::new(AtomicUsize::new(0));
                let watchdog_flag = cancelled.clone();
                let watchdog_cancellation = cancellation.clone();
                let watchdog = runtime.spawn(async move {
                    tokio::select! {
                        _ = watchdog_cancellation.cancelled() => {}
                        _ = tokio::time::sleep(PARSE_BUDGET) => {}
                    }
                    watchdog_flag.store(1, Ordering::SeqCst);
                });
                let highlights =
                    highlight_source(&languages, &language_name, bytes, Some(&cancelled));
                watchdog.abort();
                let highlights = Arc::new(highlights?);
                Ok(Box::new(ParseOutput {
                    source,
                    revision,
                    tree,
                    highlights,
                }))
            },
        ))
    }

    fn apply_background_job(
        &self,
        state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        version: u64,
        result: ModeJobResult,
    ) -> Result<bool, ModeError> {
        let state = self.state_mut(state);
        let output = match result {
            Ok(output) => output
                .downcast::<ParseOutput>()
                .map_err(|_| callback_error(self, "invalid parse job output"))?,
            Err(_) => return Ok(false),
        };
        if version != state.generation || output.revision != state.revision {
            return Ok(false);
        }
        state.source = output.source;
        state.tree = Some(output.tree);
        state.highlights = output.highlights;
        Ok(true)
    }

    fn decorations(
        &self,
        content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        let state = self.state(content_state);
        let start_byte = state.source.row_to_byte(visible_rows.start);
        let end_byte = state.source.row_to_byte(visible_rows.end);
        let first = state
            .highlights
            .spans
            .partition_point(|highlight| highlight.end_byte <= start_byte);
        state
            .highlights
            .spans
            .get(first..)
            .unwrap_or_default()
            .iter()
            .take_while(|highlight| highlight.start_byte < end_byte)
            .flat_map(|highlight| {
                let start = TextOffset {
                    char_index: state.source.byte_to_char(highlight.start_byte),
                };
                let end = TextOffset {
                    char_index: state.source.byte_to_char(highlight.end_byte),
                };
                state.highlights.faces[highlight.face_start..highlight.face_end]
                    .iter()
                    .filter_map(move |face| {
                        let capture = self.languages.highlight_names.get(*face)?;
                        Some(NamedTextDecoration {
                            start,
                            end,
                            face: FaceName::new(format!("syntax.{capture}")),
                        })
                    })
            })
            .collect()
    }
}

fn highlight_source(
    languages: &TreeSitterLanguages,
    language_name: &str,
    source: &[u8],
    cancellation: Option<&AtomicUsize>,
) -> Result<HighlightSnapshot, String> {
    let configuration = languages
        .configuration(language_name)
        .ok_or_else(|| format!("unknown tree-sitter language: {language_name}"))?;
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(configuration, source, cancellation, |name| {
            languages.configuration(name)
        })
        .map_err(|error| error.to_string())?;
    let mut active: Vec<usize> = Vec::new();
    let mut highlights = HighlightSnapshot::default();
    for event in events {
        match event.map_err(|error| error.to_string())? {
            HighlightEvent::Source { start, end } => {
                if active.is_empty() || start == end {
                    continue;
                }
                let extends_previous = highlights.spans.last().is_some_and(|previous| {
                    previous.end_byte == start
                        && highlights.faces[previous.face_start..previous.face_end]
                            .iter()
                            .copied()
                            .eq(active.iter().copied())
                });
                if extends_previous {
                    highlights.spans.last_mut().unwrap().end_byte = end;
                } else {
                    let face_start = highlights.faces.len();
                    highlights.faces.extend(active.iter().copied());
                    highlights.spans.push(HighlightSpan {
                        start_byte: start,
                        end_byte: end,
                        face_start,
                        face_end: highlights.faces.len(),
                    });
                }
            }
            HighlightEvent::HighlightStart(highlight) => active.push(highlight.0),
            HighlightEvent::HighlightEnd => {
                active
                    .pop()
                    .ok_or_else(|| "unbalanced tree-sitter highlight events".to_string())?;
            }
        }
    }
    Ok(highlights)
}

fn edit_tree(
    tree: &mut tree_sitter::Tree,
    source: &TextSnapshot,
    change: &crate::core::transaction::TextChangeSet,
) -> Result<(), String> {
    let edits = change.to_edits().map_err(|error| format!("{error:?}"))?;
    for edit in edits.into_iter().rev() {
        let start_byte = source.char_to_byte(edit.range.start);
        let old_end_byte = source.char_to_byte(edit.range.end);
        let start_position = point(source.byte_point_at_char(edit.range.start));
        let old_end_position = point(source.byte_point_at_char(edit.range.end));
        tree.edit(&InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte: start_byte + edit.insert.len(),
            start_position,
            old_end_position,
            new_end_position: advance_point(start_position, &edit.insert),
        });
    }
    Ok(())
}

fn point(point: TextBytePoint) -> Point {
    Point::new(point.row, point.byte_col)
}

fn advance_point(start: Point, inserted: &str) -> Point {
    match inserted.rsplit_once('\n') {
        Some((before_last_line, last_line)) => Point::new(
            start.row
                + before_last_line
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                + 1,
            last_line.len(),
        ),
        None => Point::new(start.row, start.column + inserted.len()),
    }
}

fn callback_error(mode: &TreeSitterMode, message: impl Into<String>) -> ModeError {
    ModeError::CallbackFailed {
        mode: mode.name.clone(),
        message: message.into(),
    }
}

fn face_for_capture(capture: &str) -> Face {
    let (color, italic, bold) = if capture.starts_with("comment") {
        (244, true, false)
    } else if capture.starts_with("string") || capture.starts_with("character") {
        (114, false, false)
    } else if capture.starts_with("keyword") {
        (204, false, true)
    } else if capture.starts_with("function") || capture.starts_with("method") {
        (81, false, false)
    } else if capture.starts_with("type") || capture.starts_with("constructor") {
        (117, false, false)
    } else if capture.starts_with("constant")
        || capture.starts_with("number")
        || capture.starts_with("boolean")
    {
        (221, false, false)
    } else if capture.starts_with("attribute") || capture.starts_with("macro") {
        (215, false, false)
    } else if capture.starts_with("operator") || capture.starts_with("punctuation") {
        (245, false, false)
    } else {
        (252, false, false)
    };
    Face {
        foreground: Some(Color::Ansi(color)),
        italic: Some(italic),
        bold: Some(bold),
        ..Face::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_use_utf8_byte_columns() {
        let snapshot = TextSnapshot::new(&ropey::Rope::from_str("中x\n"));
        assert_eq!(
            snapshot.byte_point_at_char(1),
            TextBytePoint {
                row: 0,
                byte_col: 3,
            }
        );
    }

    #[test]
    fn inserted_text_advances_tree_sitter_points() {
        assert_eq!(advance_point(Point::new(2, 4), "a\nβ"), Point::new(3, 2));
    }

    #[test]
    fn markdown_injects_rust_highlights() {
        let languages = TreeSitterLanguages::builtin();
        let source = b"```rust\nfn embedded() {}\n```\n";
        let highlights = highlight_source(&languages, "markdown", source, None).unwrap();
        let keyword = languages
            .highlight_names
            .iter()
            .position(|name| name == "keyword")
            .unwrap();
        assert!(highlights.spans.iter().any(|highlight| {
            highlight.start_byte == 8
                && highlight.end_byte == 10
                && highlights.faces[highlight.face_start..highlight.face_end].contains(&keyword)
        }));
    }

    #[test]
    fn rust_attribute_preserves_its_nested_punctuation_face() {
        let languages = TreeSitterLanguages::builtin();
        let source = b"#[derive(Debug)]\nfn main() {}\n";
        let highlights = highlight_source(&languages, "rust", source, None).unwrap();
        let attribute = languages
            .highlight_names
            .iter()
            .position(|name| name == "attribute")
            .unwrap();
        let punctuation = languages
            .highlight_names
            .iter()
            .position(|name| name == "punctuation.bracket")
            .unwrap();
        let nested_faces: Vec<_> = highlights
            .spans
            .iter()
            .filter(|highlight| highlight.start_byte <= 1 && highlight.end_byte >= 2)
            .flat_map(|highlight| {
                highlights.faces[highlight.face_start..highlight.face_end]
                    .iter()
                    .copied()
            })
            .collect();

        assert!(nested_faces.contains(&attribute));
        assert!(nested_faces.contains(&punctuation));
        assert!(
            nested_faces.iter().position(|face| *face == attribute)
                < nested_faces.iter().position(|face| *face == punctuation)
        );
    }
}
