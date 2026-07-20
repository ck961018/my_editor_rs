# Editor Kernel Architecture

**状态：** 当前实现架构

**更新日期：** 2026-07-20

## 1. 文档定位

本文描述当前源码已经实现的所有权、执行事务、Mode 和渲染数据流。
未来方向记录在 `docs/roadmap/`，不作为当前代码契约。

编辑内核不依赖具体界面：

```text
编辑领域逻辑 + View 交互会话 + Scene 快照 + Frontend 抽象
```

TUI 是当前唯一生产 Frontend，但 `app` 不依赖 `tui`，`tui` 也不反向
依赖 `app`。

## 2. 分层与依赖方向

```text
frontend -> protocol
app      -> frontend + core + protocol
tui      -> frontend + terminal + protocol
main     -> app + tui + terminal
terminal -> protocol
core     -> protocol/std
protocol -> std
```

| 层 | 当前职责 |
| --- | --- |
| `protocol` | ID、几何、Scene、selection、按键、viewport 和查询数据 |
| `core` | Buffer、Content、领域 action、文本事务和通用输入算法 |
| `frontend` | 只定义 `Frontend` 行为接缝 |
| `app` | 主循环、命令执行、View/Mode、Scene、history 和后台任务 |
| `terminal` | crossterm 输入翻译、终端生命周期和 `Canvas` 输出 |
| `tui` | Taffy 布局、viewport 跟随、pull 查询和终端绘制 |
| `main` | 组装 Terminal、TUI Frontend 与 `App<TuiFrontend<_>>` |

`core` 不感知终端、布局、异步任务或渲染；`protocol` 不执行业务 IO；
具体前后端接线只在 `main.rs`。

## 3. 顶层所有权

```text
App<F: Frontend>
├── Kernel
│   ├── ContentStore
│   ├── ModeRegistry
│   ├── ModeContentStore
│   ├── TransactionManager
│   ├── mode jobs + save tasks
│   └── AppMessage channel
├── ClientSession
│   ├── Scene + SceneBuilder + scene revision
│   ├── HashMap<ViewId, View>
│   ├── ModeViewStore + Mode chains
│   ├── ContentId -> new View Mode profile
│   ├── Dispatcher + focused SpaceId
│   ├── FaceRegistry
│   └── PresentationLayerStore
└── F: Frontend
```

`Kernel` 保存可跨 session 共享的内容、Mode content state、history 和后台
任务。`ClientSession` 保存 Scene、View、Mode view state、输入状态和呈现
缓存。当前仍是一对一组合，没有 session registry 或并发共享容器。

启动由 `app::bootstrap` 分配 editor/status 的 `ContentId` 与初始 `ViewId`。
每个 `ClientSession` 持有唯一 `SceneBuilder`。TUI 的 `SceneRenderer` 按
`ViewId` 持有 viewport；后端 session 不保存终端滚动位置。

## 4. 身份与共享范围

```text
Scene:        SpaceId -> ViewId
View:         ViewId -> ContentId + ContentViewState
Content:      ContentId -> Content
Mode content: (ModeId, ContentId) -> ModeState
Mode view:    (ModeId, ViewId) -> ModeState
```

- `SpaceId` 是 Scene 布局节点；
- `ViewId` 是独立展示和交互会话；
- `ContentId` 是可被多个 View 引用的共享内容；
- `ModeId` 是一个 native 或 script Mode 定义。

同一 `ViewId` 不能挂载到多个 Scene leaf。同一 `ContentId` 可以由多个
View 展示；这些 View 拥有独立 selections、revision、viewport 和 Mode
view state，同时共享 Content 与 Mode content state。

## 5. Content 与 View

### 5.1 Content

`Content` 是静态闭合枚举：

```rust
enum Content {
    Buffer(Buffer),
    StatusBar(StatusBar),
}
```

`ContentStore` 是唯一 Content 表，每个 entry 保存 Content 与 Revision。
Content 自己分派具体变体的 presentation、view state、snapshot、query 和
dependency 规则；Store 只负责 ID、entry revision、生命周期和跨 Content
查询协调。app 不借出或识别 `Buffer`、`StatusBar`。Content 接收
`ContentAction`、保存请求和后台 `ContentEvent`，不接收顶层 `Command`、
原始按键或可变 View state。

文本编辑在 operation 到达执行点时，使用当时的 View selections 生成计划。
Content 验证并应用 `TextChangeSet`，返回规范 `ContentChange`；app 再把
change 映射到绑定同一 Content 的全部 View。

渲染只读数据通过 `ContentStore::query` 返回有界的 owned `ContentData`。
文本渲染只查询行范围或指定 offset；Mode 后台分析通过 `TextSnapshot`
读取稳定快照，不经过同步全文查询。`StatusBar` 显式声明目标 Content 的
`DocumentStatus` 依赖，Store 负责协调查询和有效 revision。Content 还声明
`ContentPresentation::{Text, StatusBar}`，`AppQuery` 据此组装
`ViewPresentation`，不通过 selection 是否存在猜测 Content 类型。

### 5.2 View

```text
View
├── ContentId
├── ContentViewState
└── Revision
```

文本 View 的 `ContentViewState` 保存 `Selections`；无状态 View 没有
selection。View 不保存 Mode instance、presentation layer 或 history。
Mode chain、输入状态和呈现缓存由 `ClientSession` 中的集中 store 管理。

## 6. Mode 模型

一个 `Mode` 定义同时拥有两种状态作用域：

```text
ModeContentStore: (ModeId, ContentId) -> shared content state
ModeViewStore:    (ModeId, ViewId)    -> independent view state
Mode chain:       ViewId             -> ordered ModeId[]
Mode profile:     ContentId          -> ordered ModeName[]
```

每个 View 可以附加多个有序 Mode。native 与 TypeScript Mode 实现同一个
`Mode` contract；app 不按实现类型分支。`ModeContentContext` 只提供目标
Content 和只读 query，`ModeViewContext` 额外提供目标 View 与 selections。
Context 不借出 `&mut Content`、`&mut View` 或宿主对象。

Mode action 返回有序 operation。action scope 决定允许的目标：content
scope 不能产生 View operation，view scope 可以作用于绑定 View 与 Content。
脚本 primitive 和 native Mode 都直接创建 `OperationRequest`。`ModeResult`
只携带有序的 typed operation，不保留第二套 effect algebra。

新 View 的 Mode profile 属于 `ClientSession`。动态 attachment 会同时更新
profile 和该 Content 的已有 View；split 与 replace 都从同一 profile 创建
chain。`Kernel` 不保存新 View 创建策略。

Mode state 的可变 callback 不直接发布持久状态。第一次写时，
`ModeDraftJournal` 用 `clone_box()` 建立 owned draft；同一 execution frame
后续 callback 读取最新 draft。frame 成功才提交，失败直接丢弃。被动
observer 失败只回滚该 callback draft 并暂存 attachment fault。

后台任务提取、后台结果安装和 input cancel 位于用户 frame 外，但同样使用
短生命周期 draft，并在各自受控生命周期边界一次提交。

## 7. Command 与 operation 执行

顶层 `Command`、`AppCommand`、`ModeCommand`、target 和 operation 类型在
app。core 只保留纯编辑算法、`ContentAction` 和 Content 事务数据。

```text
key / timeout / explicit command / script primitive
-> Dispatcher or adapter
-> OperationRequest
-> target resolver
-> ResolvedOperation queue
-> one executor
-> Content / View / TransactionManager / App
```

`OperationRequest` 和 `ResolvedOperation` 用 enum variant 绑定合法 target 与
operation，不能表达任意 target/operation 笛卡尔积。`OperationOrigin`
记录 app/content/view 来源，resolver 校验来源 capability、View/Content
绑定和 history owner。

nested Mode operation 以前插方式进入显式队列，保持深度优先顺序。
`ContentCommand::Sequence` 在 adapter 展开，但仍属于同一 frame。edit plan
在 operation 到达执行点时生成；短生命周期 `ViewEditPlan` 保留 selections
或 revision stale precondition。

## 8. ExecutionFrame 与 history

一次物理输入、timeout 或显式命令只有一个 `ExecutionFrame`：

```text
ExecutionFrame
├── CheckpointJournal
├── ModeDraftJournal
├── PreparedEffect[]
└── ExecutionBudget
```

Content、selections 和 input 在第一次变更前 lazy checkpoint。Mode state
写入 draft。history 继续由 `TransactionManager` 拥有，Kernel 只为本 frame
第一次 history 写入保存目标 flow checkpoint。

Save、Quit 和 frontend viewport mutation 在有序执行点捕获完整 payload，
但只在 frame 成功后发布。Save 携带当时的 `SaveSnapshot`，viewport 携带
Frontend 根据实际 pane 布局解析的 `ResolvedViewportCommand`。滚动结果保存
方向和行数；`zz`、`zt`、`zb` 等对齐结果保存目标 `top_row`，不移动 cursor。
后续 operation 失败会丢弃全部 prepared effects。

`ExecutionFrame` 不等于 undo/redo `HistoryTransaction`。如果 frame 开始前
活动 transaction 已包含 A，本次追加 B 后失败，只撤销 B；A 和 transaction
的打开状态保留。Mode state、viewport、focus 和布局不进入 history。

## 9. 输入架构

Terminal 把 crossterm 事件翻译为中立协议：

```rust
KeyEvent { code: KeyCode, modifiers: KeyModifiers }
```

每个 View 有有序 Mode chain，global keymap 位于 chain 之后。Dispatcher
逐层查询 Mode keymap、dynamic capture、timeout 和 typing fallback；Mode
可以 `Stop` 或 `Continue`。后续 Mode 能观察前序 operation 和 Mode draft。

`Keymap<A>` 是泛型 trie。固定序列支持 action 与更长 prefix 共存、最长完整
匹配、timeout 和 replay。每个 `(ModeId, ViewId)` 的动态输入状态独立；
`InputCoordinator` 统一选择 pending sequence 与 dynamic context 的 deadline。

App 在 `tokio::select!` 中等待 Frontend event、最近输入 deadline、后台
`AppMessage` 和取消信号。replay 使用显式队列，并继续归属当前 frame 的
统一 replay 预算。

## 10. Scene、布局与 pull 渲染

`protocol::scene` 只保存 Scene 快照和只读访问。split、close、replace、树
修复和 ID 分配属于 `app::scene_model::SceneBuilder`。布局由 TUI 的
`TaffyEngine` 负责，并按 scene revision 缓存 resolved scene。

呈现刷新与绘制分离：

```text
controlled app phase
-> Mode content_decorations / view_policy / view_decorations
-> PresentationLayerStore

Frontend::render
-> AppQuery
-> ContentStore + View + PresentationLayerStore + FaceRegistry
-> RenderQuery visible-range pull
-> SceneRenderer paint
```

共享 content layer 按 `(ModeId, ContentId)` 保存；独立 view layer 按
`(ModeId, ViewId)` 保存。layer 同时记录 source content/view revision 和
Mode content/view state revision。刷新只重算 revision signature 已变化或
新出现的 key，并淘汰已移除的 chain/View；Mode callback 接收实际文档行数
界定的有限范围。stale 或 faulted layer 不参与组合。policy 按 Mode 高到低
取第一个显式值；decoration 按低到高组合，使高优先级后绘制。

`AppQuery` 和 renderer 不持有 Mode store，也不调用 Mode、V8、worker 或
plugin runtime。TUI 继续只 pull 可见文本行与 visible-row decorations，
没有引入全 frame push snapshot。

## 11. 保存与后台任务

Buffer 维护 current/saved `TextStateId`。保存 operation 在其有序位置捕获
path、bytes、revision 和 state，成功 frame 才启动临时文件加 rename 的
原子 IO。

保存完成带回原 revision/state。只有完成结果仍对应当前状态时才清除
modified；在途保存期间的新请求保留最新快照。关闭时取消普通任务，但等待
critical 保存任务完成。

Mode 后台 job 只接收 owned snapshot/request。worker result 通过 message 回到
主循环，并校验 job slot/version 后安装；异步任务不能直接修改宿主状态。

## 12. 前后端与远程语义边界

同进程 Frontend 异步产生 `FrontendEvent`，并同步执行：

```rust
render(&Scene, Revision, &dyn RenderQuery, focused)
```

协议层已有带 `RequestId`、revision 和结构化错误的 owned 远程语义消息。
`app::remote` 可以把本地 `AppQuery` 适配为 response，但当前没有 serde、
网络 transport、连接管理或远程 Frontend 事件循环。

## 13. 当前不变量

- Content 共享状态与 View 会话状态分离；
- ContentStore 是唯一 Content 表；
- 一个 View 可以附加多个有序 Mode；
- Mode content state 按 Content 共享，view state 按 View 隔离；
- Mode 只能直接修改自己的 draft，宿主 mutation 进入 typed operation；
- 一次输入或命令只有一个 `ExecutionFrame`；
- `TransactionManager` 是 history 生命周期的唯一所有者；
- Buffer 不保存 View selections、history stack 或 history cursor；
- SpaceId、ViewId、ContentId 不互相替代；
- SceneBuilder 属于 app，布局和 viewport 属于 TUI；
- 渲染使用 pull query，render path 不调用 Mode；
- 异步结果必须通过 revision/version 校验后安装；
- native 和 script Mode 共享注册、执行和生命周期模型。

## 14. 当前有意保留的边界

- Content 继续使用静态 enum；
- `App` 使用泛型 `F: Frontend`，不引入 app 层前端枚举或 trait object；
- 当前只有单 Frontend、单 `ClientSession`；
- Mode state v1 使用 `clone_box()` draft，不承诺零复制；
- Mode callback 只使用 content/view state 均显式可见的 canonical contract；
- Presentation 只包含现有 policy 与 decorations；
- 不提前实现 Plugin API v2、热重载、capability 或 crate 拆分。
