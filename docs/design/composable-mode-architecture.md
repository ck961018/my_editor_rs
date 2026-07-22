# 可组合 Mode 架构

**状态：** 当前实现

**更新日期：** 2026-07-22

## 1. 文档定位

本文描述 `vell-mode` 与 `vell-app` 已实现的可组合 Mode 运行模型。
所有权摘要见
[`editor-kernel-architecture.md`](editor-kernel-architecture.md)，
operation 边界见
[`command-execution-ownership.md`](command-execution-ownership.md)。

Mode 是编辑器扩展行为的统一契约。native adapter 与 TypeScript adapter
共享同一 registry、state store、输入链、operation 队列、后台任务与
presentation cache。Rust 内核不按 Vim、Tree-sitter 或语言名称分支。

## 2. 模型概览

```text
ModeRegistry
├── ModeId -> Mode definition
├── ContentKind -> frozen adapter support
└── qualified command names

Kernel
├── ModeContentStore: (ModeId, ContentId) -> state
└── background job slots

ClientSession
├── ModeViewStore: (ModeId, ViewId) -> state
├── ViewId -> ordered ModeId[]
├── ContentId -> new-View profile
├── per-View input state
├── FaceRegistry
└── PresentationLayerStore
```

一个 Mode 定义可以同时提供 content state、view state、command、input、
content-change observer、presentation 和 background job。它也可以为 Buffer
和 StatusBar 中的一种或多种 ContentKind 提供 adapter。

## 3. 统一 contract

`Mode` 是运行时擦除后的 contract。它的状态实现 `ModeState`，并通过
`clone_box()` 参与 draft。native 扩展可实现 `TypedMode`，再由
`ErasedMode` 集中完成状态与后台输出的类型检查。

统一 contract 保证：

- Mode definition 是静态注册对象；
- content state 和 view state 的作用域固定；
- callback 只拿到短生命周期、只读宿主 context；
- callback mutation 只发生在自身 state draft；
- 宿主 mutation 表达为有序 `OperationRequest`；
- ContentKind 不支持的能力在 adapter 层不可达；
- runtime type mismatch 产生 `ModeError`，不 panic。

TypeScript 的 `ScriptMode` 是这一 contract 的 adapter，不是第二套 Mode
系统。

## 4. ContentKind adapter

当前封闭 ContentKind 为 Buffer 与 StatusBar。每个 adapter 决定：

- content state factory；
- view state factory；
- 可调用 command；
- keymap 和 raw input；
- content change callback；
- content/view presentation；
- 可选后台分析。

`ModeContentContext` 和 `ModeViewContext` 也是封闭 enum。Buffer context
可以读取文本快照和文档状态，Buffer view context 还能读取 selections；
StatusBar context 只包含状态栏数据。Context 不借出可变 Content、View、
ContentStore 或 App。

## 5. Chain 与 profile

每个 View 有一条有序 Mode chain。每个 Content 有一个新 View profile；
split 和 replace 根据该 profile 建立新 chain。

启动时，session 对每种 ContentKind 单独解析 `before` 约束：

- 排序是稳定拓扑排序；
- 前向引用合法；
- 无约束的 Mode 保持注册顺序；
- 不支持当前 ContentKind 的目标不形成排序边；
- 目标缺失或同一 ContentKind 内成环时，启动失败并返回结构化错误。

动态 attachment 先验证目标 Content、adapter 和该 Content 的全部现有 View，
再提交 profile、state、chain 与 Face。验证失败不留下部分状态。

## 6. 输入处理

Terminal 先把输入翻译为
`KeyEvent { code, modifiers }`。Dispatcher 对 focused View 执行：

```text
Mode chain, high priority first
-> each Mode keymap / dynamic capture / timeout / raw input
-> optional global keymap fallback
-> typing fallback
```

每个 `(ModeId, ViewId)` 有独立的 fixed sequence、capture 和 timeout 状态。
`InputCoordinator` 统一管理 pending sequence 与 dynamic input 的 deadline。

Mode 返回 `Stop` 或 `Continue`。前一 Mode 返回的 operation 先在当前
execution frame 中执行；返回 `Continue` 时，后一 Mode 可以观察更新后的
provisional Content、View 与 Mode draft。

replay 使用显式输入队列，仍属于当前 frame，并受独立 replay 预算限制。

## 7. Command 与组合

Mode command 使用稳定限定名，例如 `pairs.quote`。跨 Mode 调用产生
`ModeCommand` operation，callback 退出后由 app 深度优先执行。

Mode 不保存其他 Mode 的指针，也不直接写其他 Mode state。限定命令由 registry
解析，并验证被调用 Mode 当前确实附加在目标 Content 或 View 上。

ModeResult 只包含 flow 与有序 typed operation，不再存在并行的字符串 effect
algebra。operation 的目标和 capability 由来源 scope 限制。

## 8. State draft 与 frame

一次输入、timeout 或显式命令使用一个 `ExecutionFrame`。Mode state 第一次
写入时创建 draft：

```text
persistent content/view state
-> clone_box()
-> ModeDraftJournal
-> later callbacks read current draft
-> frame success: commit
-> frame failure: discard
```

同一 frame 中的嵌套命令共享 draft 和 operation budget。Mode state revision
只在实际提交发生变化时递增。

Mode state 不进入文本 undo/redo。JavaScript module global 和闭包状态也不参与
宿主 rollback。

## 9. Content 变化

Content mutation 产生规范 `ContentChange`。app 先把 change 映射到所有绑定
View 的 `ContentViewState`，再通知相关 Mode attachment。

content-change callback 是被动 observer：

- 可以更新自身 content state；
- 可以使 presentation 或后台 analysis 变脏；
- 不能产生宿主 operation；
- 失败只 fault 对应 attachment，不回滚已经成功的基础文本编辑。

这条限制避免隐式递归编辑和 observer 之间的不可预测事务顺序。

## 10. Presentation

Mode 不在 render 时运行。受控 app phase 根据 revision signature 刷新：

```text
Mode callbacks
-> ContentPresentationLayer / ViewPresentationLayer
-> PresentationLayerStore
-> AppQuery
-> fallible RenderQuery
-> SceneRenderer
```

content layer 按 `(ModeId, ContentId)` 共享，view layer 按
`(ModeId, ViewId)` 隔离。每层记录来源 Content、View 和 Mode state
revision。

组合规则：

- view policy 按 Mode 高到低选择第一个显式字段；
- decoration 按低到高组合，使高优先级 Mode 后绘制；
- stale、faulted 或已卸载 layer 不参与组合；
- visible decoration 在 app query 中按行范围裁剪；
- renderer 不持有 Mode store，也不调用 V8 或 worker。

## 11. Face

Mode 可以注册 named Face 默认值。首次 provider 生效；后续重复定义记录
`FaceConflict`，供诊断查询，不静默覆盖现有 provider。主题或宿主显式设置
Face 时，可以成为最终解析值。

Mode 的 selection policy 和 decoration 只引用 FaceName，不复制主题值。

## 12. 后台任务

Mode content state 可以产生带稳定 `ModeJobSlot` 和单调 version 的 owned
`ModeJobRequest`。Kernel 按 `(ModeId, ContentId, slot)` 管理：

- 同 slot、同 version 不重复启动；
- 新 version 取消运行中的旧任务，并只保留最新 queued request；
- worker 只接收 owned 数据和 cancellation token；
- 结果通过 AppMessage 回到主循环；
- slot/version 失配或输入过期的结果被丢弃；
- 有效结果在短生命周期 Mode draft 中应用并一次提交。

后台任务不能直接修改 Content、View、Scene 或 Frontend。

## 13. Fault isolation

主动输入或 command callback 失败时，当前 frame 失败并回滚宿主 provisional
状态。Mode fault 作为结构化诊断提交，使事件循环可以继续。

被动 content-change、presentation、state factory 或 background callback
失败时，只暂停对应 attachment。基础编辑、其他 Mode 和 native fallback
继续工作。

诊断保留 Mode、phase、category、callback 与 message；presentation 诊断还可
报告 policy 来源、decoration 数量和 Face 冲突。

## 14. 不变量

- 一个 View 可以附加多个有序 Mode；
- content state 按 Content 共享，view/input state 按 View 隔离；
- Mode 不拥有 Content、View、history、Scene 或 Frontend；
- Mode 只能直接修改自己的 draft；
- 所有宿主 mutation 进入 typed operation 执行器；
- callback flow 与 operation 顺序显式；
- 被动 observer 不产生宿主 mutation；
- render path 不执行扩展代码；
- background result 必须经过 slot、version 和 revision 校验；
- native 与 script Mode 使用同一生命周期和错误模型。
