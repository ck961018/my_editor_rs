//! Selection 数据模型：cursor 是 selection 的退化形态（collapsed，anchor==head）。
//! Helix 风集合：ranges + primary_index；逻辑行列由 Buffer 按需派生。

/// 文档 char offset。Selection 长期只保存这一种位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextOffset {
    pub char_index: usize,
}

impl TextOffset {
    pub const fn origin() -> Self {
        Self { char_index: 0 }
    }
}

/// 从当前 Buffer 内容派生的逻辑行列，不写回 Selection。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPoint {
    pub row: usize,
    pub col: usize,
}

/// 选区：anchor 选择起点，head 光标位置（驱动编辑/渲染）。空 selection：anchor==head。
/// 方向隐含：head>anchor=forward。不加 direction 字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: TextOffset,
    pub head: TextOffset,
}

impl Selection {
    pub fn collapsed(at: TextOffset) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
    pub fn head(&self) -> TextOffset {
        self.head
    }
}

/// 多选区容器（Helix 风）。ranges 按 head.char_index 升序，primary_index 指向主选区。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selections {
    ranges: Vec<Selection>,
    primary_index: usize,
}

impl Selections {
    pub fn single(sel: Selection) -> Self {
        Self {
            ranges: vec![sel],
            primary_index: 0,
        }
    }

    pub fn primary(&self) -> &Selection {
        &self.ranges[self.primary_index]
    }
    pub fn primary_mut(&mut self) -> &mut Selection {
        &mut self.ranges[self.primary_index]
    }
    pub fn all(&self) -> impl Iterator<Item = &Selection> {
        self.ranges.iter()
    }
    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut Selection> {
        self.ranges.iter_mut()
    }

    /// 清除 secondary ranges，仅保留 primary。
    pub fn retain_primary(&mut self) {
        let primary = self.ranges[self.primary_index];
        self.ranges = vec![primary];
        self.primary_index = 0;
    }

    /// 构造多选区集合；`primary_index` 必须指向一个 range。
    pub fn from_parts(ranges: Vec<Selection>, primary_index: usize) -> Self {
        assert!(primary_index < ranges.len());
        Self {
            ranges,
            primary_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapsed_is_empty() {
        let s = Selection::collapsed(TextOffset::origin());
        assert!(s.is_empty());
        assert_eq!(s.head(), TextOffset::origin());
    }

    #[test]
    fn non_empty_selection() {
        let s = Selection {
            anchor: TextOffset::origin(),
            head: TextOffset { char_index: 3 },
        };
        assert!(!s.is_empty());
    }

    #[test]
    fn single_has_one_range_primary_index_zero() {
        let s = Selections::single(Selection::collapsed(TextOffset::origin()));
        assert_eq!(s.primary(), &Selection::collapsed(TextOffset::origin()));
        let count = s.all().count();
        assert_eq!(count, 1);
    }

    #[test]
    fn primary_mut_updates_head() {
        let mut s = Selections::single(Selection::collapsed(TextOffset::origin()));
        s.primary_mut().head = TextOffset { char_index: 5 };
        assert_eq!(s.primary().head().char_index, 5);
    }

    #[test]
    fn all_mut_updates_all_ranges() {
        let mut s = Selections::from_parts(
            vec![
                Selection::collapsed(TextOffset::origin()),
                Selection::collapsed(TextOffset { char_index: 3 }),
            ],
            0,
        );
        for sel in s.all_mut() {
            sel.head = TextOffset { char_index: 9 };
        }
        assert_eq!(s.all().count(), 2);
        assert!(s.all().all(|sel| sel.head.char_index == 9));
    }

    #[test]
    fn retain_primary_drops_secondaries() {
        let mut s = Selections::from_parts(
            vec![
                Selection::collapsed(TextOffset::origin()),
                Selection::collapsed(TextOffset { char_index: 3 }),
            ],
            0,
        );
        s.retain_primary();
        assert_eq!(s.all().count(), 1);
        assert_eq!(s.primary(), &Selection::collapsed(TextOffset::origin()));
    }

    #[test]
    fn retain_primary_on_single_is_noop() {
        let mut s = Selections::single(Selection::collapsed(TextOffset::origin()));
        s.retain_primary();
        assert_eq!(s.all().count(), 1);
    }
}
