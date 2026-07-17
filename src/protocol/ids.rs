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
