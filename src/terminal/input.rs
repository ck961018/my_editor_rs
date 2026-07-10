use std::io;

use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::{Stream, StreamExt};

use crate::protocol::frontend_event::{FrontendEvent, ResizeEvent};
use crate::protocol::key_event::translate_key;

pub struct Input<S = EventStream>
where
    S: Stream<Item = io::Result<Event>> + Unpin,
{
    events: S,
}

impl Input<EventStream> {
    pub fn new() -> Self {
        Self {
            events: EventStream::new(),
        }
    }
}

impl<S> Input<S>
where
    S: Stream<Item = io::Result<Event>> + Unpin,
{
    /// 测试注入：用任意事件流构造 Input。
    #[cfg(test)]
    pub(crate) fn with_stream(events: S) -> Self {
        Self { events }
    }

    pub async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        loop {
            match self.events.next().await {
                // 流真正结束（如输入源关闭）→ 通知主循环退出。
                None => return Ok(None),
                Some(Err(e)) => return Err(e),
                Some(Ok(ev)) => match map_event(ev) {
                    Some(mapped) => return Ok(Some(mapped)),
                    // map_event 过滤掉的事件（Windows Release / mouse / focus）
                    // 不能当成流结束——继续取下一个，否则主循环会误判 EOF 而 cancel 退出。
                    None => continue,
                },
            }
        }
    }
}

impl Default for Input<EventStream> {
    fn default() -> Self {
        Self::new()
    }
}

/// 纯函数：crossterm Event → FrontendEvent。
/// Windows 上每个物理键有 Press + Release；只接受 Press / Repeat，忽略 Release，
/// 否则每个字符输入两次、回车换两行。Unix 只发 Press，过滤为 no-op。
fn map_event(ev: Event) -> Option<FrontendEvent> {
    match ev {
        Event::Key(k) => {
            if k.kind == KeyEventKind::Release {
                None
            } else {
                Some(FrontendEvent::Key(translate_key(k)))
            }
        }
        Event::Resize(w, h) => Some(FrontendEvent::Resize(ResizeEvent {
            width: w,
            height: h,
        })),
        _ => None, // mouse / focus 等忽略
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frontend_event::{FrontendEvent, ResizeEvent};
    use crate::protocol::key_event::KeyEvent;
    use crossterm::event::{Event, KeyCode, KeyEvent as CrosstermKey, KeyEventKind, KeyModifiers};

    fn key_event(code: KeyCode, kind: KeyEventKind) -> CrosstermKey {
        CrosstermKey::new_with_kind(code, KeyModifiers::empty(), kind)
    }

    #[test]
    fn release_event_is_ignored() {
        let ev = Event::Key(key_event(KeyCode::Char('a'), KeyEventKind::Release));
        assert_eq!(map_event(ev), None);
    }

    #[test]
    fn press_event_translates() {
        let ev = Event::Key(key_event(KeyCode::Char('a'), KeyEventKind::Press));
        assert_eq!(map_event(ev), Some(FrontendEvent::Key(KeyEvent::char('a'))));
    }

    #[test]
    fn repeat_event_translates() {
        // 按住键时的 Repeat 仍应触发输入，不能被过滤
        let ev = Event::Key(key_event(KeyCode::Char('a'), KeyEventKind::Repeat));
        assert_eq!(map_event(ev), Some(FrontendEvent::Key(KeyEvent::char('a'))));
    }

    #[test]
    fn resize_event_translates() {
        let ev = Event::Resize(80, 24);
        assert_eq!(
            map_event(ev),
            Some(FrontendEvent::Resize(ResizeEvent {
                width: 80,
                height: 24
            }))
        );
    }

    #[tokio::test]
    async fn next_event_skips_filtered_events_until_mappable() {
        use futures::stream;
        // Windows 上每次按键发 Press + Release；Release 被 map_event 过滤。
        // next_event 必须跳过 Release 取到下一个 Press，而不是把 Release 当流结束。
        let release = Event::Key(key_event(KeyCode::Char('a'), KeyEventKind::Release));
        let press = Event::Key(key_event(KeyCode::Char('a'), KeyEventKind::Press));
        let mut input =
            Input::with_stream(stream::iter(vec![Ok::<_, io::Error>(release), Ok(press)]));
        let ev = input.next_event().await.unwrap();
        assert_eq!(ev, Some(FrontendEvent::Key(KeyEvent::char('a'))));
    }

    #[tokio::test]
    async fn next_event_returns_none_only_on_stream_end() {
        use futures::stream;
        let mut input = Input::with_stream(stream::iter(Vec::<io::Result<Event>>::new()));
        let ev = input.next_event().await.unwrap();
        assert_eq!(ev, None);
    }
}
