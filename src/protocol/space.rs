//! 空间节点：布局意图。纯数据，前后端共享。不含 viewport/cursor（前端持）。

use crate::protocol::ids::{SpaceId, ViewId};

#[derive(Clone)]
pub struct Space {
    #[allow(dead_code)] // 结构性 identity 字段
    pub id: SpaceId,
    pub kind: SpaceKind,
    pub sizing: Sizing,
    pub layer: Layer,
}

#[derive(Clone)]
pub enum SpaceKind {
    Container { arrangement: Arrangement },
    Content { view: ViewId, focusable: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    Left,
    Right,
    Up,
    Down,
}

impl SplitDirection {
    pub const fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }

    pub const fn inserts_before(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
}

#[derive(Clone)]
pub enum Arrangement {
    Flex {
        direction: Axis,
        gap: i32,
        align: Align,
    },
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Stretch,
    Start,
    Center,
    End,
}

#[derive(Clone)]
pub enum Sizing {
    Fixed(i32),
    Grow(u32),
}

#[repr(i32)]
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    Base = 0,
    Overlay = 10,
    Modal = 20,
    Debug = 100,
}

impl PartialOrd for Layer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Layer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as i32).cmp(&(*other as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layer_orders_by_discriminant() {
        assert!(Layer::Base < Layer::Overlay);
        assert!(Layer::Overlay < Layer::Modal);
        assert!(Layer::Modal < Layer::Debug);
    }
}
