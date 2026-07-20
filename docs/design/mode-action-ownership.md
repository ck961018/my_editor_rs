# Mode 与 Action 所有权设计

**状态：** 历史基线；当前 Mode 类型模型见
[`editor-kernel-architecture.md`](editor-kernel-architecture.md)

**日期：** 2026-07-17

## 1. 目标

本文保留早期 Mode、View、Content 和顶层路由的能力边界决策。
`ContentMode`/`ViewMode` 二选一模型已被当前统一 Mode contract 取代，
不再描述当前实现。

核心规则是：Mode 负责解释输入和编排操作，View 保存会话状态，Content
保存领域数据，app 验证目标并执行有序结果。

## 2. 静态 Mode 契约

注册表只接受两种定义：

```rust
enum RegisteredMode {
    PerContent(Box<dyn ContentMode>),
    PerView(Box<dyn ViewMode>),
}
```

`ContentModeContext` 只暴露 `ContentId` 和只读 `ContentQuery`。它没有
`ViewId`、selection 或 View mutation 能力。

`ViewModeContext` 暴露来源 `ViewId`、该 View 的只读 selections，以及
绑定 Content 的只读 query。它不借出可变 View 或 Content。

两种 Context 都只在一次调用期间有效。Mode 不能保存 Context 中的引用。

## 3. 实例 identity 与 effective binding

实例由集中表拥有：

```text
ContentModeInstances: (ModeId, ContentId) -> ContentModeInstance
ViewModeInstances:    (ModeId, ViewId)    -> ViewModeInstance
```

每个 Content 最多绑定一个 ContentMode。绑定后，所有引用该 Content 的
View 都解析到同一个实例，已有 ViewMode 会被移除，新 View 也不会建立
ViewMode。

没有 ContentMode 时，每个 View 可以独立绑定一个 ViewMode 或不绑定。
Vim 是 ViewMode，因此两个 View 可以分别处于 Normal 和 Insert。

Space 只负责布局。移动 View 不改变实例，关闭 View 只删除对应
ViewModeInstance；ContentModeInstance 跟随 Content 生命周期。

## 4. Action 边界

旧 `ContentCommand` 同时包含 selection 移动、文本修改、事务控制和保存，
不能继续作为 Content 的输入。目标类型分为：

```text
ViewAction        -> app 修改目标 View
ContentAction     -> Content 验证并修改领域数据
TransactionIntent -> TransactionManager 修改事务生命周期
AppAction         -> app 修改应用状态
```

`ViewAction` 包含移动、扩展、压缩 selections 和其他 View 会话操作。
`ContentAction` 使用已解析的文本范围，不依赖可变 `ContentViewState`。
motion 和 operator 的解析可以复用 core 纯算法，但解析结果必须在进入
Content 前固定。

保存、undo、redo 和 Content event 使用各自的 typed app 路径，不伪装成
ContentAction，也不要求存在 Mode。

## 5. 有序结果

Mode 返回有序 operation，而不是先修改 View 或 Content 再返回命令。

`ContentModeResult` 只能包含 ContentAction 和 TransactionIntent。
`ViewModeResult` 可以包含 ViewAction、ContentAction、TransactionIntent
和 AppAction。

执行器严格按顺序验证并应用 operation。ModeState 使用 provisional
snapshot：调用前复制状态；任一步验证或应用失败时，反向恢复本次已应用的
Content、View、TransactionManager 和 ModeState。成功后 snapshot 丢弃。

timeout 和普通 action 返回相同结果类型，因此它们走同一事务边界路径。

## 6. Content 与 View

`View` 只保存：

- `ContentId`；
- `ContentViewState`；
- View revision。

`ContentViewState` 只表达可复用的 View 会话能力，不镜像 `Content` 枚举；
View 不匹配 `Buffer`、`StatusBar` 等具体 Content 变体。具体 Content 创建
所需能力状态，`ContentStore` 在映射 ContentChange 时验证匹配关系。

它不保存 ModeInstance，也不代理 keymap、timeout、presentation 或 Mode
action。

Content 不声明默认 Mode，不接收 `ContentInput::View`，也不借入可变
`ContentViewState`。它负责查询数据、应用 ContentAction、产生
ContentChange、维护 text state identity，并生成或应用自己的事务数据。

app 收到 ContentChange 后，将 change 映射到所有绑定 View。来源 View 的
显式 ViewAction 与通用 change mapping 按 ModeResult 中声明的顺序执行。

## 7. 顶层路由

`Command`、`AppCommand`、`ModeCommand` 和目标解析属于 app。core 只保留
ContentAction、motion/target 纯算法和 Content 事务数据。

Dispatcher 可以记录输入来源 View，用于解析结果目标和事务 participant，
但不得把该 View identity 放入 ContentModeContext。

渲染只组合 View 与活动 ViewMode 的 cursor style 和 selection shape。
ContentMode 不提供 View presentation；绑定 ContentMode 的 View 使用中立
presentation。

## 8. 迁移顺序

1. 提升 Mode 定义并集中管理两类实例。
2. 删除 Content 默认 Mode，改由 bootstrap/session 建立绑定。
3. 拆分 ViewAction 与 ContentAction。
4. 将顶层命令和目标路由迁入 app。
5. 删除 `ContentInput::View` 和 Content 的 View 会话修改。
6. 接入有序 typed result 和 ModeState runtime rollback。
7. 在该边界上建立统一 TransactionManager。
