use unicode_width::UnicodeWidthChar;

pub(super) fn sanitize_terminal_text(text: &str) -> String {
    text.chars().map(terminal_char).collect()
}

pub(super) fn line_content(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line)
}

pub(super) fn terminal_char(ch: char) -> char {
    if ch.is_control() { '\u{fffd}' } else { ch }
}

pub(super) fn terminal_char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(1)
}

pub(super) fn display_width_before_col(line: &str, logical_col: usize) -> usize {
    line_content(line)
        .chars()
        .take(logical_col)
        .map(terminal_char)
        .map(terminal_char_width)
        .sum()
}

pub(super) fn take_display_width(text: &str, width: usize) -> String {
    let mut used: usize = 0;
    text.chars()
        .map(terminal_char)
        .take_while(|ch| {
            let next = used.saturating_add(terminal_char_width(*ch));
            if next > width {
                return false;
            }
            used = next;
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_and_control_characters_are_normalized() {
        assert_eq!(line_content("text\r\n"), "text");
        assert_eq!(sanitize_terminal_text("a\tb"), "a\u{fffd}b");
    }

    #[test]
    fn display_width_respects_wide_characters_and_clip_boundary() {
        assert_eq!(display_width_before_col("你a", 1), 2);
        assert_eq!(take_display_width("你a", 2), "你");
        assert_eq!(take_display_width("你a", 1), "");
    }
}
