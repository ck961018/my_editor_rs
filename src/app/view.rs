//! 视图实例的编辑会话：绑定一个 content + 持选区。
//! 按 SpaceId 索引（App.views），同 content 可被多个 View 绑定（多视图铺路）。

use crate::core::content_runtime::ContentRuntime;
use crate::protocol::ids::ContentId;
use crate::protocol::selection::{CursorPos, Selection, Selections};

pub struct View {
    /// 绑定的 content；当前仅 View::new 写入，预留给同 content 多视图解析（spec §10）。
    #[allow(dead_code)]
    content: ContentId,
    selections: Selections,
    runtime: ContentRuntime,
}

impl View {
    pub fn new(content: ContentId, runtime: ContentRuntime) -> Self {
        Self {
            content,
            selections: Selections::single(Selection::collapsed(CursorPos::origin())),
            runtime,
        }
    }
    #[allow(dead_code)] // 预留：多视图场景下查询 view 绑定的 content
    pub fn content(&self) -> ContentId {
        self.content
    }
    pub fn selections(&self) -> &Selections {
        &self.selections
    }
    #[allow(dead_code)] // Task 3 reads the focused View runtime during key resolution.
    pub fn runtime(&self) -> &ContentRuntime {
        &self.runtime
    }
    pub fn selections_and_runtime_mut(&mut self) -> (&mut Selections, &mut ContentRuntime) {
        (&mut self.selections, &mut self.runtime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::content_runtime::{ContentRuntime, StatusBarRuntime};

    #[test]
    fn new_view_has_collapsed_origin_selection() {
        let v = View::new(ContentId(0), ContentRuntime::StatusBar(StatusBarRuntime));
        assert_eq!(v.content(), ContentId(0));
        let s = v.selections();
        assert_eq!(s.all().count(), 1);
        assert_eq!(s.primary().head(), CursorPos::origin());
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn view_borrows_selections_and_runtime_together() {
        let mut view = View::new(ContentId(0), ContentRuntime::StatusBar(StatusBarRuntime));
        let (selections, runtime) = view.selections_and_runtime_mut();

        selections.primary_mut().head.char_index = 3;
        assert!(matches!(runtime, ContentRuntime::StatusBar(_)));
    }
}
