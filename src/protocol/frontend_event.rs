use crate::protocol::key_event::KeyEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResizeEvent {
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontendEvent {
    Key(KeyEvent),
    Resize(ResizeEvent),
    /// 前端请求退出（如 GUI 窗口关闭按钮）。v0.1 退出走 Ctrl+Q（Key 路径），
    /// 此变体从未构造；保留为前端事件模型的语义完整部分，供未来 GUI 前端使用。
    #[allow(dead_code)]
    QuitRequest,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_event_wraps() {
        let ev = FrontendEvent::Key(KeyEvent::ctrl('q'));
        assert_eq!(ev, FrontendEvent::Key(KeyEvent::ctrl('q')));
    }

    #[test]
    fn resize_event_carries_dims() {
        let ev = FrontendEvent::Resize(ResizeEvent {
            width: 80,
            height: 24,
        });
        match ev {
            FrontendEvent::Resize(r) => {
                assert_eq!(r.width, 80);
                assert_eq!(r.height, 24);
            }
            _ => panic!("expected Resize"),
        }
    }

    #[test]
    fn quit_request_variant() {
        assert_eq!(FrontendEvent::QuitRequest, FrontendEvent::QuitRequest);
    }
}
