# Content Runtime 与 Mode 状态设计

**日期：** 2026-07-11
**状态：** 已确认，待实施

## 背景

当前 `Content` 已是静态闭合枚举，`ContentStore` 是唯一内容表，`View` 按
`SpaceId` 持有 selection。与此同时，`Buffer` 仍同时保存文本模型、普通
keymap，以及 `BufferModes` 中的 Vim 行为和 `Normal`/`Insert` 可变状态。

这使同一 Buffer 被多个 Space 显示时共享 mode 状态：一个视图进入 Insert，其他
视图也会进入 Insert。它也使 mode 的私有状态无法扩展为用户定义的、不泄漏到
App、Space 或 Content 通用层的数据。

本设计替代
`2026-07-09-content-mode-design.md` 中关于 mode 状态归属、`Mode` 可变实例和
`ContentCommand::Mode` 仅按 Content 分派的部分。此前已经实现的最小 Vim 行为和
静态 ContentStore 边界继续保留。

## 目标

- 每个静态 `Content` 变体都能创建匹配的 `ContentRuntime`。
- `View` 按 `SpaceId` 独占持有当前 Content 的 runtime；同一 Content 的不同
  View 彼此独立。
- mode 的行为与配置仍归 Content；当前由 Buffer 暂时持有该行为。
- 一个 Buffer runtime 保留该 Buffer 所有 mode 的运行时状态；mode 切换只修改
  已有状态，不销毁或重建其他 mode 的状态。
- mode 的具体运行时字段只由对应 mode 自己理解。App、View、ContentStore、
  `ContentRuntime` 通用分派和 Buffer 都不识别这些字段。
- mode 命令按 `SpaceId + ContentId` 执行，以便操作目标 View 的 runtime。
- 保持现有按键、保存和渲染行为。

## 非目标

- 不将 `Buffer` 拆成纯文本模型和 `BufferContent` 包装；该项后续单独讨论。
- 不统一 prefix key、capture chain 或按键序列状态机；`Dispatcher` 的现有前缀
  逻辑暂时保持。
- 不把 keymap 或 prefix 等待状态放入 `ContentRuntime`。
- 不向 `RenderQuery` 增加 mode 状态，也不改变状态栏 UI。
- 不实现新的 mode、用户脚本或插件加载机制。
- 不实现动态 Scene/View 生命周期；本设计只定义创建或改绑 View 时应遵循的
  runtime 生命周期。

## 静态 Content Runtime

`ContentRuntime` 与 `Content` 同样是闭合集合，不能改为
`Box<dyn ContentRuntime>` 或 `Box<dyn Any>`。新增 Content 变体时必须同时新增
其 runtime 变体，并补全静态分派。

```rust
pub enum Content {
    Buffer(Buffer),
    StatusBar(StatusBar),
}

pub enum ContentRuntime {
    Buffer(BufferRuntime),
    StatusBar(StatusBarRuntime),
}
```

每个 Content 提供创建匹配 runtime 的入口。`ContentStore` 通过该入口为指定
`ContentId` 创建 runtime；App 不识别 `BufferRuntime`、`StatusBarRuntime` 等
具体类型。

`StatusBarRuntime` 当前可以为空，但仍必须存在。这样终端、选择器或未来的其他
Content 以后均能有自己的 View 会话状态，而无需在 `View` 增加专用字段。

## View 生命周期

`View` 由以下三项组成：绑定的 `ContentId`、按 Space 归属的 selections，以及
匹配的 `ContentRuntime`。

```text
创建 View 或将其改绑到 Content C
    -> ContentStore 为 C 创建 runtime
    -> View 原子地保存 C 与该 runtime

同一 View 中切换 mode
    -> 保留同一个 runtime，只修改其内部状态

View 从 Buffer A 改绑到 Buffer B
    -> 丢弃 A 的 runtime，创建新的 B runtime

之后重新绑定 A
    -> 再创建新的 A runtime
```

因此 runtime 的生命周期单位是“一次 Space 与 Content 的绑定”。两个 Space 同时
显示同一 Buffer 时，各自拥有独立 runtime；一个 Space 内部的所有 mode 状态则持续
保留，直到该绑定结束。

Content 与 runtime 的变体不匹配是内部不变量错误。实现不得为了掩盖错误而静默创建
或替换 runtime；构造和改绑路径应集中保证配对正确。

## Buffer Mode 行为与状态

本项不改变 `Content::Buffer(Buffer)` 的结构。`Buffer` 暂时仍持有 mode/keymap
行为与配置，但不再持有可变 mode 状态。

```text
Buffer
└─ ModeSet
   └─ 已注册 mode 的行为、配置与 keymap

ContentRuntime::Buffer
└─ BufferRuntime
   └─ ModeStateSet
      └─ 每个已注册 mode 一份不透明 state
```

`ModeSet` 创建 `ModeStateSet`，并根据目标 mode 将执行转发给具体 mode。具体 mode
创建、读取和修改自己的 state；公共容器至多识别稳定的 `ModeId`，不识别任何 mode
字段。实现可在 mode 模块内部使用 trait object 或类型擦除，但该机制不得泄漏到
Buffer、View、App 或 ContentStore。

第一版继续把 `Vim` 视为一个 mode。其 runtime 私有地保存 `Normal`/`Insert` 状态，
而不是把两者拆成两个独立 mode。`Esc`、`i` 等动作只修改该 Vim state；整个
`ModeStateSet` 不被替换。

这保留了既有最小 Vim 行为，同时允许未来 mode 以自己的方式定义更复杂的状态，
例如 operator-pending、计数或宏录制数据。

## 命令与执行路径

`ContentInput` 保持单一 `Content::execute` 入口，但将原有
`WithSelections` 替换为 View 输入：

```rust
pub enum ContentInput<'a> {
    Command(ContentCommand),
    View {
        command: ContentCommand,
        selections: &'a mut Selections,
        runtime: &'a mut ContentRuntime,
    },
    Event(ContentEvent),
}
```

- `View` 用于需要某个 Space 会话的命令，包括 `Edit` 和 `Mode`。Buffer 可将
  selections 用于编辑，并将匹配的 runtime 转交给 `ModeSet`。
- `Command` 继续用于纯 Content 操作，例如 `Save`。
- `Event` 继续用于 `SaveFinished` 等 App 异步事实，不附带某个 View 的 runtime。

Dispatcher 必须将 `ContentCommand::Mode` 解析为 `ViewContent` 目标；它与
`Edit` 一样携带 `SpaceId + ContentId`。`Save` 仍解析为只带 `ContentId` 的
Content 目标。

```text
KeyEvent
-> Dispatcher
-> DispatchCommand::ViewContent { space, content, command }
-> App 取出目标 View 的 selections 与 runtime
-> ContentStore::execute(content, ContentInput::View { ... })
-> Buffer / ModeSet / concrete Mode
```

## 按键边界

按键路由与 `ContentRuntime` 是两个概念：runtime 只提供当前 mode 所需的会话状态，
不会保存 keymap 或前缀等待状态。

在本项中，`Dispatcher` 的 global keymap、capture chain 和 pending prefix 行为保持
不变。Buffer 的 mode 行为可以只读 runtime 以选择当前有效的 mode keymap，但所有
prefix key 的统一解析留给后续独立改造。不得借本项将 prefix 状态迁入 View 或
ContentRuntime。

## 渲染边界

本项不增加 mode 可视化。`RenderQuery`、`ContentQuery`、状态栏数据和 TUI 输出均保持
不变。未来需要按 Space 显示 `NORMAL`/`INSERT` 时，应通过新的渲染查询单独设计，
而不是让前端读取 `ContentRuntime`。

## 测试与验收

实现至少覆盖：

- 每个现有 Content 可创建匹配的 `ContentRuntime`。
- 同一 Buffer 的两个 View runtime 相互独立；一个进入 Insert 不影响另一个。
- 单个 View 中 Vim 从 Normal 进入 Insert、返回 Normal 后，其完整 runtime 容器
  仍被保留。
- View 改绑 Content 时获得新 runtime；重新绑定先前 Content 时也获得新 runtime。
- `Mode` 命令解析为 `DispatchCommand::ViewContent`，并操作正确的 Space runtime。
- 现有编辑、保存、普通 keymap、mode keymap 和 prefix 测试保持通过。
- Content/runtime 不匹配会以明确的内部不变量失败，不产生静默恢复。
- 不新增 `RenderQuery` 的 mode 数据，也不改变状态栏输出。

完成实现后运行：

```text
cargo test
cargo clippy --all-targets --all-features
```

## 后续工作

以下工作明确不包含在本设计中：

- 将 `Buffer` 进一步拆成纯文档模型与 Content 交互包装。
- 统一 global、content 和 mode 的 prefix key 解析。
- 将当前 mode 作为按 Space 查询的渲染数据输出。
- 动态 Scene mutation、View 创建、改绑和销毁的统一 App 生命周期接口。
