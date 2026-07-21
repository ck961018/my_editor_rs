use crate::key_event::KeyEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResizeEvent {
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontendEvent {
    Key(KeyEvent),
    Resize(ResizeEvent),
    /// 前端请求退出，例如窗口式前端的关闭按钮。
    QuitRequest,
}
