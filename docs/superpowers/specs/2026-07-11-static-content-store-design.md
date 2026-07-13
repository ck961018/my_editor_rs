# 静态 Content 与 ContentStore 设计

**日期：** 2026-07-11  
**状态：** 已实施

> 2026-07-13 修订：View 输入中的 `selections + ContentRuntime` 已由静态
> `ContentViewState` 取代；Mode action 由 View 自己的 `ModeInstance` 执行。

## 目标

将当前以 `HashMap<ContentId, Box<dyn ContentHandler>>` 保存内容，并通过
`buffer_mut`、`as_buffer`、`as_status_bar` 等手写 RTTI 分派的模型，改为静态、
闭合的 `Content` 枚举与 `ContentStore`。

本设计的目标是：

- 内容类型在编译期确定；不使用内容 trait object 或运行时 downcast。
- `App` 不再借出或识别 `Buffer`、`StatusBar` 等具体内容类型。
- 内容只通过一个 `execute` 入口接收命令和异步事件。
- 前端继续使用 pull 模型；内容查询与按 Space 归属的 selection 查询均不泄漏
  App 的内部存储。
- 保持 `View` 按 `SpaceId` 持有 selection，避免同一内容被多个 Space 显示时混用
  会话状态。

本设计不增加终端、选择器等新内容类型；它只建立这些类型以后加入静态集合时应遵守
的边界。

## 非目标

- 不实现终端、选择器或其他新 `Content` 变体。
- 不支持插件或运行时注册新的内容类型。
- 不实现同一内容被多个 View 编辑后的其他 selection 位置变换同步。
- 不实现 split、panel 或 overlay 的 Scene/View 生命周期。
- 不拆分 Buffer 内的 mode/keymap runtime；该工作属于路线图第四项。

## 内容模型

`ContentHandler` 与 `ContentLookup` 删除。它们只为动态集合服务；在静态闭合集合中，
它们会与枚举分派重复。

```rust
pub enum Content {
    Buffer(Buffer),
    StatusBar(StatusBar),
}

pub struct ContentStore {
    contents: HashMap<ContentId, Content>,
}
```

`ContentStore` 位于 `core`，是唯一的内容表；`App` 持有该 Store，而不直接持有裸
`HashMap`。新增内容类型必须新增 `Content` 变体，编译器会强制补全所有分派位置。

`Content` 以 inherent method 提供当前分发所需的行为：

```rust
impl Content {
    fn keymap(&self) -> &Keymap;
    fn keymap_mut(&mut self) -> &mut Keymap;
    fn resolve_key(&self, key: KeyEvent) -> Option<Command>;
    fn execute(&mut self, input: ContentInput<'_>) -> ContentEffect;
}
```

这些方法通过 `match` 委派到具体变体。`Content` 不添加渲染方法；读取渲染数据由
`ContentStore::query` 处理。

## 命令与事件

现有 `ContentCommand` 保留为 Keymap、`Command` 和 Dispatcher 使用的纯命令类型，
以便继续 `Clone` 并存储在 Keymap 中。仅作术语修正：

```rust
pub enum ContentCommand {
    Edit(EditCommand),
    Save,
    Mode { mode: ModeId, action: ModeActionId },
}
```

`TextCommand` 重命名为 `EditCommand`。其变体涵盖 selection 移动、扩展、折叠、
插入和删除，因此不能用 `Text` 概括。

实际调用 Content 时，App 已经解析出目标 `ContentId`，并且对编辑命令还掌握目标
`SpaceId` 对应的 selection。新增运行时输入枚举：

```rust
pub enum ContentInput<'a> {
    Command(ContentCommand),
    WithSelections {
        command: ContentCommand,
        selections: &'a mut Selections,
    },
    Event(ContentEvent),
}

pub enum ContentEvent {
    SaveFinished {
        revision: u64,
        result: io::Result<()>,
    },
}
```

- `Command` 用于 `Save`、`Mode` 等不依赖 View 状态的命令。
- `WithSelections` 用于 `ContentCommand::Edit(EditCommand)`；selection 只在本次调用
  中借给内容，Content 不保存 `View`、`SpaceId` 或 selection 引用。
- `Event` 承载 App 返回的异步事实；它不混入用户发起的 `ContentCommand`。

`View` 继续位于 `app` 层并按 `SpaceId` 管理。Content 修改共享文本与传入的当前
selection；本项不处理其他 View 的 selection 变换。

## 副作用与保存

Content 不依赖 Tokio、不写文件、也不管理 `pending_saves`。它只描述 App 必须执行的
外部副作用：

```rust
pub enum ContentEffect {
    None,
    Save(SaveSnapshot),
}
```

保存流如下：

```text
App -> Content::execute(Command(Save))
Content -> ContentEffect::Save(snapshot + revision)
App -> 原子写入文件并维护在途/最新排队快照
task -> AppMessage::SaveCompleted(revision, result)
App -> Content::execute(Event(SaveFinished(revision, result)))
```

Buffer 只在完成 revision 等于当前 revision 时清除 `modified` 并更新 `Saved`；失败时更新
`SaveFailed`。没有路径的 Buffer 保持现有
行为：更新失败状态且不产生 Save effect。其他内容对不适用输入返回 `ContentEffect::None`，
不再因“目标必须是 Buffer”的假设而 panic。

## 查询与渲染

`ContentQuery` 改为发送给内容的查询消息，`ContentData` 是其 owned 响应：

```rust
pub enum ContentQuery {
    TextRows(RowRange),
    TextLineCount,
    DocumentStatus,
    StatusBarData,
}

pub enum ContentData {
    TextRows(Vec<String>),
    TextLineCount(usize),
    DocumentStatus(DocumentStatus),
    StatusBarData(StatusBarData),
    Unsupported,
}
```

`ContentStore::query(content_id, query)` 静态分派到目标 `Content`。Buffer 响应文本行、
行数和文档状态；StatusBar 在响应 `StatusBarData` 时，通过 Store 向其目标内容请求
`DocumentStatus`。StatusBar 不识别目标的具体类型。

当前前端查询 trait 同时提供 Content 数据与按 Space 归属的 selections；它不是纯内容
查询，因此重命名为 `RenderQuery`：

```rust
pub trait RenderQuery {
    fn content(&self, id: ContentId, query: ContentQuery) -> ContentData;
    fn selections(&self, id: SpaceId) -> Selections;
}
```

`RenderQuery` 是渲染层只读投影，而非领域模型。一个 `RenderItem` 同时携带 `content_id`
和 `space_id`：前者读取共享内容，后者读取该可见实例的 selections。TUI 不能依赖 App，
也不应了解 App 分别保存 ContentStore 与 views 的实现细节。

`AppQuery` 保留为轻量适配器：其 `content` 方法转发给 ContentStore，其 `selections`
方法读取 `views`。它不再识别任何具体内容类型。`Unsupported` 沿用当前的渲染回退：
文本为空，状态栏为默认数据。

## 错误处理

- ContentStore 找不到内容或内容不支持查询时返回 `ContentData::Unsupported`。
- 内容收到不适用输入时返回 `ContentEffect::None`。
- 缺失的 StatusBar 目标产生默认状态栏数据。
- 文件 IO 仍由 App 的 `AppMessage` 报告，不把 Tokio 或 IO 写入引入 core。

## 测试与验证

实现应覆盖：

- Content 枚举和 ContentStore 对 Buffer、StatusBar 的静态分派。
- Edit 输入只更新传入的 selections 与共享 Buffer。
- 保存 effect，以及成功和失败事件返回后的 Buffer 状态。
- StatusBar 对目标内容的 `DocumentStatus` 查询和目标缺失回退。
- 不适用命令不 panic 且不产生副作用。
- App 将 Dispatcher 的 Edit 目标包装为 `ContentInput::WithSelections`。
- App 保存任务、完成事件和 `ScriptedFrontend` 集成流程保持可用。
- SceneRenderer 通过 `RenderQuery` 渲染同一 ContentId 在多个 SpaceId 中的不同
  selections。

完成 Rust 实现后运行：

```text
cargo test
cargo clippy --all-targets --all-features
```

同时更新 `AGENTS.md`：删除将 `ContentHandler` 描述为长期分发契约的表述，改为说明
`Content` 是静态闭合集合、`ContentStore` 是唯一内容表，以及渲染查询不属于 Content
自身行为。
