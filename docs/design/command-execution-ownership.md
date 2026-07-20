# Command 与执行所有权设计

**状态：** 已实施的历史决策；当前执行模型见
[`editor-kernel-architecture.md`](editor-kernel-architecture.md)

**日期：** 2026-07-17

## 1. 结论

顶层输入路由属于 app，不属于 core，也不属于 Content。

目标数据流是：

```text
key / timeout / script / event
-> app route
-> ModeAction 或直接 typed action
-> ordered operations
-> app validation
-> View / Content / TransactionManager / App
```

`Command` 只作为 app 输入和 keymap 的路由类型。它不能继续充当 Content
领域接口。

## 2. 类型所有权

app 拥有：

- `Command`；
- `AppCommand`；
- `ModeCommand`；
- `ViewAction`；
- `TransactionIntent`；
- Dispatcher 的目标解析类型。

core 拥有：

- `ContentAction`；
- 纯 motion、target 和 range 算法；
- ContentChange；
- Content 自己的事务数据。

protocol 只保存跨前后端需要共享的中立数据，不承载 app 命令执行器。

## 3. Mode 输出

`ContentModeResult` 只能产生 ContentAction 和 TransactionIntent。
它不能产生 ViewAction、View presentation 或任意 View identity。

`ViewModeResult` 可以产生：

- 目标为绑定 View 的 ViewAction；
- 目标为绑定 Content 的 ContentAction；
- TransactionIntent；
- 需要 app 验证的 AppAction。

结果是有序 operation 列表。执行器不得根据 operation 集合猜测顺序。

ModeState 在一次 ordered result 执行期间写入 `ModeDraftJournal`。同一 frame
内后续 callback 读取最新 draft；执行失败时丢弃，成功后一次提交。Mode state
不进入 undo history。

## 4. Content 输入

Content 只接收 ContentAction、Content event、保存快照请求和事务数据应用。
它不接收顶层 Command，不接收可变 `ContentViewState`，也不执行 ViewAction。

ContentAction 必须包含执行所需的已解析目标。文本删除或替换使用确定的
range，不能要求 Content 在执行期间读取活动 selection。

Content 应返回规范 ContentChange。app 再把 change 映射到所有绑定 View，
并按 ordered result 应用来源 View 的显式 ViewAction。

## 5. Dispatcher

Dispatcher 负责：

- 组合 effective Mode keymap 与 global keymap；
- 管理固定序列、动态 capture 和 timeout；
- 记录输入来源 View；
- 把顶层 Command 解析为显式目标。

当 Content 绑定 ContentMode 时，Dispatcher 使用共享实例；否则使用来源
View 的 ViewModeInstance。来源 View 只用于外部目标和事务 participant，
不会进入 ContentModeContext。

Dispatcher 不执行 ContentAction，也不包含 Vim 等具体 Mode 分支。

## 6. 特殊路径

Save 解析为目标 Content 的 checkpoint 与 snapshot 请求，不是
ContentAction。

Undo/redo 解析为目标 Content 的 TransactionManager 历史遍历，不是
ContentAction，也不由 Buffer 私有 history 直接处理。

Viewport command 仍由 Frontend 根据 pane 几何解析。滚动请求解析为行数，
并按 cursor behavior 产生 View edit；对齐请求把当前 cursor row 解析为
`SetTopRow`，不修改 cursor。两者都先形成 `ResolvedViewportCommand`，
viewport 状态变更延迟到整个 ordered result 成功后提交。因此后续 operation
失败时不会留下前端副作用，viewport 状态也不进入 Content 或事务历史。

Content event 可以直接产生 ContentChange 或事务请求，不需要伪造 Mode。

## 7. 原子性

app 在应用 ordered result 时逐项验证：

- Mode binding 和 action；
- View 与 Content 目标关系；
- ViewAction selections 能由目标 Content 表示，且静态 edit 的起点仍匹配；
- ContentAction 数据；
- TransactionIntent 生命周期；
- AppAction 权限和布局约束。

任一步失败时，执行器恢复该 frame 已修改的 Content、View selections、input
和 TransactionManager checkpoint，并丢弃 Mode state draft。历史来源 View
已关闭时只跳过其 selections 快照，Content 历史仍正常遍历。

成功的 ViewMode 可变回调如果没有通过 ViewAction 改变 View revision，app
仍递增一次 revision，使 presentation 等纯 ModeState 变化能够触发重绘。

## 8. 迁移完成标准

- core 不再定义顶层 Command、AppCommand 或 ModeCommand；
- Content 不再接收 `ContentInput::View`；
- selection 移动和压缩只通过 ViewAction；
- 文本修改只通过 ContentAction；
- Save 和 undo/redo 只通过 app typed 路径；
- Mode timeout 和普通 action 使用同一种有序结果；
- 所有失败路径恢复 provisional ModeState；
- keymap、脚本和 event 可以复用 action 执行器而无需伪造 Mode。
