# Command 与执行所有权

**状态：** 当前实现

**更新日期：** 2026-08-02

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

注册命令是一条平行入口，最终汇入同一个 `ExecutionFrame`：

```text
key / command line / explicit API / TypeScript
-> CommandInvocation
-> CommandRegistry
-> native or script CommandAdapter
-> scoped CommandHost
-> typed request / owned query
-> current ExecutionFrame
```

任何层都不能通过字符串 effect、可变宿主引用或绕过执行帧的回调建立第二条
mutation 路径。

## 2. 类型所有权

`vell-mode` 拥有：

- `Command`、`AppCommand`、`ModeCommand` 和 `ModeInputCommand`；
- `TransactionIntent` 与 `ViewAction`；
- `OperationRequest`、目标占位类型和 operation payload；
- `ModeResult` 及其有序 operation 列表；
- `CommandId`、`CommandInvocation`、`CommandEntry` 与 `CommandRegistry`；
- `CommandAdapter`、`CommandHost`、`CommandRequest` 与 `CommandQuery`；
- `CommandPending`、`CommandContinuation` 与 `CommandError`。

`Command::Registered(CommandInvocation)` 让 keymap 直接绑定注册命令。
现有 Mode-local `ModeCommand` 保持不变，两套命名空间不合并。

`vell-core` 拥有：

- `EditCommand` 和 `ContentAction`；
- motion、target、operator 与 range 算法；
- `ContentChange`、`TextChangeSet` 和 Content 事务数据；
- `ContentInput`，目前只包含保存请求和 Content event。

`vell-app` 拥有：

- `Dispatcher` 产生的带来源命令；
- `OperationOrigin`、`ResolvedOperation` 和目标解析；
- `ExecutionFrame`、checkpoint、prepared effect 和执行预算；
- `CommandRegistry` 实例、native 命令实现和 scoped `CommandHost`；
- pending command 表、目标 pinning 与 continuation 恢复；
- App、Scene、View、history、保存和后台任务的实际 mutation。

`vell-protocol` 只保存前后端共享的中立契约，不承载编辑命令执行器，也不
枚举或调用命令。

## 3. 请求与目标解析

`OperationRequest` 用 enum variant 把合法目标和 operation 绑定：

- Content：应用 `ContentAction` 或保存；
- View：编辑、View action、Content action 或 viewport；
- History：begin、commit、rollback、undo 或 redo；
- Mode：调用当前 Content 或 View chain 中的 Mode command；
- Mode input：把输入交给目标 View 的 Mode chain；
- App：执行退出、布局等应用操作；
- ExecuteCommandLine：把一行原始命令文本交给通用命令服务。

`ExecuteCommandLine` 只携带 `source`。它要求 view scope 的来源，解析后绑定
当时的 View 与 Content。解析规则本身不属于 app，见第 9 节。

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
- 后续 Content、View、History 和 Mode operation 可以观察前序 operation 已成功
  形成的 provisional 状态；
- topology effect 不提前改写 Scene。一个 frame 最多包含一个 split、close 或
  focus，且 topology 与 viewport effect 不能混用；违反约束时整个 frame 回滚。

单 frame 最多执行 256 个 operation；nested Mode 与 replayed input 也有独立
预算。所有 producer 共用 `vell-mode` 中的 operation 上限常量。注册命令另有
256 层调用深度预算，防止 native、TypeScript 和 shortcut 互相递归耗尽栈。

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

Save、Quit、布局和 viewport 先记录为 prepared effect：

- Save 在有序位置捕获完整 `SaveSnapshot`；
- viewport 由 Frontend 根据真实 pane 几何解析；
- split 捕获目标 Space、Content 和方向，focus 捕获已解析的目标 Space；
- close 捕获当前 Space；关闭最后一个可聚焦 pane 时转成 Quit；
- Quit 只在 frame 成功后发布。

任一步失败时，app 恢复本 frame 的 Content、View、input 和 history 修改，
丢弃 Mode draft 与 prepared effect。结构化 Mode fault 可以单独提交，
用于隔离失败 attachment，而不是让事件循环停止。

## 6. 注册命令与 scoped host

`Kernel` 持有唯一 `CommandRegistry`。同 ID 重复注册替换当前实现，包括
替换 Rust 原生命令；已绑定该 ID 的 key binding 随之调用新实现。注册表按
`CommandId` 稳定迭代，注册和查找不需要 V8、app 或 TUI。

`vell-app` 为一次同步执行段构造 `ScopedCommandHost`，它借用当前 App 和
`ExecutionFrame`，只暴露四种请求：

- `Execute`：立即执行一个 typed `OperationRequest`；
- `ExecuteAsync`：执行后返回 host task，交给 continuation；
- `CreateBuffer`：创建 Content 并立即返回 `ContentId`；
- `Query`：返回 owned 快照或 ID。

host 不借出 App、Kernel、Session、Content 或 View。嵌套命令共享同一个
host、frame、预算和 origin，因此后序命令可以观察前序 operation 形成的
provisional 状态：`switchBuffer(newBuffer())` 在一个 frame 内成立。

`CreateBuffer` 把新 Content 记入 frame 的 provisional journal，失败时随
frame 一起回收。这里不建立第二个 ContentStore，也没有通用 shadow App；
只有确实需要立即观察的 lifecycle operation 才提供 provisional 数据。

Mode operation 是例外。`OperationRequest::Mode` 会重入 Mode callback，
而命令可能正在 V8 callback 内部执行，因此 host 把它排入 deferred 队列，
在同步段结束后按顺序执行。命令失败时 deferred 队列整体丢弃。

`register_command` 属于 definition state，不进入 host mutation journal。
即使同一次执行随后抛错，已完成的注册仍然保留；definition event 不能携带
Content 或 View mutation。

## 7. 异步命令与 continuation

`ExecuteAsync` 分配 `CommandTaskId`，并把它关联到本 frame 中刚 prepare 的
host task。适配器返回 `CommandCompletion::Pending`，app 把 continuation
连同当时的 View 和 Content 一起登记到 pending command 表，然后正常提交
当前 frame。

```text
sync segment -> commit frame -> host task -> completion
-> new ExecutionFrame pinned to the original view/content
-> resume continuation -> commit or roll back the new frame
```

恢复时创建的新 frame 继续绑定命令启动时的目标。await 之前的修改已经提交，
await 之后的异常只回滚新 frame。以下情况取消 continuation 而不是改用当前
焦点：目标 View 关闭、目标 revision 失效、编辑器退出。stale completion
不会 resolve 其他 invocation。

## 8. 命令行请求

`ExecuteCommandLine` 是语言中立的一行文本请求。app 只做三件事：校验 view
scope、解析目标、把文本转成对固定服务命令 `$commandLine.execute` 的
`CommandInvocation`。

Rust 层不包含 Vim、`wq` 或 Ex parser 分支。分类、shortcut 分派和 TypeScript
求值属于脚本宿主，见
[TypeScript 脚本架构](typescript-scripting-architecture.md)。服务命令返回
`Pending` 时，与其他异步命令走同一条 continuation 路径。

## 9. History 边界

`ExecutionFrame` 不等于 undo/redo history transaction。

`TransactionManager` 按 `ContentId` 持有 transaction flow、history cursor
和 redo 截断。Content 提供可组合、可反向应用的事务数据；View selection
快照作为 participant 数据随记录保存。来源 View 已关闭时，Content 历史仍可
遍历，只跳过无法恢复的 View participant。

Mode state、viewport、focus、布局和 JavaScript heap 状态不进入文本历史。
命令与快捷命令注册同样不进入文本历史。

## 10. 特殊路径

- Save 是 Content operation，但不是 `ContentAction`。
- Undo/redo 是 History operation，不由 Buffer 私有栈处理。
- Viewport 和布局是延迟 effect，不进入 Content 或 history。
- Content event 可以更新 Content，无需伪造 Mode。
- 被动 Mode callback 只能更新自身 state 或 presentation，不产生宿主
  operation，避免递归编辑。
- 注册命令通过原生 `invokeMode` 命令进入 Mode command，而不是获得第二条
  Mode 调用路径。
