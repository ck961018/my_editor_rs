/// Status-bar content owns its shared mode/configuration scope. Its target belongs to each
/// status-bar view, so one content can back both a global bar and per-pane bars.
#[derive(Clone, Default)]
pub struct StatusBar;

impl StatusBar {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_has_no_view_target() {
        let _ = StatusBar::new();
    }
}
