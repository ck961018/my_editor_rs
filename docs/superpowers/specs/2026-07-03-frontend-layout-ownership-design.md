# 前端布局所有权下放——设计规格

> 日期：2026-07-03
> 状态：已确认，待写实现计划
> 关联：`docs/superpowers/specs/2026-07-02-emacs-keymap-dispatch-design.md`（前置重构，已落地 commit ca19487）
> 对照：`docs/design/current-architecture.md`（当前架构事实描述）

## 1. 背景与动机

当前架构里 `TaffyEngine` 在 `app/mod.rs`（`App.engine` 字段），每帧 `render()` 流程：

```
TaffyEngine.layout(scene) → ResolvedScene { items: RenderItem{ content_id, rect, clip, state, ... } }
  ├─ 用 item.rect.height 调 viewport.ensure_cursor_visible    ← core 依赖 layout 结果
  └─ build_frame(resolved, contents)
       └─ content.render(ctx with rect_height) → FrameContent::Editor{lines} / StatusBar{...}  ← core 依赖 rect.height 切行
  → frontend.render(&frame)   ← 前端只按 rect 绘制已渲染好的 FrameContent
```

问题：

1. **`Rect` 这种 tui 几何在后端产出**。`RenderItem.rect` / `ResolvedScene` 是 tui 特定的布局结果，却由 app（后端）算出。若未来加 GUI 前端，这套 tui 几何对 GUI 无意义——后端不该决定前端渲染几何。
2. **`content.render` 依赖 `rect.height`**。core 把内容渲染成 `FrameContent::Editor { lines }`（按高度切行），导致 layout 必须先于 render 在后端完成，前后端职责纠缠。
3. **viewport 在后端**。`App::render` 用 layout 出的 `rect.height` 调 `ensure_cursor_visible`，viewport 存 core 的 `Space` 里。layout 一旦下放，后端拿不到 rect.height，viewport 成为最硬的耦合点。
4. **每帧全量 push Frame**。`build_frame` 每帧把所有 content 的已渲染内容打包成 `Frame` 推给前端，数据重（含全部可见行文本），且前端无自治权。

目标（借鉴 Helix）：

1. **后端不感知 tui 几何**：`Rect`/`ResolvedScene`/`RenderItem` 从后端消失，改由前端 layout 产出。后端不再持 `TaffyEngine`。
2. **后端不感知 viewport**：viewport 完全下放前端，后端只暴露 cursor 逻辑位置查询。
3. **前端 pull 内容**：前端 layout 出 rect 后，按可见范围向后端查询文本行/状态栏数据，前端自行切行/格式化/paint。后端→前端不再 push 已渲染内容。
4. **数据轻量**：前端只拉可见行（rect.height 行），不搬整个 buffer；不引入跨进程序列化复杂度。
5. **Scene 布局意图协议化**：`Scene`/`Space` 等纯数据布局意图类型下沉 `protocol/`，前后端共享，无镜像无转换。

## 2. 工业对照

| 编辑器 | 进程模型 | 前端访问内容 | 几何归属 | 失效/优化 |
|---|---|---|---|---|
| Zed | 单进程 | 直接引用 rope slice | GPUI flex（前端） | 每帧重建 element tree + 内部 diff |
| Helix | 单进程 | 直接读 document rope | compositor + tui layout（前端） | 每帧重读 + frame diff（cell grid 比对） |
| Neovim | 真 C/S（msgpack-RPC） | push 已渲染 grid_line 事件 | 核心算 grid | 核心 grid diff 后推变化 cell |

结论：**Helix/Zed 单进程下"前端 pull"= 同进程函数调用读 rope**，不是 C/S 查询接口；**Neovim 的 grid push 只在跨进程时才值得**。本项目单二进制同进程，走 Helix 路线：前端持 layout engine + viewport，通过 `ContentQuery` trait（同进程函数调用）pull 可见内容，不序列化、不跨进程。

## 3. 模块布局

### protocol——纯数据布局意图下沉 + 查询契约

| 文件 | 变更 | 内容 |
|---|---|---|
| `protocol/scene.rs` | **新建**（从 `layout/scene.rs` 移入） | `Scene`/`SpaceNode`/`SceneBuilder`/`BuildError` |
| `protocol/space.rs` | **新建**（从 `layout/space.rs` 移入） | `Space`/`SpaceKind`/`Arrangement`/`Axis`/`Sizing`/`Align`/`Layer` |
| `protocol/geometry.rs` | **新建**（从 `layout/scene.rs` 拆出） | `Size`/`Rect`/`Point` + `Rect::intersect` |
| `protocol/ids.rs` | 改 | 合并 `layout/ids.rs`（`SpaceId`/`ContentId` 统一到 protocol） |
| `protocol/content_query.rs` | **新建** | `ContentQuery` trait + `RowRange` + `StatusBarData` |
| `protocol/cursor.rs` | 不变 | `CursorPos` |
| `protocol/viewport.rs` | 改 | `Viewport` 留 protocol（前端用），但后端不再持；`ensure_cursor_visible` 逻辑移前端 |
| `protocol/status.rs` | 不变 | `StatusMessage` |
| `protocol/key_event.rs` / `frontend_event.rs` | 不变 | |
| `protocol/frame.rs` | **删除** | `Frame`/`FrameItem`/`FrameContent` 不再需要（前端不再消费中性 Frame） |
| `protocol/edit_view.rs` | **删除** | `SpaceState`（viewport+cursor 透传）随 viewport 下放而消失 |

### Space 字段瘦身

`Space` 当前字段：`{ id, kind, sizing, layer, viewport, cursors, wrap_mode }`。

移除 `viewport`（下放前端）、`cursors`（cursor 走 query）、`wrap_mode`（前端渲染参数）。瘦身后：

```rust
pub struct Space {
    pub id: SpaceId,
    pub kind: SpaceKind,
    pub sizing: Sizing,
    pub layer: Layer,
}
```

`SpaceKind::{ Container{arrangement, children}, Host{content} }` 不变。

### core——领域逻辑

| 文件 | 变更 | 说明 |
|---|---|---|
| `core/content.rs` | 改 | 删 `RenderCtx`/`ContentHandler::render`/`ContentHandler::line`/`len_lines`/`file_name`/`modified`/`status` 等渲染访问器；`ContentHandler` 仅留分发契约（`keymap`/`keymap_mut`/`default_binding`/`buffer_mut`）。`ContentLookup` **保留**（dispatcher 查 keymap 用）。`Cursors` **保留**（buffer 编辑原语 `insert_at_cursors`/`delete_at_cursors` 用），但从 `Space` 字段移除，cursor 归 App 层（见 §4.5）。`WrapMode`/`SpaceState` 删 |
| `core/buffer.rs` | 改 | `Buffer` 不再 impl `render`；保留文本查询原语（`line(idx) -> Cow<str>`/`len_lines()`/`file_name()`/`modified()`/`status()`）供 `ContentQuery` impl 复用；编辑原语 `insert_at_cursors`/`delete_at_cursors`/`move_cursor_*` 接 `&mut Cursors`（签名不变，Cursors 由 App 传入） |
| `core/status_bar.rs` | 改 | `StatusBar` 不再 impl `render`；保留 `target_content_id` + 暴露 `status_bar_data(target: &dyn ContentLookup) -> StatusBarData` 供 `ContentQuery` impl 复用 |
| `core/operation.rs` / `core/keymap.rs` | 不变 | |

> `ContentHandler` 与 `ContentQuery` 是两个正交契约：前者是"分发契约"（keymap 查表），后者是"查询契约"（内容读取）。`Buffer` 同时 impl 两者；`StatusBar` impl 前者，其查询数据由 app 层 `ContentQuery` impl 委托产生。

### frame/——删除

`frame/mod.rs`（`build_frame`）整体删除。后端不再产 Frame。

### layout/——层消失

`layout/` 整层删除：

- `layout/scene.rs` → 移 `protocol/scene.rs`
- `layout/space.rs` → 移 `protocol/space.rs`
- `layout/ids.rs` → 合并 `protocol/ids.rs`
- `layout/taffy_engine.rs` → 移 `tui/taffy_engine.rs`（前端内部）
- `layout/resolved.rs` → 移 `tui/resolved.rs`（前端内部 layout 产出，不进 protocol）
- `layout/mod.rs` → 删除

### tui/——前端自治

| 文件 | 变更 | 职责 |
|---|---|---|
| `tui/scene_renderer.rs` | **新建** | 核心：持 `TaffyEngine` + per-space `Viewport` 缓存；`render(scene, query, focused, canvas)` = layout → ensure viewport（用 query.cursor + rect.height）→ pull 可见行（query.lines）→ 逐行画到 `Canvas` |
| `tui/taffy_engine.rs` | 移入 | `TaffyEngine`（从 layout 移来，逻辑不变） |
| `tui/resolved.rs` | 移入 | `ResolvedScene`/`RenderItem`（前端内部类型，含 Rect） |
| `tui/tui_frontend.rs` | 改 | `TuiFrontend<W>` = `SceneRenderer` + `Output<W>`；`Frontend::render` 调 `SceneRenderer::render(..., &mut output)` |
| `tui/headless.rs` | **新建**（从 `app/frontend.rs` 拆出） | `HeadlessFrontend` = `SceneRenderer` + `Output<Vec<u8>>`；捕获每帧 VT 字节供测试断言 |

### app——编排

| 文件 | 变更 | 职责 |
|---|---|---|
| `app/mod.rs` | 改 | 删 `engine: TaffyEngine` 字段；`render()` 不再 layout/build_frame，改为 `frontend.render(&scene, &query, focused)`；删 viewport 跟随逻辑（移前端）；`App` impl `ContentQuery`（委托 `contents` 里的 `Buffer`/`StatusBar`） |
| `app/content.rs` | 改 | `ContentLookup` impl **保留**（dispatcher 用）；新增 `HashMap<ContentId, Box<dyn ContentHandler>>` 的 `ContentQuery` impl（委托 Buffer/StatusBar 固有方法 + App cursors map） |
| `app/frontend.rs` | 改 | `Frontend` trait：`render(&mut self, scene: &Scene, query: &dyn ContentQuery, focused: SpaceId)`；`FrontendImpl` 枚举不变（Tui/Headless）；`HeadlessFrontend` 移 `tui/headless.rs` |
| `app/dispatcher.rs` / `app/executor.rs` | 不变 | |

### 依赖方向

```
protocol ← core ← app ← main
    ↑            ↑
    └── tui ─────┘   (tui 依赖 protocol + core；并 impl app::Frontend trait——依赖倒置)
```

- `protocol` 零依赖中立层（含 Scene/Space/geometry/ids/ContentQuery 契约）。
- `core` 依赖 `protocol`。
- `tui` 依赖 `protocol`（Scene/ContentQuery）+ `core`（Buffer 查询原语）+ `terminal`（Canvas）+ `taffy`，并 impl `app::Frontend` trait。
- `app` 依赖 `protocol`/`core`，定义 `Frontend` trait 但不 import `tui`（依赖倒置：trait 在 app，实现在 tui；`FrontendImpl` 枚举的 `Tui` 变体才在构造期接触 `TuiFrontend`，与现状一致）。
- `taffy` 依赖从 app 移到 tui。

## 4. 核心类型

### 4.1 ContentQuery（`protocol/content_query.rs`）

前端 pull 后端内容的契约。同进程函数调用，返回 owned 数据（v0.2 不追零拷贝）。

```rust
use crate::protocol::cursor::CursorPos;
use crate::protocol::ids::ContentId;
use crate::protocol::status::StatusMessage;

/// 行范围 [start, end)，前端按可见行拉取。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowRange { pub start: usize, pub end: usize }

/// 状态栏显示数据（owned）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusBarData {
    pub file_name: Option<String>,
    pub modified: bool,
    pub message: StatusMessage,
}

/// 前端查询后端内容的契约。同进程同步调用。
pub trait ContentQuery {
    /// 拉指定行范围的文本（[start, end)）。返回 Vec 长度 = min(range.len(), line_count - start)；
    /// 超出末尾的行不返回（前端按 rect.height 拉但实际行数可能更少，缺位画空行）。
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String>;
    /// 状态栏数据。
    fn status_bar(&self, cid: ContentId) -> StatusBarData;
    /// 焦点光标逻辑位置（前端算屏坐标 + viewport 跟随用）。
    fn cursor(&self, cid: ContentId) -> CursorPos;
    /// 总行数（前端算滚动边界用）。
    fn line_count(&self, cid: ContentId) -> usize;
}
```

`App` impl `ContentQuery`：按 `cid` 分派到 `contents` 里的 `Buffer`（lines/cursor/line_count）或 `StatusBar`（status_bar 委托 target content）。

### 4.2 Frontend trait（`app/frontend.rs`）

```rust
pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;
    fn render(&mut self, scene: &Scene, query: &dyn ContentQuery, focused: SpaceId) -> io::Result<()>;
}
```

不再传 `&Frame`。前端拿到 Scene（布局意图）+ query（内容 pull）+ focused（焦点 space），自己 layout + pull + paint。

### 4.3 SceneRenderer（`tui/scene_renderer.rs`）

前端核心，TuiFrontend 与 HeadlessFrontend 共用。

```rust
pub struct SceneRenderer {
    engine: TaffyEngine,
    viewports: HashMap<SpaceId, Viewport>,  // per-space viewport 缓存
}

impl SceneRenderer {
    pub fn new() -> Self { ... }

    pub fn render(
        &mut self,
        scene: &Scene,
        query: &dyn ContentQuery,
        focused: SpaceId,
        canvas: &mut dyn Canvas,
    ) -> io::Result<()> {
        // 1. TaffyEngine.layout(scene) → ResolvedScene（前端内部，含 Rect）
        // 2. 对 focused space：query.cursor(focused_cid) → ensure_cursor_visible(cursor.row, rect.height) 调整 viewport
        // 3. 对每个 Host item：query.lines(cid, top_row..top_row+rect.height) → 逐行画到 canvas（按 rect 偏移）
        // 4. 焦点光标屏坐标 = (rect.y + cursor.row - viewport.top_row, rect.x + cursor.col - viewport.left_col) → canvas.move_cursor + show_cursor
    }
}
```

### 4.4 viewport 跟随（前端）

`Viewport::ensure_cursor_visible(cursor_row, height)` 逻辑从 core 移到 `tui/scene_renderer.rs`（或 `protocol/viewport.rs` 保留方法，前端调）。`Viewport::new` 不再 `-1`（§6.4 债务清理——状态栏高度由 layout 算出，viewport 只管自己 rect 的高度）。

首次渲染时 `SceneRenderer.viewports` 中对应 space 尚无 viewport，按该 space 的 `rect.height` 用 `Viewport::new(rect.height)` 初始化并插入缓存；后续帧复用缓存实例，仅在 rect.height 变化（resize）时重建。

### 4.5 cursor 归属（App 层）

`Space` 移除 `cursors` 字段后，cursor 需要新归宿。**放在 App 层**：

```rust
// app/mod.rs
pub struct App {
    contents: HashMap<ContentId, Box<dyn ContentHandler>>,
    cursors: HashMap<ContentId, Cursors>,   // 每个 Host content 一个编辑会话 cursor
    scene: Scene,
    ...
}
```

- `App::new` 为每个 Host content 初始化 `Cursors::single(CursorPos::origin())`。
- `executor::execute(op, content, cursors: &mut Cursors)`——签名从接 `&mut Space` 改为接 `&mut Cursors`，不再触碰 Space。
- `ContentQuery::cursor(cid)` 查 `self.cursors[&cid].primary`。
- 焦点 cursor = `cursors[focused_content_id].primary`，传给前端经 `ContentQuery::cursor`。

理由：cursor 是"编辑会话状态"（哪个 content 在哪），不属于 buffer 文本（Buffer 保持纯文本模型），也不属于布局（Space 只描述几何意图）。放 App 层最简，不动 Buffer/Space 内部结构。`Cursors` 类型保留在 `core/content.rs`（buffer 编辑原语 `insert_at_cursors`/`delete_at_cursors` 签名需要）。

### 4.6 删除 Operation::ViewportScrollBy

`Operation::ViewportScrollBy { lines }` 当前无键绑定构造（死代码），且 viewport 下放前端后后端 executor 不再该碰 viewport。删掉该变体 + executor 对应分支。未来滚动由前端 viewport 自治或专门的前端事件处理。

## 5. 数据流

### 5.1 事件流（输入 → 状态，不变）

```
crossterm EventStream → Input → FrontendEvent → App::handle_event
  → Dispatcher.dispatch → Operation → executor.execute（改 buffer/cursor，不调 viewport）
```

executor 改完 cursor 就完事，**不再调 `ensure_cursor_visible`**——viewport 跟随是前端渲染时的事。

### 5.2 渲染流（状态 → 输出，重写）

```
App::render:
  frontend.render(&scene, &query, focused)
    → SceneRenderer.render:
        ├─ TaffyEngine.layout(scene) → ResolvedScene { items: Rect, ... }   ← 前端算几何
        ├─ query.cursor(focused_cid) → ensure_cursor_visible                 ← 前端管 viewport
        ├─ query.lines(cid, top_row..top_row+rect.height) → Vec<String>      ← 前端 pull 可见行
        └─ 逐行画到 Canvas（crossterm queue / 内存 Vec）+ 光标定位
```

后端每帧只传 `&Scene`（布局意图，轻）+ `&dyn ContentQuery`（查询句柄，不传数据）。内容数据由前端按需 pull，只拉可见行。

### 5.3 失效策略

**v0.2 每帧重查重画，无缓存无变更通知**（Helix frame diff 的简化版）。理由：

- 同进程函数调用读内存很便宜，80×24 终端每帧 pull ~23 行 ≈ 1.8KB，可忽略。
- v0.2 content 数量少（editor + status），每帧全量重画到 Canvas（`clear_line` + `write_str`）与当前模式一致，无回归风险。
- frame diff（cell grid 比对，只写变化 cell）留 v0.3 作为渲染优化，不阻塞本次架构 correctness。
- 不引入变更通知通道/版本号缓存——YAGNI，避免过度设计（current-architecture §6.5 批评过预留抽象）。

## 6. 关键决策记录

| 决策 | 选择 | 理由 |
|---|---|---|
| 数据形态 | 后端发原始内容数据（路线 A） | 彻底解耦 rect 回传；content.render/FrameContent 模型拆除 |
| 协议形态 | 同进程 Rust struct，不序列化 | 单二进制，"协议"= 中性类型层；跨进程留远期 |
| 内容传输 | 前端 pull（Helix 式） | 工业验证；同进程下 pull = 函数调用；不传重数据 |
| viewport 归属 | 完全下放前端 | 后端不感知几何，cursor 走 query；避免双向回传 |
| 失效机制 | 每帧重查重画（选项 B） | 同进程读内存便宜；frame diff/版本号缓存留 v0.3 |
| Scene 协议化 | 纯数据类型下沉 protocol（路线 2） | 避免镜像转换；本就是前后端共享数据 |
| ContentQuery 返回 | owned `Vec<String>` | 1.8KB/帧可忽略；owned 让前端处理简单；零拷贝留 v0.3 |
| Scene 增量 | v0.2 全量重发 | 结构静态，只有 size 变；细粒度 diff 留 v0.3 多面板时 |
| `taffy` 依赖位置 | 移到 tui | 布局算法是前端实现细节 |
| cursor 归属 | App 层 `HashMap<ContentId, Cursors>` | Space 删 cursors 后需归宿；放 App 不动 Buffer/Space 结构；cursor 是编辑会话状态 |
| `Operation::ViewportScrollBy` | 删 | 死代码 + viewport 下放前端后端不该碰 |

## 7. 测试策略

### 7.1 HeadlessFrontend 捕获

`HeadlessFrontend` 持 `Output<Vec<u8>>` 作 Canvas，每帧 `render` 后捕获 VT 字节快照。测试断言字节内容（含文本/状态栏/光标序列），与当前 `tui_frontend.rs` 测试模式一致。

### 7.2 测试分层

- **protocol**：Scene/Space/geometry/ids/ContentQuery 类型的构造与不变量（SceneBuilder 校验、Rect::intersect 等，从 layout 测试搬来）。
- **core**：Buffer/StatusBar 文本查询原语（line/len_lines/status_bar_data），删 render 相关测试。
- **tui**：
  - `taffy_engine` 几何测试（从 layout 搬来，断言 Grow/Fixed 分配、resize）。
  - `scene_renderer` 集成测试：用 stub `ContentQuery`（返回固定 lines/cursor）+ `Output<Vec<u8>>`，断言 VT 输出含正确可见行 + 光标屏坐标 + viewport 跟随（cursor 超出底部时 top_row 调整）。
  - `headless` 捕获测试：驱动若干 FrontendEvent，断言捕获的帧字节。
- **app**：
  - `ContentQuery` impl 测试：lines/cursor/line_count/status_bar 分派正确。
  - 集成测试（沿用现有 `make_app` 模式）：插入/退退格/箭头/保存/resize 全流程，断言走 HeadlessFrontend 捕获的字节而非 Frame.items（现有测试找 `frame.items` 的断言改写为找字节）。
  - viewport 跟随回归：cursor 移出底部时，下一帧捕获的字节反映 top_row 滚动。

### 7.3 测试迁移

现有测试中依赖 `Frame`/`FrameItem`/`FrameContent`/`frame.items` 断言的（`app/mod.rs` 5 处、`frame/mod.rs`、`app/frontend.rs`）全部改写为字节级断言（经 HeadlessFrontend 捕获）或 `ContentQuery` 直查。`status_bar_renders_focused_buffer_info` 等改用 `ContentQuery::status_bar` 断言数据正确性 + 字节断言渲染正确性分离。

## 8. 迁移影响清单

### 删除

- `protocol/frame.rs`（Frame/FrameItem/FrameContent）
- `protocol/edit_view.rs`（SpaceState）
- `frame/mod.rs`（build_frame）+ `frame/` 目录
- `layout/` 整层（mod.rs/scene.rs/space.rs/ids.rs/taffy_engine.rs/resolved.rs）——内容移走后删空
- `core/content.rs` 中 `RenderCtx`/`ContentLookup`/`render`/`Cursors`/`WrapMode`/`SpaceState` 引用
- `App.engine` 字段 + `App::render` 中的 layout/build_frame/viewport 跟随代码

### 移动

- `layout/scene.rs` → `protocol/scene.rs`（剥 Viewport/Cursors/WrapMode 字段引用）
- `layout/space.rs` → `protocol/space.rs`
- `layout/ids.rs` → 合入 `protocol/ids.rs`
- `layout/taffy_engine.rs` → `tui/taffy_engine.rs`
- `layout/resolved.rs` → `tui/resolved.rs`
- `app/frontend.rs` 的 `HeadlessFrontend` → `tui/headless.rs`

### 新建

- `protocol/content_query.rs`（ContentQuery/RowRange/StatusBarData）
- `protocol/geometry.rs`（Size/Rect/Point）
- `tui/scene_renderer.rs`（SceneRenderer）
- `tui/headless.rs`（HeadlessFrontend）

### 改写

- `core/content.rs`：ContentHandler 瘦身为分发契约（删渲染访问器 + RenderCtx），保留 ContentLookup/Cursors
- `core/buffer.rs` / `core/status_bar.rs`：删 render impl，保留查询原语；StatusBar 加 `status_bar_data(&dyn ContentLookup) -> StatusBarData`
- `core/operation.rs`：删 `ViewportScrollBy` 变体 + Direction（若仍无消费者）
- `app/mod.rs`：删 `engine` 字段；加 `cursors: HashMap<ContentId, Cursors>` 字段；impl `ContentQuery`；`render` 改调 `frontend.render(&scene, &query, focused)`；删 viewport 跟随代码
- `app/executor.rs`：签名改 `execute(op, content, cursors: &mut Cursors)`；删 `ViewportScrollBy` 分支
- `app/frontend.rs`：`Frontend::render` 签名改 `(&mut self, scene: &Scene, query: &dyn ContentQuery, focused: SpaceId)`
- `tui/tui_frontend.rs`：改用 SceneRenderer
- `main.rs`：不变（FrontendImpl::Tui 构造不变）

## 9. Non-goals / Follow-up

- **frame diff（cell grid 比对）**：v0.3 渲染优化，本次不做。
- **零拷贝 pull（Cow/RopeSlice）**：v0.3 性能有压力再做。
- **Scene 结构细粒度增量 diff（space 增删改协议）**：v0.3 多面板/弹层时再加，v0.2 全量重发。
- **跨进程 C/S（序列化协议）**：远期，本次明确不做。
- **软换行（WrapMode::Soft）**：仍预留，前端渲染参数，本次不实现。
- **多光标 secondaries 渲染**：v0.2 secondaries 仍空，viewport/cursor 跟随只针对 primary。

## 10. 风险

1. **测试改写量大**：现有 5 处 app 集成测试 + frame/headless 测试依赖 Frame 结构，全部改字节级断言。需逐个核对断言语义不丢失（尤其 status_bar_renders_focused_buffer_info）。
2. **viewport 跟随语义迁移**：core→前端，`Viewport::new` 的 `-1` 债务清理后，确保 cursor 跟随行为与现状一致（不回归）。需回归测试守护。
3. **ContentQuery impl 分派**：`StatusBar` 的 status_bar_data 要查 target content，链式查询需正确传 target cid。
4. **依赖倒置边界**：`Frontend` trait 在 app，`SceneRenderer` 在 tui，确保 app 不 import tui（当前 `app/frontend.rs` 用 `crate::tui::tui_frontend::TuiFrontend` 是已有边界，保持）。
