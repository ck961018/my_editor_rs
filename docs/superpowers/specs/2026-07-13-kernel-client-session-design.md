# Kernel 与 ClientSession 分层设计

**日期：** 2026-07-13
**状态：** 已实施

## 目标

- 将可由多个客户端共享的内核状态与单客户端会话状态分成独立所有权实体。
- 保持当前单 Frontend 行为与泛型静态分发，不提前引入多客户端调度或锁。
- 为未来一个 Kernel 对应多个 ClientSession 提供直接扩展点。

## 所有权

```text
App<F>
├── Kernel
│   ├── ContentStore
│   ├── ModeRegistry
│   ├── AppTasks
│   ├── save message channel
│   └── pending saves
├── ClientSession
│   ├── Scene + SceneBuilder + scene revision
│   ├── focused SpaceId
│   ├── ViewStore + ViewId allocator
│   └── Dispatcher (包含客户端前缀键状态)
└── Frontend
```

Content 与 Mode 定义属于 Kernel；View、ModeInstance、selection、布局、焦点和按键前缀状态
属于 ClientSession。后台保存修改共享 Content，因此也属于 Kernel。

## Viewport

TUI viewport 继续由 `SceneRenderer` 持有。它是具体前端 presentation/layout 状态，放入
后端 ClientSession 会违反现有前后端边界。未来每个远程客户端拥有自己的 Frontend/TUI
实例，因此仍能自然获得独立 viewport。

## 当前接线

当前 `App<F>` 仍只持有一个 Kernel、一个 ClientSession 和一个 Frontend，事件循环无需新增
集合、Arc、Mutex 或调度器。布局入口在 App 上协调共享 Content 与会话；成功后只修改该
ClientSession。

## 非目标

- 不同时运行多个 Frontend，不实现 session registry、认证、生命周期或并发调度。
- 不把 ContentStore 放入 Arc/Mutex，不引入跨线程共享。
- 不移动 Taffy、viewport 或 Canvas 到后端。
- 不改变远程消息、Scene wire data 或文本位置模型。

## 验收

- App 顶层不再混放 ContentStore 与 Scene/ViewStore。
- 两个 ClientSession 可引用同一个 Kernel 的 Content 数据，并拥有不同 Scene、focus、View
  与 Dispatcher 状态。
- 编辑、布局、保存、远程查询和渲染行为保持通过。
- fmt、test、clippy 与 diff 检查通过。
