use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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

pub(super) fn terminal_grapheme(grapheme: &str) -> String {
    grapheme.chars().map(terminal_char).collect()
}

pub(super) fn grapheme_width(grapheme: &str, cell_col: usize, tab_width: usize) -> usize {
    if grapheme == "\t" {
        let tab_width = tab_width.max(1);
        tab_width - cell_col % tab_width
    } else {
        UnicodeWidthStr::width(terminal_grapheme(grapheme).as_str())
    }
}

pub(super) fn display_width_before_col(line: &str, logical_col: usize, tab_width: usize) -> usize {
    let mut logical = 0;
    let mut cells = 0;
    for grapheme in line_content(line).graphemes(true) {
        if logical >= logical_col {
            break;
        }
        cells += grapheme_width(grapheme, cells, tab_width);
        logical += grapheme.chars().count();
    }
    cells
}

pub(super) fn take_display_width(text: &str, width: usize) -> String {
    let mut used: usize = 0;
    let mut result = String::new();
    for grapheme in text.graphemes(true) {
        let grapheme = terminal_grapheme(grapheme);
        let next = used.saturating_add(UnicodeWidthStr::width(grapheme.as_str()));
        if next > width {
            break;
        }
        result.push_str(&grapheme);
        used = next;
    }
    result
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
    fn display_width_respects_graphemes_and_clip_boundary() {
        assert_eq!(display_width_before_col("你a", 1, 4), 2);
        assert_eq!(display_width_before_col("e\u{301}a", 2, 4), 1);
        assert_eq!(display_width_before_col("👩\u{200d}🔬a", 3, 4), 2);
        assert_eq!(display_width_before_col("🇨🇳👍🏽a", 4, 4), 4);
        assert_eq!(take_display_width("你a", 2), "你");
        assert_eq!(take_display_width("你a", 1), "");
        assert_eq!(take_display_width("e\u{301}a", 1), "e\u{301}");
    }

    #[test]
    fn hard_tabs_advance_to_the_next_configured_stop() {
        assert_eq!(display_width_before_col("\tb", 1, 4), 4);
        assert_eq!(display_width_before_col("a\tb", 2, 4), 4);
        assert_eq!(display_width_before_col("a\tb", 3, 4), 5);
        assert_eq!(display_width_before_col("a\tb", 2, 8), 8);
    }
}
