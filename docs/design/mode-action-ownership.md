# Mode 与 Action 所有权

**状态：** 当前实现

**更新日期：** 2026-07-22

## 1. 核心规则

Mode 解释输入、维护扩展状态并编排 operation；Content 保存编辑领域数据；
View 保存与一个 Content 绑定的会话状态；app 解析目标并原子执行结果。

当前实现只有一个统一 `Mode` contract，不再区分互斥的 `ContentMode` 与
`ViewMode`。每个 Mode 可以为多个封闭 ContentKind 提供 adapter。

## 2. 定义与 adapter

`ModeRegistry` 为定义分配 `ModeId`，并冻结以下静态信息：

- owned `ModeName` 与限定 command 名；
- 支持的 `ModeAdapters`；
- keymap、输入与 callback；
- attachment ordering 约束；
- named Face 默认值。

`ModeAdapters` 当前可包含 Buffer 和 StatusBar slot。native Mode 可以直接
实现 `Mode`，也可以通过 `TypedMode` 与 `ErasedMode` 在静态状态类型和
运行时类型擦除之间建立单一 adapter。TypeScript Mode 使用相同的 erased
contract。

不支持当前 `ContentKind` 的 Mode 不能附加；不匹配的 context 或 state 类型
返回结构化错误。

## 3. 状态与 identity

```text
ModeContentStore: (ModeId, ContentId) -> Mode content state
ModeViewStore:    (ModeId, ViewId)    -> Mode view state
Mode chain:       ViewId             -> ordered ModeId[]
Mode profile:     ContentId          -> ordered ModeName[]
```

Mode content state 在同一 Content 的所有 View 间共享。Mode view state、
key sequence、capture 和 timeout 按 View 隔离。

`View` 自身只持有：

- `ContentId`；
- 与 ContentKind 对齐的 `ContentViewState`；
- View revision。

View 不持有 Mode instance、history、viewport 或 presentation layer。
Space 只标识 Scene 布局节点，也不拥有 View 或 Mode 状态。

## 4. Context 能力

`ModeContentContext` 和 `ModeViewContext` 都按 ContentKind 封闭：

- Buffer content context 可以查询文档状态、稳定文本快照和范围；
- Buffer view context 还包含目标 View 的 selections；
- StatusBar context 只提供状态栏相关 owned 数据；
- 不合法的 cursor、edit 或 analysis 能力不会出现在对应 adapter 上。

Context 只在 callback 期间有效，不借出 `&mut Content`、`&mut View`、
`ContentStore` 或 App。脚本 context 中的 native function 在 callback
结束后必须拒绝调用。

## 5. Action 与 operation

Mode action 返回 `ModeResult`，其中包含 flow 决策和有序
`OperationRequest`。

content scope 可以：

- 产生绑定 Content 的 `ContentAction`；
- 请求 history 或保存；
- 调用 content-scoped Mode command；
- 产生仍需 app 验证的无目标应用操作。

view scope 还可以：

- 产生绑定 View 的 `ViewAction` 或 selection-relative `EditCommand`；
- 请求 viewport；
- 调用 view-scoped Mode command。

所有目标在 app 中结合 operation origin 解析。Mode 不能直接修改其他 Mode
state，也不能保存另一个 Mode 的实例引用；跨 Mode 调用使用限定命令名并进入
同一个 execution frame。

## 6. Draft 与 fault isolation

第一次写入 Mode state 时，`ModeDraftJournal` 通过 `clone_box()` 创建
owned draft。同一 frame 内的后续 callback 读取最新 draft：

- frame 成功时，content 和 view draft 分别提交到持久 store；
- frame 失败时，普通 draft 被丢弃；
- 主动 callback fault 使当前 frame 失败；
- 被动 presentation、content-changed 或 background callback fault 只暂停对应
  attachment，不阻止基础文本编辑；
- fault、state 与 presentation revision 用于决定后续刷新和诊断。

Mode state 不属于 undo/redo history。TypeScript 模块全局与 V8 heap 状态也不
参与宿主 rollback。

## 7. Attachment 与排序

每个 Content 的新 View profile 由 `ClientSession` 管理。初始 profile 按
ContentKind 对 Mode 的 `before` 约束执行稳定拓扑排序：

- 前向引用有效；
- 无约束 Mode 保持注册顺序；
- 目标不支持当前 ContentKind 时，该边不进入这条 chain；
- 缺失目标和同一 ContentKind 的环返回结构化启动错误。

动态 attachment 先验证 Content、adapter 和所有已有 View 的
ContentKind/ViewState 配对，再一次性更新 profile、chain、state 与 Face。
失败时不留下部分 attachment。

## 8. Presentation 与后台任务

Mode 可以贡献：

- content decoration layer；
- view decoration layer；
- cursor、selection shape 与 face policy；
- named Face 默认值。

这些数据先发布到 Rust 的 presentation cache。render path 只读取缓存，不调用
Mode、V8 或 worker。

后台任务由 Mode 提供 owned request，`Kernel` 按
`(ModeId, ContentId, ModeJobSlot)` 管理运行版本与取消。结果返回主循环后，
只有 slot、version 和输入仍有效时才能通过短生命周期 draft 安装。
