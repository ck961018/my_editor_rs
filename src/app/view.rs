//! 视图实例的编辑会话：绑定一个 content + 持选区。
//! 按 SpaceId 索引（App.views），同 content 可被多个 View 绑定（多视图铺路）。

use crate::protocol::ids::ContentId;
use crate::protocol::selection::{CursorPos, Selection, Selections};

pub struct View {
    /// 绑定的 content；当前仅 View::new 写入，预留给同 content 多视图解析（spec §10）。
    #[allow(dead_code)]
    content: ContentId,
    selections: Selections,
}

impl View {
    pub fn new(content: ContentId) -> Self {
        Self {
            content,
            selections: Selections::single(Selection::collapsed(CursorPos::origin())),
        }
    }
    #[allow(dead_code)] // 预留：多视图场景下查询 view 绑定的 content
    pub fn content(&self) -> ContentId {
        self.content
    }
    pub fn selections(&self) -> &Selections {
        &self.selections
    }
    pub fn selections_mut(&mut self) -> &mut Selections {
        &mut self.selections
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_view_has_collapsed_origin_selection() {
        let v = View::new(ContentId(0));
        assert_eq!(v.content(), ContentId(0));
        let s = v.selections();
        assert_eq!(s.all().count(), 1);
        assert_eq!(s.primary().head(), CursorPos::origin());
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn selections_mut_allows_edit() {
        let mut v = View::new(ContentId(1));
        v.selections_mut().primary_mut().head = CursorPos {
            char_index: 5,
            row: 0,
            col: 5,
        };
        assert_eq!(v.selections().primary().head().char_index, 5);
    }
}
