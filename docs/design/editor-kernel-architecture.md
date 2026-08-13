# Editor Kernel Architecture

**状态：** 当前实现架构

**更新日期：** 2026-08-10

## 1. 文档定位

本文描述当前源码已经实现的 crate 边界、所有权、执行事务、Mode 和渲染
数据流。本文只记录当前实现，不混入未来演进计划。

编辑内核不依赖具体界面：

```text
编辑领域逻辑 + View 交互会话 + Scene 快照 + Frontend 抽象
```

TUI 是当前唯一生产 Frontend，但 `app` 不依赖 `tui`，`tui` 也不反向
依赖 `app`。

## 2. 分层与依赖方向

```text
vell-frontend  -> vell-protocol
vell-core      -> vell-protocol
vell-mode      -> vell-core + vell-protocol
vell-plugin-v8 -> vell-mode + vell-core + vell-protocol
vell-app       -> vell-frontend + vell-mode + vell-core + vell-protocol
vell-tui       -> vell-frontend + vell-protocol
vell binary    -> vell-app + vell-plugin-v8 + vell-tui
```

| 层 | 当前职责 |
| --- | --- |
| `vell-protocol` | ID、Scene、输入、viewport、查询与远程语义数据 |
| `vell-core` | Content、Buffer、领域 action、文本事务和输入算法 |
| `vell-mode` | Mode、adapter、state、命令、operation 和 presentation |
| `vell-frontend` | 只定义 `Frontend` 行为接缝 |
| `vell-app` | 主循环、执行、View、Scene、history、保存和后台任务 |
| `vell-plugin-v8` | TypeScript/V8 宿主与通用 Mode adapter |
| `vell-tui` | 终端 IO、Taffy、viewport、pull 查询和绘制 |
| 根二进制 | 加载脚本 Mode，组装 TUI 与 `App<TuiFrontend<_>>` |

`vell-core` 不感知 Mode、终端、布局、异步任务或渲染；
`vell-protocol` 不执行业务 IO；`vell-app` 的普通依赖不含 V8 或 TUI。
具体 V8、App 与 TUI 接线只在 `src/main.rs`。

## 3. 顶层所有权

```text
App<F: Frontend>
├── Kernel
│   ├── ContentStore
│   ├── ModeRegistry
│   ├── ContentClassifier
│   ├── ModeContentStore
│   ├── TransactionManager
│   ├── mode jobs + save tasks
│   └── AppMessage channel
├── ClientSession
│   ├── ViewWorkspace
│   │   ├── Scene + SceneBuilder + scene revision
│   │   ├── HashMap<ViewId, View> + semantic tree
│   │   ├── ViewPaneMap + status structure
│   │   └── ViewId allocation + focused SpaceId
│   ├── ModeViewStore + Mode chains
│   ├── ModeResolver + attachment overrides
│   ├── Dispatcher
│   ├── FaceRegistry
│   └── PresentationLayerStore
└── F: Frontend
```

`Kernel` 保存可跨 session 共享的内容、View definition、Mode content
state、history 和后台任务。`ViewWorkspace` 保存 View 与 Scene 的完整结构状态；
`ClientSession` 组合 workspace、Mode view state、输入状态和呈现缓存。
当前仍是一对一组合，没有 session registry 或并发共享容器。

根二进制先加载内建与用户 TypeScript Mode，再由 `vell-app::bootstrap`
分配 editor 的 `ContentId` 与初始 `ViewId`。
每个 `ViewWorkspace` 持有唯一 `SceneBuilder`。TUI 的 `SceneRenderer` 按
`ViewId` 持有 viewport；后端 session 不保存终端滚动位置。

## 4. 身份与共享范围

```text
Scene:        SpaceId -> ViewId
View pane:    (ViewId, SpaceId) -> PaneKey
View:         ViewId -> ViewDefinitionId + BindingKey -> ContentId
View def:     ViewDefinitionId -> binding schema
Document:     ViewId -> optional ContentViewState
Content:      ContentId -> Content
Mode content: (ModeId, ContentId) -> ModeState
Mode view:    (ModeId, ViewId) -> ModeState
```

- `SpaceId` 是 Scene 布局节点；
- `ViewId` 是独立展示和交互会话；
- `ContentId` 是可被多个 View 引用的共享内容；
- `ModeId` 是一个 native 或 script Mode 定义。

同一 `ViewId` 可以挂载到多个 Scene leaf：一个 view 的正文与
gutter/bar 等直属 Pane 各占一个 Content Space，view 端的
`ViewPaneMap` 维护 SpaceId 与 PaneKey 的映射，渲染查询携带来源
SpaceId。同一 `ContentId` 可以由多个 View 通过不同角色引用；这些 View
拥有独立 revision、viewport 和 Mode view state，同时共享 Content 与
Mode content state。BufferView 的 `document` binding 还拥有独立
selections。View 另有 binding revision；只有 bindings 实际变化时递增，
用于拒绝过期的 Mode attachment plan。

## 5. Content 与 View

### 5.1 Content

`Content` 是静态闭合枚举：

```rust
enum Content {
    Buffer(Buffer),
}

enum ContentKind {
    Buffer,
}
```

`ContentKind` 是与 `Content` 一一对应的封闭判别值，由
`Content::kind()` 穷尽映射。它不是插件字符串或动态 registry
key；新增 Content 时必须同步处理所有静态分派位置。
状态栏不是 Content：它是 editor view 的直属 Pane（见 5.2）。

`ContentStore` 是唯一 Content 表，每个 entry 保存 Content 与 Revision。
Content 自己分派具体变体的 view state、snapshot 和 query；Store 只负责
ID、entry revision 与生命周期。app 不借出或识别 `Buffer`。
Content 接收
`ContentAction`、保存请求和后台 `ContentEvent`，不接收顶层 `Command`、
原始按键或可变 View state。

文本编辑在 operation 到达执行点时，使用当时的 View selections 生成计划。
Content 验证并应用 `TextChangeSet`，返回规范 `ContentChange`；app 再把
change 映射到绑定同一 Content 的全部 View。

渲染只读数据通过 `ContentStore::query` 返回有界的 owned `ContentData`。
文本渲染只查询行范围或指定 offset；Mode 后台分析通过 `TextSnapshot`
读取稳定快照，不经过同步全文查询。Buffer 的资源名、资源路径、载体状态、
脏状态、保存结果和文本统计分别通过独立 query 暴露，不存在聚合状态结构。
Content 还声明 `ContentKind`；`AppQuery` 穷尽匹配
`(ContentKind, ContentViewState)` 组装 `ViewPresentation`。
`RenderQuery` 的 content、view 和
decoration 查询都返回 `Result`；缺失 ID、不支持的查询、错误的
数据变体和种类错配统一返回 `RenderQueryError`。渲染路径不
通过 selection 是否存在猜测 Content 类型，也不因查询契约错误
而 panic。

### 5.2 View

```text
View
├── ViewDefinitionId
├── ContentBindings (BindingKey -> ContentId)
├── optional document ContentViewState
├── Revision
├── ViewPaneMap (SpaceId <-> PaneKey)
├── switchable
└── parent / children (语义树)
```

View definition 由 Kernel 的 `ViewDefinitionRegistry` 唯一持有，View
实例只保存它的稳定 ID 和已经校验的 bindings。同 ID 的幂等注册允许完全
相同的 schema，冲突 schema 会被拒绝。BufferView 使用
`core.buffer` definition 与保留的 `document` binding；它的
`ContentViewState` 是与封闭 Content 对齐的显式枚举，因此始终有
`BufferViewState { selections }`。没有 `document` binding 的复合 View
不伪造 ContentViewState。
View 是完整的展示单元：它可以控制多个直属 Pane（正文 `body`、原生行号
`builtin.gutter`、状态栏 `builtin.status` 等），`ViewPaneMap` 维护
SpaceId 与 PaneKey
的双向映射；渲染与事件查询携带来源 SpaceId，由 view 决定该 Pane
的 presentation。View 不保存 Mode instance、presentation layer 或
history。Mode chain、输入状态和呈现缓存由 `ClientSession` 中的
集中 store 管理。

`ViewBindingOperation::Rebind` 只改变一个已声明 binding，保留 ViewId、
definition、其他 bindings、Pane 和语义树。`document` rebind 会重建匹配
新 Content 的 view state 和 Mode attachment；`view.switch` 则替换完整
View。关闭 Content 时，`ViewWorkspace` 先把匹配的 document View 提升到
最近的 switchable 生命周期 owner，去重将删除的语义子树，再拒绝所有仍会
存活的 binding 引用；`force` 不绕过引用保护。

`switchable` 是实例属性：通用 View 切换沿焦点 Pane 所属 view 的
语义 `parent` 链向上，第一个 switchable 的 view 即切换目标。语义
父子（复合 view 的组合关系）与 Space 布局父子是不同维度。

Native `core.diff` 验证了复合 View 的语义根可以没有直属 Pane。它持有
`left/right` bindings，两个不可切换的子 BufferView 分别拥有实际 body
Pane。生命周期 owner 与 Scene 替换锚点因此是两个概念：前者是 DiffView，
后者优先选择当前焦点子 Pane，否则选择第一个子 body Pane。整体切换和关闭
删除完整语义子树，但复用该叶 Pane 安装替代 View，不增加协调 Pane。

脚本复合 View 与 Native `core.diff` 共用同一个声明式 recipe 和替换路径。
recipe 只保存父 binding schema、两个文档子 View 的 binding 映射与分割方向；
Kernel registry 负责跨 definition 校验，ClientSession 在完整 workspace
candidate 上预校验全部 Mode attachment，`ViewWorkspace` 仍独占 ID、Scene、
Pane 与原子发布。脚本不接收布局树或可变 View factory。
recipe 创建校验要求父 View 初始不产生 Pane；运行时父子一致性校验允许
View extension 为父 View 增加直属 Pane，因此 extension 不会改变 binding
operation 或生命周期语义。
Diff 替换在进入 `ExecutionFrame` 的 prepared effects 前生成完整 workspace
candidate 并预校验全部 attachment。发布时先接续新 attachment，再清理旧
View，避免共享 Mode content state 和 Face remap 因引用短暂归零而重建。

`core.buffer` 的 gutter 是 `ViewWorkspace` 管理的默认 pane recipe，不是
插件 extension、Content、Mode 或独立 View。每个 BufferView 恰有一个不可
聚焦的 `builtin.gutter` 和一个 body；split、switch、close、Diff child 与
per-pane status 都把完整 recipe 当成一个布局槽位。session 级
`EditorOptions` 决定 gutter 的启动宽度；隐藏时仍保留 Space，只使用
`Fixed(0)`。

`AppQuery` 从该 BufferView 的 primary selection 和 Content 文本统计组装
owned `LineNumberPresentation`。当前逻辑行显示一基绝对行号，其他可见行显示
相对距离。TUI 与 body 共用同一 ViewId viewport，只绘制 presentation，不进入
Mode、V8 或 Worker。

复合 View 的具名 binding operation 通过 recipe 找到对应子 binding，并在一个
`ViewWorkspace` draft 中同时改变父 binding 和子 BufferView 的 `document`
binding。Scene 与父子 ViewId 保持不变；子 View 的 ContentViewState 按新
Content 重建，已有 Mode view state 保留，Mode content state 引用从旧
Content 迁移到新 Content。`diff.setRightContent` 只是该通用路径的内建命令
入口，因此 Native `core.diff` 和声明同一 `right` 角色的脚本 View 语义一致。

状态栏不是 Content（ADR 0001），也不是独立 View：状态栏 Space 直接
引用其服务的 editor view，并在该 view 的 `ViewPaneMap` 中登记为
`builtin.status` Pane。`ViewWorkspace` 支持两种布局策略：全局策略
只有一个状态栏 Space，焦点变化时把它移交给新的焦点 editor view
（改写 Space 的 view 引用）；per-pane 策略为每个 Buffer View 维护
独立状态栏 Space。状态栏可通过把对应 Space 高度设为零独立隐藏。
app 可按 View 查询单个状态栏，也可按 Content 查询全部对应
状态栏。状态栏最终呈现是带 Face 的左、中、右分段；呈现数据
来自该 view 自身的 `viewPolicy.statusBar`，可替换默认呈现，TUI
只负责布局与绘制。

## 6. Mode 模型

一个 `Mode` 定义同时拥有两种状态作用域：

```text
ModeContentStore: (ModeId, ContentId) -> shared content state
ModeViewStore:    (ModeId, ViewId)    -> independent view state
Mode chain:       ViewId             -> ordered ModeId[]
Attachment rule:  ModeId             -> View + optional binding/languages
```

每个 View 可以附加多个有序 Mode。native 与 TypeScript Mode 实现同一个
`Mode` contract；app 不按实现类型分支。每个定义通过 `ModeAdapters`
提供 Buffer 支持 slot（封闭表只含 Buffer）。registry 在注册时冻结
这张封闭 support table，并可按 `(ModeId, ContentKind)` 查询绑定了
Mode definition 的 adapter。runtime callback 只从已注册 adapter
进入。

`ModeResolver` 对 `before` 约束做稳定拓扑排序。前向引用有效；无约束的
Mode 保持注册顺序。目标不存在或约束成环时返回结构化错误。具体 View 的
chain 还要根据 attachment rule、binding 对应 Content 的分类、adapter 支持
以及 Content/View override 筛选。View 顺序覆盖在筛选后作为部分优先列表
应用，不启用已被筛掉的 Mode；未列出的 Mode 仍保持稳定拓扑顺序。

`ModeContentContext` 和 `ModeViewContext` 都是按 `ContentKind` 封闭的
enum。Buffer variant 仅提供强类型文本查询、细粒度资源事实、snapshot
和 selections。不支持的能力不会出现在对应的强类型
context 上。Context 不借出 `&mut Content`、`&mut View` 或宿主对象。
TypeScript 通过 `on.buffer` 映射 Content-bound context，通过 `on.view`
映射没有 Content identity 的 View context，不建立另一套 Mode runtime。
状态栏呈现不再经过 Mode adapter：插件通过
buffer mode 的 `viewState.viewPolicy.statusBar` 提供，由 app 在
render query 层组装。

Mode action 返回有序 operation。action scope 决定允许的目标：content
scope 不能产生 View operation，view scope 可以作用于绑定 View 与 Content。
脚本 primitive 和 native Mode 都直接创建 `OperationRequest`。`ModeResult`
只携带有序的 typed operation，不保留第二套 effect algebra。

`Kernel` 的 `ContentClassifier` 统一产生 Content 分类；`ClientSession` 的
`ModeResolver` 为每个具体 View 生成带 binding revision 的有序 attachment
plan。split、replace 和 rebind 都先为候选 View 解析计划，再与 View 结构一起
发布。安装器按 diff 保留、添加、删除并重排 chain；失败不创建部分 state，
过期计划不改动任何 Mode state。content state 按 `(ModeId, ContentId)` 引用
计数共享，保留 attachment 的 view state 不会因重新解析而重建。

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

Save、Quit、split、focus 和 frontend viewport mutation 在有序执行点捕获
完整 payload，但只在 frame 成功后发布。Save 携带当时的 `SaveSnapshot`，
viewport 携带
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

`protocol::scene` 只保存 Scene 快照和只读访问。`ViewWorkspace` 使用
`app::scene_model::SceneBuilder` 生成快照，并统一拥有 split、close、
replace、View 子树修复、焦点修复和 ID 分配。结构操作先在 workspace
副本中完成；Scene、View tree 与 Pane map 全部校验成功后才一次发布。
布局由 TUI 的 `TaffyEngine` 负责，并按 scene revision 缓存 resolved
scene。

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

Mode 后台 job 只接收 owned snapshot/request。job result 通过 message 回到
主循环，并校验 job slot/version 后安装；异步任务不能直接修改宿主状态。

Script Worker 是独立的平台能力，不占 Mode job slot。`vell-plugin-v8` 在主
事件循环的 worker poll tick 泵送消息，并通过 revision-safe sink 发布结果。

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
- `Content`、`ContentKind` 和 `ContentViewState` 保持封闭且一一对应；
- ContentStore 是唯一 Content 表；
- 一个 View 可以附加多个有序 Mode；
- Mode content state 按 Content 共享，view state 按 View 隔离；
- Mode 只能直接修改自己的 draft，宿主 mutation 进入 typed operation；
- 一次输入或命令只有一个 `ExecutionFrame`；
- `TransactionManager` 是 history 生命周期的唯一所有者；
- Buffer 不保存 View selections、history stack 或 history cursor；
- SpaceId、ViewId、ContentId 不互相替代；
- View 子树、Pane、Scene、焦点和结构 ID 只由 ViewWorkspace 修改；
- SceneBuilder 封装在 ViewWorkspace，布局和 viewport 属于 TUI；
- 渲染使用 pull query，render path 不调用 Mode；
- 异步结果必须通过 revision/version 校验后安装；
- native 和 script Mode 共享注册、执行和生命周期模型。

## 14. 当前有意保留的边界

- Content 继续使用静态 enum；
- `App` 使用泛型 `F: Frontend`，不引入 app 层前端枚举或 trait object；
- 当前只有单 Frontend、单 `ClientSession`；
- Mode state 使用 `clone_box()` draft，不承诺零复制；
- Mode callback 只使用 content/view state 均显式可见的 canonical contract；
- Presentation 只包含现有 policy 与 decorations；
- 远程协议只有 owned 语义消息，尚无 transport 与连接管理；
- 插件不拥有 Content，热重载、包管理和通用 capability 尚未实现。
