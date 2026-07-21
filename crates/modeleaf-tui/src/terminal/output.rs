use std::io::{self, Write};

use crate::protocol::content_query::{Color, CursorStyle, Face};
use crossterm::style::{
    Attribute, Color as TerminalColor, ResetColor, SetAttribute, SetBackgroundColor,
    SetForegroundColor,
};
use crossterm::{cursor, queue, terminal};

/// 绘图画布抽象：SceneRenderer 经 &mut dyn Canvas 输出。
/// 含 move_cursor/write_str + hide_cursor/show_cursor/flush，
/// 使 renderer 与 Output 固有方法解耦（后端可换）。
pub trait Canvas {
    fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()>;
    fn write_str(&mut self, s: &str) -> io::Result<()>;
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn set_cursor_style(&mut self, style: CursorStyle) -> io::Result<()>;
    fn set_reverse(&mut self, on: bool) -> io::Result<()>;
    fn set_face(&mut self, face: &Face) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

impl<W: Write> Canvas for Output<W> {
    fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()> {
        Output::move_cursor(self, row, col)
    }
    fn write_str(&mut self, s: &str) -> io::Result<()> {
        Output::write_str(self, s)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        Output::hide_cursor(self)
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        Output::show_cursor(self)
    }
    fn set_cursor_style(&mut self, style: CursorStyle) -> io::Result<()> {
        Output::set_cursor_style(self, style)
    }
    fn set_reverse(&mut self, on: bool) -> io::Result<()> {
        Output::set_reverse(self, on)
    }
    fn set_face(&mut self, face: &Face) -> io::Result<()> {
        Output::set_face(self, face)
    }
    fn flush(&mut self) -> io::Result<()> {
        Output::flush(self)
    }
}

pub struct Output<W: Write> {
    out: W,
}

impl<W: Write> Output<W> {
    pub fn new(out: W) -> Self {
        Self { out }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    pub fn hide_cursor(&mut self) -> io::Result<()> {
        queue!(self.out, cursor::Hide)
    }

    pub fn show_cursor(&mut self) -> io::Result<()> {
        queue!(self.out, cursor::Show)
    }

    pub fn set_cursor_style(&mut self, style: CursorStyle) -> io::Result<()> {
        let style = match style {
            CursorStyle::Default => cursor::SetCursorStyle::DefaultUserShape,
            CursorStyle::Block => cursor::SetCursorStyle::SteadyBlock,
            CursorStyle::Bar => cursor::SetCursorStyle::SteadyBar,
        };
        queue!(self.out, style)
    }

    pub fn set_reverse(&mut self, on: bool) -> io::Result<()> {
        let attr = if on {
            Attribute::Reverse
        } else {
            Attribute::NoReverse
        };
        queue!(self.out, SetAttribute(attr))
    }

    pub fn set_face(&mut self, face: &Face) -> io::Result<()> {
        queue!(self.out, ResetColor, SetAttribute(Attribute::Reset))?;
        if let Some(color) = face.foreground {
            queue!(self.out, SetForegroundColor(terminal_color(color)))?;
        }
        if let Some(color) = face.background {
            queue!(self.out, SetBackgroundColor(terminal_color(color)))?;
        }
        for (enabled, attribute) in [
            (face.bold, Attribute::Bold),
            (face.italic, Attribute::Italic),
            (face.underline, Attribute::Underlined),
        ] {
            if enabled == Some(true) {
                queue!(self.out, SetAttribute(attribute))?;
            }
        }
        Ok(())
    }

    /// 内部 0-based；crossterm MoveTo 也是 0-based，参数顺序为 (col, row)。
    pub fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()> {
        queue!(self.out, cursor::MoveTo(col as u16, row as u16))
    }

    /// 清空整个终端画布，供切换 screen buffer 等生命周期操作使用。
    pub fn clear_screen(&mut self) -> io::Result<()> {
        queue!(self.out, terminal::Clear(terminal::ClearType::All))
    }

    pub fn write_str(&mut self, s: &str) -> io::Result<()> {
        self.out.write_all(s.as_bytes())
    }

    /// 取回底层 writer，主要用于验证生成的终端输出。
    pub fn into_inner(self) -> W {
        self.out
    }
}

fn terminal_color(color: Color) -> TerminalColor {
    match color {
        Color::Ansi(value) => TerminalColor::AnsiValue(value),
        Color::Rgb { red, green, blue } => TerminalColor::Rgb {
            r: red,
            g: green,
            b: blue,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::content_query::CursorStyle;

    #[test]
    fn write_str_emits_bytes() {
        let mut out = Output::new(Vec::new());
        out.write_str("hello").unwrap();
        assert_eq!(out.into_inner(), b"hello");
    }

    #[test]
    fn move_cursor_emits_moveto_with_col_row_order() {
        let mut out = Output::new(Vec::new());
        // 内部 (row=2, col=5) -> crossterm MoveTo(column=5, row=2)
        // crossterm 0.28 emit 1-based 的 ESC[{row+1};{column+1}H = "3;6"
        // （若参数顺序写反成 MoveTo(column=2, row=5) 会得到 "6;3"，可区分）
        out.move_cursor(2, 5).unwrap();
        let bytes = out.into_inner();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("3;6"), "got: {s}");
    }

    #[test]
    fn clear_screen_queues_without_flush() {
        let mut out = Output::new(Vec::new());
        out.clear_screen().unwrap();
        // queue! 不 flush，但 Vec 立即接收字节，应非空
        assert!(!out.into_inner().is_empty());
    }

    #[test]
    fn canvas_dispatches_to_output() {
        let mut out = Output::new(Vec::new());
        let c: &mut dyn Canvas = &mut out;
        c.write_str("x").unwrap();
        c.move_cursor(2, 5).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains('x'));
        assert!(s.contains("3;6"), "got: {s}");
    }

    #[test]
    fn set_reverse_on_emits_sgr_7() {
        let mut out = Output::new(Vec::new());
        out.set_reverse(true).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[7m"), "got: {s}");
    }

    #[test]
    fn set_reverse_off_emits_sgr_27() {
        let mut out = Output::new(Vec::new());
        out.set_reverse(false).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[27m"), "got: {s}");
    }

    #[test]
    fn canvas_dispatches_set_reverse() {
        let mut out = Output::new(Vec::new());
        let c: &mut dyn Canvas = &mut out;
        c.set_reverse(true).unwrap();
        c.set_reverse(false).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[7m"), "on: {s}");
        assert!(s.contains("\x1b[27m"), "off: {s}");
    }

    #[test]
    fn set_cursor_style_block_emits_decsusrps() {
        let mut out = Output::new(Vec::new());
        out.set_cursor_style(CursorStyle::Block).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[2 q"), "got: {s}");
    }

    #[test]
    fn set_cursor_style_default_emits_decsusrps() {
        let mut out = Output::new(Vec::new());
        out.set_cursor_style(CursorStyle::Default).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[0 q"), "got: {s}");
    }

    #[test]
    fn set_cursor_style_bar_emits_decsusrps() {
        let mut out = Output::new(Vec::new());
        out.set_cursor_style(CursorStyle::Bar).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[6 q"), "got: {s}");
    }

    #[test]
    fn canvas_dispatches_set_cursor_style() {
        let mut out = Output::new(Vec::new());
        let c: &mut dyn Canvas = &mut out;
        c.set_cursor_style(CursorStyle::Block).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[2 q"), "got: {s}");
    }
}
