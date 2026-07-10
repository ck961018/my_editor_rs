#[allow(dead_code)] // 预留 id 类型，v0.2+ 多场景时启用
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SceneId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SpaceId(pub u64);

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
    }
}
