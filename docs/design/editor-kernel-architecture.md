# Editor Kernel Architecture

**状态：** 当前实现架构
**更新日期：** 2026-07-16

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
| `core` | Buffer、Content、ContentStore、编辑命令、Mode、keymap trie 和通用输入状态机 |
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
│   ├── AppTasks
│   ├── pending_saves
│   └── AppMessage channel
├── ClientSession
│   ├── Scene + SceneBuilder + scene revision
│   ├── ViewStore: HashMap<ViewId, View>
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
View:  ViewId  -> ContentId + ModeInstance + ContentViewState
Store: ContentId -> Content
```

- `SpaceId` 标识 Scene 树中的布局节点；
- `ViewId` 标识一次独立的展示和交互会话；
- `ContentId` 标识可被多个 View 引用的共享内容。

同一个 `ViewId` 不能同时挂载到多个 Scene leaf。同一 `ContentId` 可以由多个 View 展示，
这些 View 拥有彼此独立的 selection、Mode 状态、revision 和前端 viewport。

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
`Buffer`/`StatusBar`，而是通过 ContentStore 分派。Content 通过三类 `ContentInput` 接收行为：

- `Command`：不依赖某个 View 的共享内容命令，例如保存；
- `View`：同时携带语义命令和该 View 的 `ContentViewState`，例如编辑文本并更新 selection；
- `Event`：后台结果，例如带 Buffer revision 的 `SaveFinished`。

执行结果显式区分 `Handled(ContentEffect)` 与 `NotHandled`。当前 effect 只有保存快照；App
解释 effect 并启动 IO，Content 不直接管理异步任务或 Frontend。

只读数据统一通过 `ContentStore::query(ContentId, ContentQuery)` 返回 owned `ContentData`。
状态栏通过目标 Buffer 的 `DocumentStatus` 生成数据，而不是让渲染层识别具体 Content 类型。

### 5.2 View

`View` 是 App 层完整的交互边界，当前持有：

```text
View
├── ContentId
├── ContentViewState
├── Option<ModeInstance>
└── Revision
```

`ContentViewState` 与 Content 类型配对。Buffer View 保存 `Selections`，StatusBar View 没有
selection。类型不匹配属于 App 内部不变量破坏，会直接 panic，而不是跨边界的可恢复错误。

View 对上游只暴露中立行为：当前 keymap、输入状态、动态 capture、typing fallback、输入取消、
超时通知、cursor style 和 mode command 执行。App 与 Dispatcher 不读取 Vim 的 count、operator
或字符搜索状态。

## 6. Mode 与命令模型

### 6.1 定义、注册与实例

`Mode` trait 是原生 Mode 的定义契约，负责：

- 声明 owned `ModeName` 与 `ModeActionName`；
- 为每个 View 创建私有 `ModeState`；
- 根据状态提供 keymap、typing fallback 和 cursor style；
- 实现通用 `InputContext<Command>` 所需的等待、capture、timeout 与 cancel 行为；
- 执行 `ModeCommand`，原地更新私有状态，并按需产生普通顶层 `Command`。

`ModeRegistry` 按名称注册定义，并在进程内分配稳定的 `ModeId`/`ModeActionId`。`ModeInstance`
通过 `Rc` 共享已注册定义，同时独占 `Box<dyn ModeState>`。当前生产 registry 只注册内建 Vim；
脚本 adapter 尚未实现。

Mode 的私有语法只产生通用命令。例如 Vim 的 `f/F`、count 和 `dd` 最终产生
`MoveToChar`、`MoveToLine` 或 `DeleteLines`，Buffer 不知道这些按键语法。

### 6.2 命令层级

```text
Command
├── App(AppCommand)
├── Content(ContentCommand)
│   ├── Edit(EditCommand)
│   ├── Transaction / Undo / Redo
│   ├── Sequence(ContentSequence)
│   └── Save
├── Mode(ModeCommand)
├── Viewport(ViewportCommand)
└── Noop
```

`Dispatcher` 再将 `Command` 解析为带实际目标的 App 内部 `DispatchCommand`。App command 由 App
执行；Mode command 交给目标 View 的 ModeInstance；Content command 交给目标 `ContentStore`
条目。`Save` 只需要 `ContentId`，其他 Content command 同时借用目标 View 的
`ContentViewState`。这是同一命令类型的两种执行上下文，不形成第二套 view-content 命令。

`ContentSequence` 是验证后的有序 Content 命令容器，只接受需要 `ContentViewState` 的命令，
不能包含 `Save`。它保证所有成员使用同一个执行上下文，但不代替 Content transaction 的回滚
与 undo 语义。

Mode action 返回 `Option<Command>`，并以原始 View 为来源重新进入与 keymap 相同的 Dispatcher
目标解析入口。因此 Mode 可以产生 Content、Viewport 或 App 命令；全局 keymap 也可以直接
绑定 `Command::Viewport`，并解析到 focused View。Viewport 由 Frontend 根据实际 pane 高度
解析，再降低为 `ContentCommand::Edit`。命令链使用有固定上限的迭代执行，避免用户 Mode
通过 `Mode -> Mode` 造成递归溢出或无限循环。unknown mode/action 返回 `ModeError`，不再被
App 静默吞掉。

Mode action 先更新 Mode 私有状态，再执行后续 replay。这样按键导致 Normal/Insert 切换后，
同一输入队列中的下一个键立即使用新状态。

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

动态输入使用与 Vim 无关的接口：

```rust
InputStatus::{Ready, Awaiting(TimeoutPolicy)}
InputDecision<A>::{Pass, Consumed, Emit(A)}
InputContext<A>::{status, capture, on_timeout, cancel}
```

`InputCoordinator` 将固定序列 pending 和动态 context 放在同一 LIFO 等待栈中，并选择最近到期
的 deadline；被较新 context 覆盖的底层 timer 不暂停。Dispatcher 只在 context 处于
`Awaiting` 时调用 `capture`；`Pass` 传播原键，`Consumed` 吞掉输入，`Emit` 产生中立 action。

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
Content 类型。它只拉取可见文本行，并在前端计算 `DisplayPoint`、viewport 和光标位置。

### 8.3 文本位置

```text
TextOffset   Selection 持久保存的 Rope 字符偏移
TextPoint    Buffer 按当前内容派生的逻辑行列
DisplayPoint TUI 结合布局和 viewport 计算的显示位置
```

当前逻辑列与 terminal cell 列按一对一映射。tab stop、Unicode width、grapheme、emoji 和软换行
尚未进入显示模型。

## 9. 保存与后台任务

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
- Mode 定义由 registry 共享，ModeInstance 和 ModeState 按 View 隔离。
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
