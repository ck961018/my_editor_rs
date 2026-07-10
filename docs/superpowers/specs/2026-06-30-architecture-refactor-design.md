# 架构重构设计：Document/View 分离 + 各司其职

> 本文是 `my_editor_rs` 的一次架构重构设计 spec，基于 `main` 分支当前 v0.2 架构的问题，重新划分模块职责。
> 策略：一次性重写（非分阶段迁移）。
> 对应当前架构描述：`docs/design/current-architecture.md`。

## 1. 背景与动机

v0.2 Scene 驱动前端层骨架清晰（分层、协议中立、Scene 抽象、Taffy 隔离、测试扎实），但职责混乱主要集中在 `layout/content.rs` 的 `ContentStore`：它把**状态持有**（Editor+Viewport）、**几何路由**（content_kind 二分）、**事件分派**（handle_event）三职责混在一起，并用两个 `ContentId` 伪装多内容（实际只有一个 Editor 状态）。此外 `Viewport` 越权住在 `tui/` 且 `height-1` 硬编码状态栏行数，`CorePatch` 是死协议，`Editor` 把文档状态与光标状态混在一起。

核心病因是**抽象的形状与现实不匹配**：为"未来多内容/多面板"预付的抽象，落在一个单编辑器 + 状态栏的现实上，演变成"一个演员分饰多角"。

本次重构不缩小抽象，而是**把抽象摆正**：采用经典 Document/View 分离模型——Content 是文档数据，Space 是视图实例，一份文档可被多个视图展示。让每个模块真正各司其职。

## 2. 目标与非目标

### 目标

1. **App 不感知 tui/gui**：App 只依赖 `Frontend` trait（render + next_event），不碰 crossterm、`TuiRenderer`、`Output`、`TerminalGuard`。换 GUI 只需新实现 `Frontend`。
2. **layout 不感知 editor、不处理事件循环**：layout 引擎只算几何，不依赖 `core`，不持 Editor，不处理事件。事件循环在 App，事件处理在 core。
3. **Content 不感知 Editor**：Content（渲染策略）经中立 `EditView` trait 读文档数据，不 import `Editor`。
4. **保持 Scene/Space/Content 的 layout 设计**：`ContentId` 仍是 Space 引用 Content 的标签，几何仍由 Scene/Space/Taffy 算。Space 的语义补全为"带视图状态的展示实例"。
5. **Document/View 分离**：cursor 与 viewport 归 Space（视图），文档数据归 Content（Editor/Buffer）。支持一份内容多视图展示。

### 非目标

- 多 space 分屏的交互实现（v0.2 不落地，骨架支持）
- 多 cursor 共享 buffer 的同步（v0.2 不触发）
- wrap 软换行切换（字段预留，默认不折行）
- 水平滚动、多层 Layer/z_index、Terminal/Tree/Inspector 等内容类型
- 增量渲染/脏区域 diff（保持全量重绘）
- CorePatch 增量更新通道（删除，YAGNI）

## 3. 架构总览

### 3.1 分层与依赖方向

依赖自底向上单向。核心变化：cursor/viewport 上移到 layout 的 Space，core 退化为纯文档 + 纯函数；新增 protocol 的 `EditView`/`Frontend`/`CursorPos`/`Viewport` 契约；**layout 不再依赖 core**。

```
main.rs                 接线：组装 App，进入 TerminalGuard
  └─ app.rs             App：事件循环 + 持 contents/scene/focused/frontend；定义 Frontend trait
       ├─ frontend/tui/      TUI 投影（Frontend trait 的实现）
       │    ├─ tui_frontend.rs  TuiFrontend: impl Frontend（Content 注册表 + Output + Input + TerminalGuard）
       │    ├─ content/         EditorContent / StatusBarContent: impl Content
       │    └─ output.rs        Output<W>
       ├─ layout/          Scene 驱动几何（不依赖 core）
       │    ├─ space.rs        Space{ content_id, viewport, cursor, ... } / SpaceKind / Sizing / Layer
       │    ├─ scene.rs        Scene / SceneBuilder / Rect
       │    ├─ taffy_engine    纯几何算 rect，透传 Space 的 viewport/cursor
       │    ├─ resolved.rs     ResolvedScene / RenderItem（带 space 的 viewport+cursor）
       │    └─ ids.rs          SceneId / SpaceId / ContentId
       ├─ protocol/        中立契约（零上层依赖）
       │    ├─ frontend_event  FrontendEvent / ResizeEvent
       │    ├─ key_event       KeyEvent / translate_key
       │    ├─ cursor          CursorPos（中立光标值）
       │    ├─ viewport        Viewport（中立视口滚动值）
       │    └─ edit_view       EditView trait / ContentLookup trait / SpaceState / RenderCtx
       └─ core/             编辑模型（不持 cursor）
            ├─ buffer.rs       Buffer（rope + path + modified）+ 文件 IO
            ├─ edit.rs         编辑/移动纯函数（操作 &Buffer + &mut CursorPos）
            └─ status.rs       Status / StatusMessage
```

### 3.2 依赖关系

| 层 | 依赖 | 不感知 |
|---|---|---|
| `core` | `protocol`（`CursorPos`）+ `ropey` | 终端、视图状态、Space |
| `protocol` | 零依赖（仅 std） | 一切实质实现 |
| `layout` | `protocol`（`CursorPos`/`Viewport`/`Rect`/`ContentId`）+ `taffy` | core、Editor、终端、事件 |
| `frontend/tui` | `protocol` + `layout` + `terminal` | core、Editor、业务逻辑 |
| `app` | `core` + `protocol` + `layout` + `Frontend` trait | tui 实现细节 |

`Frontend` trait 放 app 层（app 定义、frontend 实现，依赖倒置），其 `render` 签名引用 `layout::ResolvedScene`——app 依赖 layout，无循环。`RenderCtx`/`SpaceState` 等渲染上下文为纯 protocol 类型，不含 `ResolvedScene`，避免 protocol↔layout 循环。

### 3.3 各层一句话职责

| 层 | 职责 |
|---|---|
| `core` | 文档数据（Buffer）+ 编辑/移动纯函数 |
| `protocol` | 中立契约：事件、`CursorPos`、`Viewport`、`EditView`/`Frontend` trait |
| `layout` | Scene/Space 几何 + 持视图状态（viewport/cursor） |
| `frontend/tui` | 把文档数据 + Space 状态画成终端字节 |
| `app` | 事件循环 + 持文档/Scene/焦点 + 编排 |

## 4. 各层详解

### 4.1 core —— 文档数据 + 纯函数

cursor 移走后，core 不持视图状态，只剩文档数据 + 操作它们的纯函数。无状态容器持有 cursor。

- **`Buffer`**（buffer.rs）：`rope + path + modified`。`load_from_file`（NotFound→空 Rope 降级新文件、非 UTF-8 由 App 进一步降级 OpenFailed）、`save`、`insert_char`、`delete_backward`、`line`、`len_lines` 等。基本沿用 v0.2，但不再被 cursor 依赖。
- **`Status`**（status.rs）：`StatusMessage` 枚举（None/Saved/SaveFailed/NewFile/OpenFailed），沿用 v0.2。
- **`edit.rs`**（新增）：编辑/移动纯函数，操作传入的 buffer + cursor + status，不知 Space/App/终端：

```rust
pub enum EditAction { None, Saved, SaveFailed, Quit }
pub enum Direction { Left, Right, Up, Down }

pub fn handle_key(buf: &mut Buffer, cur: &mut CursorPos, status: &mut Status,
                  key: KeyEvent) -> EditAction;   // 替代旧 Editor::handle_event
pub fn ensure_cursor_valid(buf: &Buffer, cur: &mut CursorPos);  // 多 space 共享 buffer 时重算 row/col（v0.2 不触发）
```

`handle_key` 是纯函数：吃 `&mut Buffer + &mut CursorPos + &mut Status + KeyEvent`，返回 `EditAction`。App 调用时把 `space.cursor` 和 `contents[id]` 的 buffer/status 传进去。

### 4.2 protocol —— 中立契约

零上层依赖。把 core 与 layout/frontend 解耦。

- **`FrontendEvent`/`KeyEvent`/`translate_key`**：沿用 v0.2。`KeyEvent::Char(u8)` 只放行 ASCII graphic + 空格的局限保留（已知债务，非本次范围）。
- **`CursorPos`**（cursor.rs，新增）：中立光标值，layout 持它而不依赖 core：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorPos { pub char_index: usize, pub row: usize, pub col: usize }
```

- **`Viewport`**（viewport.rs，新增）：视口滚动位置。**不再存 width/height**（从 rect 拿），消除 `height-1` 越权：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport { pub top_row: usize, pub left_col: usize }

impl Viewport {
    pub fn ensure_cursor_visible(&mut self, cursor_row: usize, view_height: usize);
    pub fn scroll_down(&mut self, n: usize, view_height: usize);
    pub fn scroll_up(&mut self, n: usize);
}
```

- **`EditView`**（edit_view.rs，新增）：文档投影 trait。**不含 cursor/viewport**（那些在 Space），纯文档：

```rust
pub trait EditView {
    fn line(&self, idx: usize) -> Cow<str>;
    fn len_lines(&self) -> usize;
    fn file_name(&self) -> Option<&str>;
    fn modified(&self) -> bool;
    fn status(&self) -> StatusMessage;
}
```

- **`ContentLookup`**（edit_view.rs）：按 id 查文档，App impl、Frontend 用：

```rust
pub trait ContentLookup { fn get(&self, id: ContentId) -> Option<&dyn EditView>; }
```

- **`SpaceState`/`RenderCtx`**（edit_view.rs）：渲染上下文，纯 protocol 类型（不含 `ResolvedScene`，避循环）：

```rust
#[derive(Clone, Copy)]
pub struct SpaceState { pub viewport: Viewport, pub cursor: CursorPos }

pub struct RenderCtx<'a> {
    pub contents: &'a dyn ContentLookup,
    pub focused: SpaceId,
    pub focused_state: SpaceState,   // 焦点 space 的视图状态，供 StatusBarContent 读
    pub focused_content: ContentId,
}
```

### 4.3 layout —— Space 持视图状态，引擎纯几何

- **`ids.rs`**：`SceneId`/`SpaceId`/`ContentId`，沿用 v0.2。
- **`Space`**（space.rs）：升级为带视图状态的展示实例：

```rust
pub struct Space {
    pub id: SpaceId,
    pub kind: SpaceKind,          // Container{arrangement,children} | Host{content_id}
    pub sizing: Sizing,           // Fixed(i32) | Grow(u32)
    pub layer: Layer,             // Base | Overlay | Modal | Debug（v0.2 仅 Base）
    pub viewport: Viewport,       // 视图状态：看哪部分
    pub cursor: CursorPos,        // 视图状态：编辑哪
    pub wrap_mode: WrapMode,      // 视图状态：是否折行（v0.2 预留，默认 None）
}
```

状态栏 Space 不展示文档，其 `viewport/cursor` 不被其 Content 读取。Space 视图状态设 `Option`（状态栏为 `None`）更显式，spec 倾向 `Option`，实现时定。

- **`Scene`/`SceneBuilder`**（scene.rs）：沿用 v0.2 不变量校验（无环/悬空/root）。`build_editor_scene` 产出 `root: Vertical [editor_space Grow(1), status_space Fixed(1)]`。
- **`taffy_engine`**：仍**只算 `rect`**。`viewport/cursor/wrap_mode` 是 Space 上的字段，引擎只读透传、不参与布局计算。三步沿用 v0.2：DFS 建 Taffy 节点 → `compute_layout` → DFS 收集 `RenderItem`（f32 round 成 i32 Rect，clip 传递）。
- **`resolved.rs`**：渲染项带该 space 的视图状态：

```rust
pub struct RenderItem {
    pub content_id: ContentId,
    pub rect: Rect,
    pub clip: Option<Rect>,
    pub state: SpaceState,        // 透传该 space 的 viewport + cursor
    pub layer: Layer,
    pub z_index: i32,
    pub order: u64,
}
pub struct ResolvedScene { pub items: Vec<RenderItem> }
```

`render()` 按 `(layer, z_index, order)` 排序逐项分派（v0.2 全 Base，实质 DFS 前序）。

### 4.4 frontend/tui —— Content 渲染策略 + Frontend 实现

- **`Content` trait**（content.rs）：渲染策略，依赖 `Output`/`Rect`/`RenderCtx`，放 tui 层：

```rust
pub trait Content {
    fn render(&self, ctx: &RenderCtx, state: &SpaceState,
              rect: Rect, clip: Option<Rect>, out: &mut Output) -> io::Result<()>;
}
```

- **`EditorContent`**：无自身状态。用 `state.viewport` + `state.cursor` + `ctx.contents.get(content_id)` 画可见行 + 算光标屏幕坐标。
- **`StatusBarContent`**：无自身状态。用 `ctx.focused_state.cursor` + `ctx.contents.get(ctx.focused_content)` 画状态行（文件名/modified/row:col/status）。这是"特殊的 Content"——其数据来自焦点上下文而非自身 Space。
- **`TuiFrontend`**（tui_frontend.rs）：`impl Frontend`，持真多内容注册表 + IO：

```rust
pub struct TuiFrontend {
    registry: HashMap<ContentId, Box<dyn Content>>,
    input: Input,
    output: Output<Stdout>,
    guard: TerminalGuard,
}
```

`render` 实现：构造 `RenderCtx`（含焦点 state，从 `scene` 找 `focused` space 的 state + 其 content_id）→ 遍历 `scene.items` → `registry[item.content_id].render(&ctx, &item.state, item.rect, item.clip, &mut self.output)` → flush（统一定位光标 + show_cursor，沿用 v0.2 修复）。`next_event` 委托 `Input`。

**注册表在 Frontend 不在 App**——Content 是 UI 渲染策略，归 UI 层。App 只调 `frontend.render(&contents, &scene, focused)`，不知 Content 存在。

### 4.5 app —— 事件循环 + 编排

app 层定义 `Frontend` trait（frontend 实现层实现，依赖倒置）：

```rust
pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;
    fn render(&mut self, contents: &dyn ContentLookup,
              scene: &ResolvedScene, focused: SpaceId) -> io::Result<()>;
}
```

`App` 持文档容器 + Scene + 焦点 + Frontend：

```rust
struct Document { buffer: Buffer, status: Status }   // impl EditView
struct App {
    contents: HashMap<ContentId, Document>,
    scene: Scene,
    focused: SpaceId,
    should_quit: bool,
    frontend: Box<dyn Frontend>,
}
```

`run()`：

```
render()                                          // 首帧
while !should_quit:
    ev = frontend.next_event()
    if Resize: scene.resize() + （viewport 尺寸下帧从 rect 自动更新）
    match ev:
        编辑事件(Key Char/Enter/Backspace/Arrow/Ctrl):
            let (buf, cur, status) = 焦点 space 的 cursor + 焦点 content 的 buffer/status
            action = core::handle_key(buf, cur, status, key)
            焦点 space.viewport.ensure_cursor_visible(cur.row, 上帧 rect.height)
            match action { Quit => should_quit=true, ... }
        视图事件(PageDown/PageUp/...):
            焦点 space.viewport.scroll_*(..., rect.height)
    render()
```

`render()` = `layout::layout(&scene)` → `frontend.render(&self, &resolved, focused)`。

App 不 import `TuiFrontend`/`Output`/`Content`/`Editor`（除通过 `Document` 持 `Buffer`）。

## 5. 数据流

### 5.1 事件流（输入 → 状态）

```
Frontend::next_event → FrontendEvent
  → App 分类路由：
       编辑事件 → core::handle_key(&mut buf, &mut space.cursor, &mut status, key)
                → space.viewport.ensure_cursor_visible(space.cursor.row, view_height)
       视图事件 → space.viewport.scroll_*()
```

事件链路四步：获取在 Frontend（IO）、翻译在 protocol（translate_key）、路由在 App（编辑/视图分类）、处理在 core（handle_key）。

### 5.2 渲染流（状态 → 输出）

```
ResolvedScene = layout::layout(&scene)        // 每个 item: { content_id, rect, state{viewport,cursor}, ... }
Frontend::render(&contents, &resolved_scene, focused)
  → 构造 RenderCtx（焦点 state + 焦点 content_id）
  → 遍历 items: registry[content_id].render(ctx, item.state, rect, out)
       EditorContent:    用 item.state.viewport + item.state.cursor + EditView.line() 画
       StatusBarContent: 用 ctx.focused_state.cursor + ctx.focused_content 的 EditView 画状态行
  → flush 光标
```

## 6. 关键设计决策（澄清记录）

| 决策点 | 结论 | 理由 |
|---|---|---|
| App 与前端边界 | Frontend trait 统一（render + next_event） | App 持 `dyn Frontend`，输入输出 + 终端生命周期收进 Frontend |
| 内容注册表 | 真多内容注册表 `ContentId → Box<dyn Content>`，放 Frontend | Content 是 UI 渲染策略，归 UI 层；App 不碰 Content 类型 |
| Content 与 Editor | 经中立 `EditView` trait 解耦 | Content 不 import Editor，与 protocol 解耦 crossterm 同构 |
| viewport 归属 | 归 Space | 视图状态归视图；支持一 content 多 space（分屏各自滚动） |
| cursor 归属 | 归 Space | 多视图多光标；Document/View 分离 |
| 状态栏建模 | 普通 Space + 特殊 Content | 特殊性收敛在 StatusBarContent 实现，不污染管线一致性 |
| Editor 形态 | cursor 移走，退化为 Buffer + 纯函数 | 文档状态与视图状态分离 |
| CursorPos 归属 | 放 protocol | 让 layout 持 cursor 但不依赖 core（"layout 不感知 editor"硬保证） |
| 事件处理位置 | 处理在 core（handle_key 纯函数） | 路由在 App，获取在 Frontend，处理在 core |
| 重构策略 | 一次性重写 | 架构变动大，spec 写目标架构 |

## 7. 待定项与已知复杂度

- **wrap 归属**：归 Space（与 viewport/cursor 一致）。`wrap_mode` 是 Space 字段，`EditView` 不含它。v0.2 字段预留、默认 `None`，未来实现切换。
- **多 Space 共享 buffer 的 cursor 同步**：v0.2 不触发（单编辑区）。模型支持，但 A space 插入影响 B space `char_index` 有效性需 `ensure_cursor_valid` 重算。标 future，不预付实现。
- **状态栏 Space 视图状态闲置**：状态栏 Space 持 `viewport/cursor` 但其 Content 不读。倾向 Space 视图状态设 `Option`（状态栏为 `None`），实现时定。
- **Frontend trait 归属**：放 app 层（依赖倒置），其 `render` 引用 `layout::ResolvedScene`。
- **`KeyEvent::Char(u8)` 的 Unicode 局限**：保留（非本次范围），未来扩展时再改。

## 8. scope

### 8.1 v0.2 交付（功能与现状对齐）

打开文件（NotFound→NewFile/非 UTF-8→OpenFailed）、Char/Enter/Backspace、方向键移动、Ctrl+S/Ctrl+Q、Resize、文本区 + 末行状态栏 + 光标定位、滚动跟随。

### 8.2 v0.2 架构落地（新骨架）

Frontend trait + TuiFrontend；EditView + ContentLookup；Content 注册表（EditorContent/StatusBarContent）；Space 持 viewport+cursor+wrap_mode；core 纯函数（handle_key）；layout 不依赖 core；CursorPos/Viewport 提到 protocol。

### 8.3 v0.2 不实现（骨架预留）

多 space 分屏交互、多 cursor 同步、wrap 切换、水平滚动（left_col 固定 0）、多层 Layer/z_index（全 Base）、Terminal/Tree/Inspector 等内容类型。

## 9. 测试策略

一次性重写：旧 84 测试随 API 变化重写，行为层测试思路保留（打开/编辑/保存/resize 全流程）用新 API 重写。

| 层 | 测试 | 关键可测抽象 |
|---|---|---|
| core | Buffer IO + `handle_key` 纯函数（操作 buffer+cursor） | 纯函数，无 IO |
| protocol | translate_key、CursorPos/Viewport 值类型、EditView mock | EditView 可 mock |
| layout | Scene/Space 几何 + Space 持视图状态 + ResolvedScene 透传 viewport/cursor | 不需终端 |
| frontend | Content 渲染用 `Output<Vec<u8>>` 断言 VT 字节；TuiFrontend 用 `Output<Vec<u8>>` | `Output<Vec<u8>>` |
| app | `ScriptedInput` + mock Frontend（`RecordingFrontend`）驱动集成测试 | InputSource + Frontend trait |

可测试性四个关键抽象：`InputSource`（输入可脚本驱动）、`Frontend` trait（可用 mock 替换 TuiFrontend）、`Output<Vec<u8>>`（输出可字节断言）、`EditView`（文档投影可 mock）。

## 10. 消失的旧物

| 旧物 | 处置 | 原因 |
|---|---|---|
| `ContentStore` | 删除 | 状态持有/路由/分派三职责拆归 App+Space+Frontend |
| `Editor` 持 cursor | 拆分 | cursor 移 Space，Editor 拆成 Buffer + 纯函数 |
| `CorePatch`/`PatchList` | 删除 | 死协议，YAGNI |
| `Viewport` 住 `tui/` + `height-1` | 移 protocol + 尺寸从 rect 拿 | 越权消除 |
| `ContentKind` 二分路由 | 删除 | 注册表直接按 id 查 Content |
| `EditorState` | 删除 | 状态归 App(Document) + Space |
