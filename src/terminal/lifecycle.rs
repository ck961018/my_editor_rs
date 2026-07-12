use std::io;

use crossterm::{
    cursor::SetCursorStyle,
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

/// RAII guard：进入时启用 raw mode + alternate screen，drop 时恢复。
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            SetCursorStyle::DefaultUserShape,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}
