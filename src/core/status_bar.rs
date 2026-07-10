use crate::core::content::{ContentHandler, ContentLookup};
use crate::core::keymap::Keymap;
use crate::protocol::content_query::StatusBarData;
use crate::protocol::ids::ContentId;
use crate::protocol::status::StatusMessage;

/// 状态栏 content：观察 target_content_id 指向的 content，查询时主动查其
/// file_name/modified/status。自身不持显示数据，只持指针 + 空 keymap。
pub struct StatusBar {
    target_content_id: ContentId,
    keymap: Keymap,
}

impl StatusBar {
    pub fn new(target_content_id: ContentId) -> Self {
        Self {
            target_content_id,
            keymap: Keymap::new(),
        }
    }
    #[allow(dead_code)] // 测试用
    pub fn target_content_id(&self) -> ContentId {
        self.target_content_id
    }
    /// 产状态栏显示数据：查 target content 的 file_name/modified/status。
    pub fn status_bar_data(&self, lookup: &dyn ContentLookup) -> StatusBarData {
        let target = lookup.get(self.target_content_id);
        StatusBarData {
            file_name: target
                .and_then(|c| c.as_buffer())
                .and_then(|b| b.file_name().map(|s| s.to_string())),
            modified: target
                .and_then(|c| c.as_buffer())
                .map(|b| b.modified())
                .unwrap_or(false),
            message: target
                .and_then(|c| c.as_buffer())
                .map(|b| b.status())
                .unwrap_or(StatusMessage::None),
        }
    }
}

impl ContentHandler for StatusBar {
    fn keymap(&self) -> &Keymap {
        &self.keymap
    }
    fn keymap_mut(&mut self) -> &mut Keymap {
        &mut self.keymap
    }
    fn as_status_bar(&self) -> Option<&StatusBar> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::protocol::ids::ContentId;

    fn lookup_with(buf: &Buffer, target: ContentId) -> impl ContentLookup {
        struct L<'a> {
            buf: &'a Buffer,
            target: ContentId,
        }
        impl<'a> ContentLookup for L<'a> {
            fn get(&self, id: ContentId) -> Option<&dyn ContentHandler> {
                if id == self.target {
                    Some(self.buf)
                } else {
                    None
                }
            }
        }
        L { buf, target }
    }

    #[test]
    fn status_bar_data_target_missing_defaults() {
        let sb = StatusBar::new(ContentId(0));
        let buf = Buffer::new();
        let data = sb.status_bar_data(&lookup_with(&buf, ContentId(9)));
        assert!(data.file_name.is_none());
        assert!(!data.modified);
        assert_eq!(data.message, StatusMessage::None);
    }

    #[test]
    fn target_content_id_stored() {
        let sb = StatusBar::new(ContentId(7));
        assert_eq!(sb.target_content_id(), ContentId(7));
    }
}
