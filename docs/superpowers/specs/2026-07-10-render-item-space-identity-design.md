# RenderItem Space Identity Design

## Goal

修复同一 `ContentId` 在多个 `SpaceId` 中显示时，TUI 错误复用第一个
space 的 selection 和 viewport 的问题。渲染项必须保留其来源 space 的身份。

## Scope

- 将 `SpaceKind::Host` 改名为 `SpaceKind::Content`。
- 让 `RenderItem` 同时携带 `space_id: SpaceId` 与 `content_id: ContentId`。
- 删除渲染阶段按 `ContentId` 反查 `SpaceId` 的路径。
- 为同一内容多 space 的 selection 渲染添加回归测试。

本设计不改变 `Space` 的树模型，不处理 `children` 的重复存储，也不实现
split、tab、浮层或动态 View 生命周期。

## Terminology And Ownership

`Space` 是 UI scene 树的统一节点类型。根、内部节点和内容叶子都是
`Space`；`SpaceId` 是该节点的稳定身份。

`SpaceKind` 仍承担必要的节点行为判别：

- `Container { arrangement, children }` 是布局内部节点。
- `Content { content }` 是绑定 `ContentId` 并参与内容渲染的叶子节点。

本次仅将 `Host` 重命名为 `Content`。`Host` 没有说明它承载的是共享内容，
而 `Content` 与 `ContentId` 的绑定关系直接、明确。`Window` 不作为
`Space` 的替代名称，因为 Vim/Emacs 语义中的 window 仅对应可见内容区域，
不包含根和内部布局节点。

内容和每个可见实例的状态分别归属：

- `ContentId`：共享文本、状态栏等内容数据。
- `SpaceId`：某内容的可见实例身份；`View` 的 selection 和 TUI viewport
  均以它为键。

## Rendering Data Flow

`TaffyEngine::collect` 在遍历 `SpaceKind::Content` 时已同时拥有当前的
`SpaceId` 和绑定的 `ContentId`。它构造：

```rust
RenderItem {
    space_id,
    content_id,
    rect,
    clip,
    layer,
    z_index,
    order,
}
```

`SceneRenderer` 对每个渲染项使用两种身份：

- `item.content_id` 传给 `ContentQuery::lines` 和 `ContentQuery::status_bar`，
  读取共享内容。
- `item.space_id` 查询 `ContentQuery::selections` 并索引内部 viewport 缓存。

渲染器不得再通过 scene 和 `ContentId` 搜索 space。该搜索在内容被多个
space 共享时不唯一，深度优先找到的第一个 space 会污染其他可见实例的
selection 和 viewport。

聚焦 viewport 跟随也必须依据 `item.space_id == focused`，而不是依据内容
身份匹配。

## Error Handling And Compatibility

本变更不新增错误类型，也不修改 `ContentQuery` 的公开接口。现有查询缺失
selection 的回退行为保持不变；本设计只确保渲染器传入正确的 `SpaceId`。

`SpaceKind` 的改名是行为不变的术语调整。`Container` 与 `Content` 的
children 约束、Sizing、Layer 和 SceneBuilder 的分配语义均维持原样。

## Tests

新增或调整以下测试：

1. `TaffyEngine`：构造两个 `SpaceKind::Content` 指向同一个 `ContentId`；
   断言生成两个 `RenderItem`，且分别携带对应、不同的 `SpaceId`。
2. `SceneRenderer`：构造两个并排 content space，共享一个 `ContentId`；
   测试查询对象按 `SpaceId` 返回不同的 selection；断言左右渲染区域分别
   使用各自的反白区间。旧实现会令两个区域都使用第一个 space 的 selection，
   因而该测试应先失败。

完成实现后运行 `cargo test` 和
`cargo clippy --all-targets --all-features`。
