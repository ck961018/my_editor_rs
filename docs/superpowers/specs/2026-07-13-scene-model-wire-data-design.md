# Scene 模型与协议数据分离设计

**日期：** 2026-07-13
**状态：** 已实施

## 目标

- `protocol::scene` 只定义可拥有、可复制和可传输的 Scene 快照数据及只读访问。
- `app::scene_model` 拥有 SpaceId 分配、标准场景构建、split、close、replace 与 sizing 修改。
- TUI 只消费 `&Scene`，不依赖 app 或获得 Scene 修改能力。

## 边界

```text
app::SceneBuilder --修改--> protocol::Scene --只读--> Frontend/TUI
        │                         │
        └─ 分配与树不变量          └─ root/size/nodes 快照数据
```

`Scene` 保留 root、size、node 集合和只读查询；resize 由 session 直接更新快照字段。树校验、
View 去重、节点修复以及 mutation result/error 都属于后端模型。`build_editor_scene` 仍接收
ClientSession 长期持有的唯一 `SceneBuilder`，保证 SpaceId 不复用。

TUI 单元测试使用前端本地的纯数据 fixture 构造 Scene，不反向依赖 app 模型。当前不选择 serde、
snapshot/delta 编码或网络 transport；这些在真正实现远程 Scene 消息时再决定。

## 验收

- `protocol::scene` 不再出现 `SceneBuilder`、split、close、repair 或模型错误。
- 所有生产 Scene 修改入口位于 app 层，现有布局不变量测试随模型迁移。
- TUI 生产代码和测试均不依赖 app。
- 标准布局、split/close、焦点回退、Taffy 布局与渲染测试保持通过。
- fmt、test、clippy 与 diff 检查通过。
