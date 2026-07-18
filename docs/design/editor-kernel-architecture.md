# Editor Kernel Architecture

**状态：** 当前实现架构
**更新日期：** 2026-07-17

## 1. 文档定位

本文描述当前源码已经实现的架构、所有权和运行时数据流。尚未实现的脚本运行时、远程
transport、多客户端调度、完整 Vim 语法和增量布局等方向统一记录在
`docs/roadmap/editor-evolution-roadmap.md`，不作为当前代码契约。

项目的核心目标是让编辑内核不依赖某一种界面实现：

```text
编辑领域逻辑 + View 交互会话 + Scene 快照 + Frontend 抽象
```

TUI 是目前唯一的生产 Frontend，但 `app` 不依赖 `tui`，TUI 也不反向依赖 App。

## 2. 分层与依赖方向

```text
frontend -> protocol
app      -> frontend + core + protocol
tui      -> frontend + terminal + protocol
main     -> app + tui + terminal
terminal -> protocol
core     -> protocol + std
protocol -> std
```

各层职责如下：

| 层 | 当前职责 |
| --- | --- |
| `protocol` | ID、几何、Scene 快照、selection、按键、viewport、查询数据和远程语义消息 |
| `core` | Buffer、Content、ContentStore、领域 action、文本事务数据和输入算法 |
| `frontend` | 只定义 `Frontend` 行为接缝 |
| `app` | 主循环、命令路由、View/Session/Kernel 所有权、Scene 修改和后台保存 |
| `terminal` | crossterm 输入翻译、终端生命周期和 `Canvas` 输出 |
| `tui` | Taffy 布局、viewport 跟随、pull 查询和终端绘制 |
| `main` | 组装 Terminal、TUI Frontend 与 `App<TuiFrontend<_>>` |

`core` 不感知终端、布局、异步任务或渲染；`protocol` 不执行业务 IO；具体前后端接线只在
`main.rs`。

## 3. 顶层所有权

当前运行时所有权为：

```text
App<F: Frontend>
├── Kernel
│   ├── ContentStore
│   ├── ModeRegistry
│   ├── ContentModeInstances
│   ├── TransactionManager
│   ├── AppTasks
│   ├── pending_saves
│   └── AppMessage channel
├── ClientSession
│   ├── Scene + SceneBuilder + scene revision
│   ├── ViewStore: HashMap<ViewId, View>
│   ├── ViewModeInstances
│   ├── focused SpaceId
│   └── Dispatcher
└── F: Frontend
```

`Kernel` 与 `ClientSession` 已按共享数据和客户端会话拆分，但当前仍是一对一组合：没有
session registry，也没有用 `Arc<Mutex<_>>` 提供并发共享。该拆分首先用于明确所有权，并为
未来多 Frontend 保留自然扩展点。

启动由 `app::bootstrap` 统一分配 editor/status 的 `ContentId` 与初始 `ViewId`，再把明确的
View↔Content 绑定和下一个 ViewId 传给 `ClientSession`。Session 构造不读取约定编号来猜测
Content 角色，`App` 也不长期保存 editor/status 角色字段。

Viewport 属于具体 Frontend。TUI 的 `SceneRenderer` 按 `ViewId` 保存 viewport，后端
`ClientSession` 不保存终端滚动位置。

## 4. 三种身份

布局、交互会话和共享内容使用不同 ID：

```text
Scene: SpaceId -> ViewId
View:  ViewId  -> ContentId + ContentViewState
Store: ContentId -> Content
Mode:  (ModeId, ViewId | ContentId) -> ModeInstance
```

- `SpaceId` 标识 Scene 树中的布局节点；
- `ViewId` 标识一次独立的展示和交互会话；
- `ContentId` 标识可被多个 View 引用的共享内容。

同一个 `ViewId` 不能同时挂载到多个 Scene leaf。同一 `ContentId` 可以由多个 View 展示，
这些 View 拥有彼此独立的 selection、revision 和前端 viewport。ViewMode
状态按 View 隔离；ContentMode 状态按 Content 共享。

## 5. Content 与 View

### 5.1 Content

`Content` 当前是静态闭合枚举：

```rust
enum Content {
    Buffer(Buffer),
    StatusBar(StatusBar),
}
```

`ContentStore` 是唯一 Content 表，每个 `ContentEntry` 同时保存 Content 与 Revision，重复
ContentId 会在不修改旧条目的前提下返回错误。除启动时构造内建 Content 外，App 的执行与查询路径不借出
`Buffer`/`StatusBar`，而是通过 ContentStore 分派。Content 接收已解析的
`ContentAction`、保存请求和后台 `ContentEvent`。它不接收顶层 Command、
`ContentInput::View` 或可变 `ContentViewState`。

文本操作先由 app 使用只读 selections 规划为静态 `TextChangeSet`，再由
Content 验证和应用。Content 返回规范 `ContentChange`，app 将它映射到绑定
同一 Content 的所有 View。

执行结果显式区分 `Handled(ContentEffect)` 与 `NotHandled`。当前 effect 只有保存快照；App
解释 effect 并启动 IO，Content 不直接管理异步任务或 Frontend。

只读数据统一通过 `ContentStore::query(ContentId, ContentQuery)` 返回 owned `ContentData`。
状态栏通过目标 Buffer 的 `DocumentStatus` 生成数据，而不是让渲染层识别具体 Content 类型。
Content 还通过穷尽分派声明 `ContentPresentation::{Text, StatusBar}`；AppQuery 据此组装带 View
会话状态的 `ViewPresentation`，不会通过 selection 是否存在来推断 Content 类型。

### 5.2 View

`View` 是 App 层完整的交互边界，当前持有：

```text
View
├── ContentId
├── ContentViewState
└── Revision
```

`ContentViewState` 只表达跨 Content 复用的 View 能力。文本 View 保存
`Selections`，无状态 View 没有 selection。具体 Content 负责创建状态，并在
change mapping 边界验证能力是否匹配。

View 只保存会话数据，不保存或代理 Mode。keymap、动态 capture、timeout、
cursor style 和 Mode action 都由 app 中的集中实例表处理。App 与 Dispatcher
不读取 Vim 的 count、operator 或字符搜索状态。

## 6. Mode 与命令模型

### 6.1 静态能力与实例作用域

`ModeRegistry` 只接受 `ContentMode` 或 `ViewMode` 两种静态定义。

- `ContentModeContext` 只有目标 Content identity 和只读 query；
- `ViewModeContext` 只有绑定 View、其 selections 和绑定 Content 的只读 query；
- 两种 Context 都不借出可变状态；
- keymap、typing、capture、timeout、cancel、presentation 和 execute 都使用
  对应的 Context 类型。

`ContentModeInstance` 按 `(ModeId, ContentId)` 共享，`ViewModeInstance` 按
`(ModeId, ViewId)` 隔离。每个 View 最多解析到 ContentMode、ViewMode 或无
Mode 三者之一。Vim 是 ViewMode，因此不同 View 可以处于不同 Vim 状态。

Mode 返回有序 typed operation。`ContentModeResult` 不能表达 View identity
或 `ViewAction`；`ViewModeResult` 可以表达绑定 View、Content、事务、App 和
Viewport 操作。ModeState 是 provisional 状态，跨层失败会恢复调用前快照。

### 6.2 路由与领域 action

顶层 `Command`、`AppCommand`、`ModeCommand` 和目标解析都在 app。core 只保留
`EditCommand` 等纯领域算法、`ContentAction` 和 Content 事务数据。

```text
key / timeout / script / event
-> Dispatcher 或直接 typed action
-> ContentModeResult / ViewModeResult
-> app ordered executor
-> ViewAction / ContentAction / TransactionIntent / AppAction
```

`EditCommand` 在 app 中结合只读 View selections 和 Content 解析为静态
`ResolvedViewEdit`。Content 只验证并应用 `ContentAction`，不再执行 selection
移动或顶层命令。Save、undo、redo 和 Content event 使用各自的 typed 路径。

ordered executor 使用固定命令链上限。任一步失败时，它反向恢复本次已应用的
Content 数据，并恢复 ModeState、View selections 和 TransactionManager。
依赖前端几何的 viewport 操作先只读解析，待整条命令链成功后再提交前端状态。
Visual operator 在切换 Vim 状态前解析静态范围；进入 Normal 还会显式压缩
selections。

## 7. 输入架构

### 7.1 中立按键

Terminal 将 crossterm 事件翻译为协议层：

```rust
KeyEvent { code: KeyCode, modifiers: KeyModifiers }
```

Ctrl、Alt、Shift 都是 modifiers，不编码进 `KeyCode`。GUI 或远程 Frontend 将来应产生同一
中立事件，而不是复用 terminal 翻译细节。

### 7.2 固定序列

`Keymap<A>` 是泛型 trie。每个 `KeyNode<A>` 可以同时拥有 action 和 children，因此单键动作与
更长前缀可以共存。该模块只依赖泛型 action 与中立 `KeyEvent`，不认识具体 `Command`；
Mode 构造 keymap 时负责把 `EditCommand` 包装为顶层命令。当前活动固定层只有：

1. focused View 当前 Mode 的 keymap；
2. global keymap。

Dispatcher 不物理合并这些树，而是在匹配时虚拟叠加：同序列按层优先级选择 action，任一层
存在更长候选就继续等待。中止时选择已缓冲序列中消费按键最多的完整绑定。

每个 binding 的 RHS 是一个结构化 action，不在 keymap 层内嵌 action list、脚本字符串或递归
remap。需要组合行为时，由该 action 对应的语义命令或未来脚本函数负责。

Leader 是定义绑定时展开的 alias，运行时 trie 只保存 concrete `KeyEvent`。固定序列共享全局
默认超时；显式 prefix 可以覆盖为 `After(Duration)` 或 `Never`，descendant 继承当前路径上
最近遇到的设置。

### 7.3 通用 Awaiting

动态输入使用与 Vim 无关的值类型：

```rust
InputStatus::{Ready, Awaiting(TimeoutPolicy)}
InputDecision<A>::{Pass, Consumed, Emit(A)}
```

`InputCoordinator` 将固定序列 pending 和动态 context 放在同一 LIFO 等待栈中，并选择最近到期
的 deadline；被较新 context 覆盖的底层 timer 不暂停。Dispatcher 只在 context 处于
`Awaiting` 时调用 `capture`；`Pass` 传播原键，`Consumed` 吞掉输入，`Emit` 产生中立 action。
具体回调由 `ContentMode` 和 `ViewMode` 分别声明，并接收各自的只读 Context，
不再通过一个无法表达 View 能力边界的统一 `InputContext` trait。

固定序列 mismatch/timeout 的处理顺序为：

1. 若已有完整绑定，先执行它；
2. 再把未消费的缓冲键作为普通新输入 replay；
3. 若没有完整绑定，原始 prefix 直接走 unmapped/fallback，不重新进入固定 keymap；
4. 导致 mismatch 的新键随后按普通输入处理。

mode 或 focus 切换会丢弃相关 fixed pending，并取消受影响 View 的动态 Awaiting，不 replay 旧
状态下的输入。

### 7.4 App 输入循环

App 在每轮事件循环中向 Dispatcher 查询最近 input deadline，并与 Frontend event、后台
`AppMessage`、任务取消信号一起进入 `tokio::select!`。输入使用显式队列处理，保证
“执行 action -> 同步 focused View 输入状态 -> replay”这一顺序。

## 8. Scene、布局与渲染

### 8.1 Scene 模型

`protocol::scene` 只保存可拥有的 `Scene`/`SpaceNode` 快照和只读访问。树修改属于
`app::scene_model::SceneBuilder`：

- 分配唯一 `SpaceId`；
- 构建标准 editor + status bar 场景；
- split、close、replace view、set sizing；
- 校验父子关系、Content leaf 和 View 唯一挂载不变量。

`Scene::nodes` 的 HashMap key 是 Space identity 的唯一真相源，`SpaceNode` 与 `Space` 不再
重复保存 SpaceId。SceneBuilder 的 SpaceId 分配与其他 ID/Revision 一样使用 checked add。

每个 `ClientSession` 持有唯一 builder。成功修改 Scene 后递增 scene revision，并同步创建、
移除或保留对应 View。

### 8.2 Pull 渲染

渲染数据流为：

```text
Scene snapshot
  -> TaffyEngine.layout(scene, scene_revision)
  -> ResolvedScene<RenderItem>
  -> RenderQuery.view/content
  -> viewport follow + paint
  -> Canvas
```

`TaffyEngine` 按 scene revision 缓存 `ResolvedScene`。revision 改变时，当前实现重建整棵
`TaffyTree` 并重新计算布局；它不会在每一帧无条件创建新树，也尚未消费 Scene diff 做增量更新。

`SceneRenderer` 按 `ViewPresentation::{Text, StatusBar}` 显式分派，不通过不支持的 query 猜测
Content 类型。它只拉取可见文本行，并在前端计算 `DisplayPoint`、viewport 和光标位置；每帧
按 resolved ViewId 清理已离开 Scene 的 viewport，移动到其他 Space 的 View 状态仍会保留。

### 8.3 文本位置

```text
TextOffset   Selection 持久保存的 Rope 字符偏移
TextPoint    Buffer 按当前内容派生的逻辑行列
DisplayPoint TUI 结合布局和 viewport 计算的显示位置
```

`TextPoint.col` 仍是 Unicode scalar 的逻辑列；TUI 的 `text_cells` 使用 `unicode-width` 把可见
scalar 映射为 terminal cell 宽度，宽字符的 viewport 跟随、裁剪、selection paint 和光标列都按
cell 计算。CR/LF 不进入行内容，其他控制字符在输出前替换为 U+FFFD。tab stop、grapheme cluster、
组合 emoji 序列和软换行尚未进入显示模型。

## 9. 事务与保存

`TransactionManager` 是 history、history cursor、redo 截断和活动事务的唯一
所有者。每个 `ContentId` 有独立事务流，活动事务记录可选的来源 `ViewId`。
跨 View 编辑、关闭 owner View 和 Save 都通过同一 checkpoint 路径。

core 的闭合 `ContentTransaction` 负责分派具体 Content 事务数据；app 的
`TransactionRecord` 只将该中立载荷与通用 View participant 配对，不匹配
Buffer 等具体类型。View participant 使用 `Source { before, after }`，无
View 输入使用 `None`，不伪造 `ViewId`。Buffer 只生成、验证和应用文本事务
数据，并维护 current/saved `TextStateId`；它没有 history stack 或 history
cursor。

ordered executor 只在首次事务写入时保存目标 Content flow 的 active 状态和
可能被截断的 redo 尾部。已提交历史前缀不会为每条命令重复复制；失败时用
该 checkpoint 恢复 Manager，成功时直接丢弃。

undo/redo 先按 `ContentId` 选择历史流。来源 View 仍存在时恢复其 selections，
其他 View 只执行 `ContentChange` mapping；来源 View 已关闭时跳过该快照。
ModeState、viewport、focus 和布局不进入第一阶段历史。

### 9.1 保存与后台任务

Buffer 每次编辑递增文档 revision。保存时 Content 生成包含 path、bytes 和 revision 的不可变
`SaveSnapshot`，App 使用临时文件加 rename 执行原子写入。

保存完成事件带回原 revision。只有当前 Buffer revision 与已保存 revision 相等时才清除
modified；在途保存期间的新保存请求保留最新快照，并在当前任务结束后继续执行。关闭时取消
普通任务，但等待 critical 保存任务完成。

## 10. 前后端与远程语义边界

同进程 Frontend 只有两个行为：异步产生 `FrontendEvent`，以及同步
`render(&Scene, Revision, &dyn RenderQuery, focused)`。

协议层还定义了尚未接入 transport 的 owned 远程语义消息：

- `Hello/Welcome` 与 capability negotiation；
- 带 `RequestId` 的 View/Content request/response；
- scene、view、content revision；
- `SceneChanged`、`ViewChanged`、`ContentInvalidated` 通知；
- unknown object、unsupported query 和版本不兼容等结构化错误。

`app::remote` 已能把本地 `AppQuery` 适配为 response，但当前没有 serde、网络 transport、Scene
snapshot/delta、连接管理或远程 Frontend 事件循环。

## 11. 当前不变量

- Content 共享状态与 View 会话状态分离。
- ContentViewState 只表达通用 View 能力，不镜像具体 Content 变体。
- Mode 定义由 registry 共享；ViewModeState 按 View 隔离，ContentModeState
  按 Content 共享。
- 每个 View 只解析到 ContentMode、ViewMode 或无 Mode 三者之一。
- TransactionManager 是历史和活动事务生命周期的唯一所有者。
- Buffer 不保存 View selections、history stack 或 history cursor。
- App/Dispatcher 只看通用输入状态和命令，不读取具体 Mode 语法。
- SpaceId、ViewId、ContentId 不互相替代。
- ContentStore 是唯一 Content 表；Frontend 不识别具体 Content 对象。
- 布局和 viewport 属于 TUI；core/protocol 不依赖 Taffy。
- Scene builder 属于 App，协议只保存 Scene 快照。
- 渲染使用 owned pull query，后端不 push frame。
- 异步保存结果必须以文档 revision 校验，不能覆盖更新后的状态。

## 12. 当前有意保留的边界

- Content 继续使用静态 enum，不为 Mode 脚本化提前改成插件对象。
- 只有单 Frontend、单 ClientSession；不提前加入并发共享和连接管理。
- ModeRegistry 只承载原生 Mode；不伪造尚未确定的脚本对象或 ABI 字段。
- 当前只有 focused Mode + global 两个固定 keymap 层，不提前实现完整 major/minor mode stack。
- 远程协议只完成语义数据结构，不提前绑定 transport 和序列化库。
- Taffy 只按 revision 缓存整树结果；是否增量更新由真实性能数据决定。
