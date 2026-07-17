#[expect(
    dead_code,
    reason = "scene identity is reserved for multi-scene sessions"
)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SceneId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SpaceId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ViewId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ContentId(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_copy_eq_hash() {
        let a = SpaceId(1);
        let b = a;
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(ContentId(2));
        assert!(set.contains(&ContentId(2)));
        let view = ViewId(3);
        assert_eq!(view, ViewId(3));
    }
}
