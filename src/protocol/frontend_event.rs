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
    /// 前端请求退出，例如窗口式前端的关闭按钮。
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the terminal frontend exits through its configured key path"
        )
    )]
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
