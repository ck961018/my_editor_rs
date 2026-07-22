# Command 与执行所有权

**状态：** 当前实现

**更新日期：** 2026-07-22

## 1. 结论

命令和扩展 operation 的语言中立契约属于 `vell-mode`，目标解析和执行属于
`vell-app`，Content 领域 mutation 属于 `vell-core`。

```text
key / timeout / explicit command / script primitive / app event
-> Dispatcher or Mode adapter
-> OperationRequest
-> app target resolver
-> ResolvedOperation queue
-> ExecutionFrame
-> Content / View / TransactionManager / prepared effect
```

任何层都不能通过字符串 effect、可变宿主引用或绕过执行帧的回调建立第二条
mutation 路径。

## 2. 类型所有权

`vell-mode` 拥有：

- `Command`、`AppCommand`、`ModeCommand` 和 `ModeInputCommand`；
- `TransactionIntent` 与 `ViewAction`；
- `OperationRequest`、目标占位类型和 operation payload；
- `ModeResult` 及其有序 operation 列表。

`vell-core` 拥有：

- `EditCommand` 和 `ContentAction`；
- motion、target、operator 与 range 算法；
- `ContentChange`、`TextChangeSet` 和 Content 事务数据；
- `ContentInput`，目前只包含保存请求和 Content event。

`vell-app` 拥有：

- `Dispatcher` 产生的带来源命令；
- `OperationOrigin`、`ResolvedOperation` 和目标解析；
- `ExecutionFrame`、checkpoint、prepared effect 和执行预算；
- App、Scene、View、history、保存和后台任务的实际 mutation。

`vell-protocol` 只保存前后端共享的中立契约，不承载编辑命令执行器。

## 3. 请求与目标解析

`OperationRequest` 用 enum variant 把合法目标和 operation 绑定：

- Content：应用 `ContentAction` 或保存；
- View：编辑、View action、Content action 或 viewport；
- History：begin、commit、rollback、undo 或 redo；
- Mode：调用当前 Content 或 View chain 中的 Mode command；
- Mode input：把输入交给目标 View 的 Mode chain；
- App：执行退出、布局等应用操作。

请求中的 `Current` 不是隐式全局状态。app 结合
`OperationOrigin { scope, view, content, mode }` 解析它，并验证来源 capability、
View 与 Content 绑定以及 history owner。解析后才产生带具体 ID 的
`ResolvedOperation`。

content scope 不能伪造 View operation。view scope 只能作用于绑定的 View 与
Content。保留的显式跨 ID target 在启用前也必须经过相同验证。

## 4. 有序执行

Mode callback 和脚本原语只追加 operation。app 严格按列表顺序执行：

- nested Mode operation 前插到显式队列，保持深度优先顺序；
- command sequence 展开后仍属于同一 execution frame；
- selection-relative edit 在轮到该 operation 时，以当时的 selections 规划；
- 绝对 edit plan 携带 selection 或 revision precondition；
- 后续 operation 可以观察前序 operation 已成功形成的 provisional 状态。

单 frame 最多执行 256 个 operation；nested Mode 与 replayed input 也有独立
预算。所有 producer 共用 `vell-mode` 中的 operation 上限常量。

## 5. ExecutionFrame 原子性

每次物理输入、timeout 或显式命令建立一个 `ExecutionFrame`：

```text
ExecutionFrame
├── CheckpointJournal
├── ModeDraftJournal
├── touched View revisions
├── PreparedEffect[]
└── ExecutionBudget
```

Content、selection 和 input 在第一次写入前按需 checkpoint。
`TransactionManager` 为当前 Content 保存 history flow checkpoint。
Mode state 写入 draft，成功后一次提交。

Save、Quit 和 viewport 先记录为 prepared effect：

- Save 在有序位置捕获完整 `SaveSnapshot`；
- viewport 由 Frontend 根据真实 pane 几何解析；
- Quit 只在 frame 成功后发布。

任一步失败时，app 恢复本 frame 的 Content、View、input 和 history 修改，
丢弃 Mode draft 与 prepared effect。结构化 Mode fault 可以单独提交，
用于隔离失败 attachment，而不是让事件循环停止。

## 6. History 边界

`ExecutionFrame` 不等于 undo/redo history transaction。

`TransactionManager` 按 `ContentId` 持有 transaction flow、history cursor
和 redo 截断。Content 提供可组合、可反向应用的事务数据；View selection
快照作为 participant 数据随记录保存。来源 View 已关闭时，Content 历史仍可
遍历，只跳过无法恢复的 View participant。

Mode state、viewport、focus、布局和 JavaScript heap 状态不进入文本历史。

## 7. 特殊路径

- Save 是 Content operation，但不是 `ContentAction`。
- Undo/redo 是 History operation，不由 Buffer 私有栈处理。
- Viewport 是延迟前端 effect，不进入 Content 或 history。
- Content event 可以更新 Content，无需伪造 Mode。
- 被动 Mode callback 只能更新自身 state 或 presentation，不产生宿主
  operation，避免递归编辑。
