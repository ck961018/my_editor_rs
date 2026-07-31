use ropey::Rope;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

pub(crate) fn previous_boundary(rope: &Rope, char_index: usize) -> usize {
    let char_index = char_index.min(rope.len_chars());
    let byte_index = rope.char_to_byte(char_index);
    let (mut chunk, mut chunk_byte_index, mut chunk_char_index, _) = rope.chunk_at_byte(byte_index);
    let mut cursor = GraphemeCursor::new(byte_index, rope.len_bytes(), true);

    loop {
        match cursor.prev_boundary(chunk, chunk_byte_index) {
            Ok(None) => return 0,
            Ok(Some(boundary)) => {
                return chunk_char_index + chunk[..boundary - chunk_byte_index].chars().count();
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                let next = rope.chunk_at_byte(chunk_byte_index - 1);
                chunk = next.0;
                chunk_byte_index = next.1;
                chunk_char_index = next.2;
            }
            Err(GraphemeIncomplete::PreContext(needed)) => {
                let (context, context_start, _, _) = rope.chunk_at_byte(needed - 1);
                cursor.provide_context(context, context_start);
            }
            Err(GraphemeIncomplete::NextChunk | GraphemeIncomplete::InvalidOffset) => {
                unreachable!("rope chunks cover the grapheme cursor")
            }
        }
    }
}

pub(crate) fn next_boundary(rope: &Rope, char_index: usize) -> usize {
    let char_index = char_index.min(rope.len_chars());
    let byte_index = rope.char_to_byte(char_index);
    let (mut chunk, mut chunk_byte_index, mut chunk_char_index, _) = rope.chunk_at_byte(byte_index);
    let mut cursor = GraphemeCursor::new(byte_index, rope.len_bytes(), true);

    loop {
        match cursor.next_boundary(chunk, chunk_byte_index) {
            Ok(None) => return rope.len_chars(),
            Ok(Some(boundary)) => {
                return chunk_char_index + chunk[..boundary - chunk_byte_index].chars().count();
            }
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_byte_index += chunk.len();
                let next = rope.chunk_at_byte(chunk_byte_index);
                chunk = next.0;
                chunk_char_index = next.2;
            }
            Err(GraphemeIncomplete::PreContext(needed)) => {
                let (context, context_start, _, _) = rope.chunk_at_byte(needed - 1);
                cursor.provide_context(context, context_start);
            }
            Err(GraphemeIncomplete::PrevChunk | GraphemeIncomplete::InvalidOffset) => {
                unreachable!("rope chunks cover the grapheme cursor")
            }
        }
    }
}

pub(crate) fn is_boundary(rope: &Rope, char_index: usize) -> bool {
    let char_index = char_index.min(rope.len_chars());
    let byte_index = rope.char_to_byte(char_index);
    let (chunk, chunk_byte_index, _, _) = rope.chunk_at_byte(byte_index);
    let mut cursor = GraphemeCursor::new(byte_index, rope.len_bytes(), true);

    loop {
        match cursor.is_boundary(chunk, chunk_byte_index) {
            Ok(boundary) => return boundary,
            Err(GraphemeIncomplete::PreContext(needed)) => {
                let (context, context_start, _, _) = rope.chunk_at_byte(needed - 1);
                cursor.provide_context(context, context_start);
            }
            Err(
                GraphemeIncomplete::PrevChunk
                | GraphemeIncomplete::NextChunk
                | GraphemeIncomplete::InvalidOffset,
            ) => unreachable!("rope chunks cover the grapheme cursor"),
        }
    }
}

pub(crate) fn boundary_at_or_after(rope: &Rope, char_index: usize) -> usize {
    let char_index = char_index.min(rope.len_chars());
    if is_boundary(rope, char_index) {
        char_index
    } else {
        next_boundary(rope, char_index)
    }
}

pub(crate) fn boundary_at_or_before(rope: &Rope, char_index: usize) -> usize {
    let char_index = char_index.min(rope.len_chars());
    if is_boundary(rope, char_index) {
        char_index
    } else {
        previous_boundary(rope, char_index)
    }
}

pub(crate) fn column(rope: &Rope, line_start: usize, char_index: usize) -> usize {
    let target = boundary_at_or_after(rope, char_index).max(line_start);
    let mut current = line_start;
    let mut column = 0;
    while current < target {
        let next = next_boundary(rope, current);
        if next <= current || next > target {
            break;
        }
        current = next;
        column += 1;
    }
    column
}

pub(crate) fn at_column(rope: &Rope, line_start: usize, line_end: usize, column: usize) -> usize {
    let mut current = line_start;
    for _ in 0..column {
        let next = next_boundary(rope, current);
        if next <= current || next > line_end {
            return line_end;
        }
        current = next;
    }
    current.min(line_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_extended_grapheme_boundaries() {
        let rope = Rope::from_str("e\u{301}👩\u{200d}🔬🇨🇳👍🏽x");
        let expected = [0, 2, 5, 7, 9, 10];

        assert!(expected.iter().all(|index| is_boundary(&rope, *index)));
        assert!(
            (0..=rope.len_chars())
                .filter(|index| is_boundary(&rope, *index))
                .eq(expected)
        );
    }

    #[test]
    fn crosses_rope_chunks_without_splitting_a_grapheme() {
        let text = format!("a{}b", "\u{301}".repeat(2_000));
        let rope = Rope::from_str(&text);
        let boundary = 2_001;

        assert!(rope.chunks().count() > 1);
        assert_eq!(next_boundary(&rope, 0), boundary);
        assert_eq!(previous_boundary(&rope, boundary), 0);
        assert!(!is_boundary(&rope, 1_000));
    }

    #[test]
    fn grapheme_columns_map_to_char_offsets() {
        let rope = Rope::from_str("e\u{301}x\na\u{301}b");
        let second_line = rope.line_to_char(1);
        let second_end = second_line + 3;

        assert_eq!(column(&rope, 0, 2), 1);
        assert_eq!(
            at_column(&rope, second_line, second_end, 1),
            second_line + 2
        );
        assert_eq!(at_column(&rope, second_line, second_end, 9), second_end);
    }
}
