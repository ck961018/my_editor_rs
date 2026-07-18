use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use tree_sitter::StreamingIterator;
use tree_sitter::{InputEdit, Language, ParseOptions, Parser, Point, Query, QueryCursor};

use super::{
    Mode, ModeContentContext, ModeError, ModeJobRequest, ModeJobResult, ModeState, ModeViewContext,
};
use crate::app::mode_name::{ModeActionName, ModeName};
use crate::core::content::ContentChange;
use crate::core::text_snapshot::{TextBytePoint, TextSnapshot};
use crate::protocol::content_query::{Color, Face, FaceName, NamedTextDecoration, RowRange};
use crate::protocol::revision::Revision;
use crate::protocol::selection::TextOffset;

const PARSE_BUDGET: Duration = Duration::from_millis(500);

pub(super) struct TreeSitterMode {
    name: ModeName,
    language: Language,
    highlights: Query,
}

#[derive(Clone)]
struct TreeSitterState {
    source: TextSnapshot,
    revision: Revision,
    tree: Option<tree_sitter::Tree>,
    generation: u64,
    scheduled_generation: Option<u64>,
}

struct ParseOutput {
    source: TextSnapshot,
    revision: Revision,
    tree: tree_sitter::Tree,
}

impl TreeSitterMode {
    pub(super) fn rust() -> Self {
        let language: Language = tree_sitter_rust::LANGUAGE.into();
        let highlights = Query::new(&language, tree_sitter_rust::HIGHLIGHTS_QUERY)
            .expect("bundled Rust highlight query must compile");
        Self {
            name: ModeName::new("tree-sitter-rust"),
            language,
            highlights,
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
        self.highlights
            .capture_names()
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
        let revision = context.content_revision().unwrap_or_default();
        Ok(Box::new(TreeSitterState {
            source,
            revision,
            tree: None,
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
        let state = self.state_mut(state);
        if state.scheduled_generation == Some(state.generation) {
            return None;
        }
        state.scheduled_generation = Some(state.generation);
        let generation = state.generation;
        let source = state.source.clone();
        let revision = state.revision;
        let old_tree = state.tree.clone();
        let language = self.language.clone();
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
                Ok(Box::new(ParseOutput {
                    source,
                    revision,
                    tree,
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
        let Some(tree) = state.tree.as_ref() else {
            return Vec::new();
        };
        let start_byte = state.source.row_to_byte(visible_rows.start);
        let end_byte = state.source.row_to_byte(visible_rows.end);
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);
        let source = &state.source;
        let mut captures = cursor.captures(
            &self.highlights,
            tree.root_node(),
            |node: tree_sitter::Node<'_>| std::iter::once(source.byte_slice(node.byte_range())),
        );
        let mut decorations = Vec::new();
        while let Some((query_match, capture_index)) = captures.next() {
            let capture = query_match.captures[*capture_index];
            let range = capture.node.byte_range();
            if range.is_empty() || range.end <= start_byte || range.start >= end_byte {
                continue;
            }
            let capture_name = self.highlights.capture_names()[capture.index as usize];
            decorations.push(NamedTextDecoration {
                start: TextOffset {
                    char_index: source.byte_to_char(range.start),
                },
                end: TextOffset {
                    char_index: source.byte_to_char(range.end),
                },
                face: FaceName::new(format!("syntax.{capture_name}")),
            });
        }
        decorations
    }
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
}
