# View 与 Space 身份分离设计

**日期：** 2026-07-13
**状态：** 已实施

## 目标

- 新增独立 `ViewId`；`SpaceId` 只标识布局节点，`ViewId` 标识展示与交互会话，
  `ContentId` 标识共享内容。
- Scene 的 Content leaf 只引用 `ViewId`，不再直接引用 `ContentId`。
- App 以 `ViewId` 保存 View；View 继续引用 Content，并独立持有 ModeInstance、selection
  与 ContentViewState。
- 前端通过 `ViewId` 查询 ViewData，再使用其中的 `ContentId` 拉取共享内容。
- TUI viewport 按 `ViewId` 保存，使 View 移动到另一个 Space 后仍保留 viewport。

## 身份链

```text
SpaceId -> ViewId -> ContentId
 layout    session    shared model
```

一个 View 同时只能被一个 Scene leaf 引用。拆分同一 Content 时创建新的 ViewId；关闭 leaf
时删除对应 View；替换 leaf 内容时创建新 View，避免旧会话状态错误复用。布局 sizing、focus
和几何仍按 SpaceId 管理。

## 命令与渲染

Dispatcher 将编辑命令解析到 `ViewId + ContentId`。App 用 ViewId 取得会话状态，并用
ContentId 执行共享内容命令。

`RenderQuery::view` 改为接收 ViewId。`ViewData` 携带它绑定的 ContentId；TUI 的布局结果
保留 SpaceId 与 ViewId，几何使用 SpaceId，viewport 与 ViewData 使用 ViewId。

## 非目标

- 不引入多客户端 Session；仍只有一个 App 和一份 Scene/Focus/View 集合。
- 不泛化 presentation；仍沿用当前文本/状态栏查询，这属于下一项。
- 不改变 ContentId 或 SpaceId 的分配策略，不实现 View 跨 Scene 共享。

## 验收

- Scene leaf 不再包含 ContentId。
- 同一 Content 的两个 View 拥有不同 ViewId，Mode 与 selection 保持独立。
- 将同一 ViewId 绑定到不同 SpaceId 时，前端查询身份与 viewport 身份不变。
- 现有布局、编辑、保存和渲染测试通过。
- fmt、test、clippy 与 diff 检查通过。
