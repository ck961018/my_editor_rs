//! 几何原语：Size/Rect/Point。纯数据，前后端共享。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.width).min(other.x + other.width);
        let y1 = (self.y + self.height).min(other.y + other.height);
        if x1 > x0 && y1 > y0 {
            Some(Rect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            })
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    dead_code,
    reason = "integer point is a neutral geometry primitive reserved for frontend adapters"
)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rect_intersect() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let o = Rect {
            x: 5,
            y: 5,
            width: 10,
            height: 10,
        };
        assert_eq!(
            r.intersect(&o),
            Some(Rect {
                x: 5,
                y: 5,
                width: 5,
                height: 5
            })
        );
        let far = Rect {
            x: 20,
            y: 20,
            width: 5,
            height: 5,
        };
        assert_eq!(r.intersect(&far), None);
    }
}
