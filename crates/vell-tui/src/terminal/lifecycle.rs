use std::io;

use crossterm::{
    cursor::{self, SetCursorStyle},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

/// RAII guard：进入时启用 raw mode + alternate screen，drop 时恢复。
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            cursor::Show,
            SetCursorStyle::DefaultUserShape,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}
