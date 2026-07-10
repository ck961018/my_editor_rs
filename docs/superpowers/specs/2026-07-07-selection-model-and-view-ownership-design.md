# Selection 模型 + View 实体归属——设计规格

> 日期：2026-07-07
> 状态：已确认，待写实现计划
> 前置：`docs/superpowers/specs/2026-07-03-frontend-layout-ownership-design.md`（前端布局下放 + pull 模型，已落地 commit b6b250c）
> 对照：`docs/design/current-architecture.md`（当前架构事实描述）

## 1. 背景与动机

前置重构（2026-07-03）把布局/渲染所有权下放前端、改 Helix 式 pull 模型后，cursor 落在 `App.cursors: HashMap<ContentId, Cursors>`。当前 cursor 模型有两个问题：

1. **cursor 是点，不是 selection**。`CursorPos { char_index, row, col }` 表达不了选区（anchor + head 区间），`Cursors { primary, secondaries }` 是"多点 cursor"而非"多选区"。Zed/Helix 等现代多光标编辑器的理念是：**cursor 是 selection 的退化形态**（空 selection，即 anchor==head，head 即光标位置）。当前模型离这套理念差一截——没有 anchor，编辑原语按点操作，未来选区编辑（建选区、按 selection 删除/替换、多光标交互）要重写。

2. **cursor 按 ContentId 索引，无法支持多视图**。同一个 content（如一个 buffer）若在多个 space 显示，每个 space 应有独立 cursor/selection。当前 `HashMap<ContentId, Cursors>` 在这里会崩——两个 space 共享一个 cursor。虽然 v0.2 是单视图，但架构上应按视图（SpaceId）索引，为多视图铺路。同时 cursor 作为"编辑会话态"裸挂在 App struct 上，缺乏内聚归属——viewport 已下放前端 per-space 缓存，cursor/selection 同属"视图实例的态"，应有更内聚的实体承载。

目标（借鉴 Helix/Zed）：

1. **建立 selection 模型**：`Selection { anchor, head }`（区间），`Selections { ranges, primary_index }`（Helix 风集合）。cursor = collapsed selection（anchor==head），head 即光标位置。
2. **v0.2 退化形态占位**：所有 selection 恒 collapsed（anchor==head），不实现真选区编辑。模型就位，选区编辑留 v0.3。
3. **selection 与 View 实体绑定**：引入 `View { content, selections }` 编辑会话实体，按 SpaceId 索引（`HashMap<SpaceId, View>`）。selection 不裸挂 App，有内聚归属。
4. **ContentQuery 返回 Selections**：前端拉完整 selections（未来画选区高亮），cursor 查询维度从 ContentId 改 SpaceId。

## 2. 工业对照

| 编辑器 | selection 类型 | 容器表示 | primary | 归属 |
|---|---|---|---|---|
| Helix | `Range { from, to, direction }` | `Selection { ranges: Vec<Range>, primary_index }` | index | Document（按 ViewId 索引） |
| Zed | `Selection { start, end, reversed }` | `Vec<Selection>` + 焦点标记 | 焦点 | Editor（view 层） |
| Neovim | `pos_T`（点） | window 单 cursor | — | Window |

结论：Helix 的 `Selection` 是集合类型（`ranges: Vec<Range> + primary_index`），primary 是 index 不是独立字段，ranges 恒有序（编辑后 normalize = sort + merge）。这为多光标编辑原语（sort/merge/多 match 选择）而生。Zed 用 `Vec<Selection>` + 焦点标记，primary 概念较弱。

本项目选 **Helix 风**（`ranges: Vec<Selection> + primary_index`）：
- 统一有序 Vec，未来多光标 normalize 自然（对 Vec 操作 + 调 index）。
- primary 是 index，多光标增删时 primary 跟着 range 走（不特殊处理）。
- v0.2 `ranges.len()==1`、`primary_index==0`，index 机制虽冗余但就位，v0.3 多光标时不重构结构。

但 **`Selection` 元素用 `anchor/head` 而非 Helix 的 `from/to+direction`**：anchor/head 直接是语义端点，方向隐含在大小关系（head>anchor=forward），不需单独 direction 字段。sort 时取 `min(anchor,head)`。表达更直接，v0.2 collapsed 时 anchor==head 无歧义。

归属选 **后端 View 实体**（非前端）：selection 是编辑态（驱动 insert/delete + 被输入事件改），当前架构输入处理在后端（App→Dispatcher→Executor），selection 必须后端 executor 可达。放前端会破坏"前端纯渲染 pull"边界（executor 改 selection 要跨前后端）。Zed 能放前端 Editor 是因为其 Editor 是 view+输入+buffer 合一；本项目后端处理输入，故 selection 留后端，挂 View 实体。

## 3. 模块布局

### protocol——selection 数据模型下沉

| 文件 | 变更 | 内容 |
|---|---|---|
| `protocol/selection.rs` | **新建**（由 `protocol/cursor.rs` 改名 + 扩展） | `CursorPos`（保留）+ `Selection` + `Selections` |
| `protocol/cursor.rs` | **删除** | 内容移入 `selection.rs` |
| `protocol/content_query.rs` | 改 | `ContentQuery::cursor(cid)->CursorPos` → `selections(sid: SpaceId)->Selections`；`RowRange`/`StatusBarData` 不变 |
| 其余 protocol 文件 | 不变 | `space.rs`/`scene.rs`/`geometry.rs`/`ids.rs`/`status.rs`/`key_event.rs`/`frontend_event.rs`/`viewport.rs` |

### core——编辑原语升级

| 文件 | 变更 | 说明 |
|---|---|---|
| `core/content.rs` | 改 | `Cursors` 类型移除（定义已在 `protocol/selection.rs` 的 `Selections`）；`ContentHandler`/`ContentLookup` 不变 |
| `core/buffer.rs` | 改 | 编辑原语签名 `CursorPos→Selection` / `Cursors→Selections`；底层 `move_cursor_*`/`set_cursor` 降级 `pub(crate)` 作实现细节；新增 `move_selection_*`/`set_selection`/`insert_at_selections`/`delete_at_selections`/`recompute_selection`；import `cursor→selection` |
| `core/operation.rs` / `core/keymap.rs` / `core/status_bar.rs` | 不变 | Operation 枚举保留 `Cursor*` 前缀（用户面术语） |

### app——View 实体 + 编排

| 文件 | 变更 | 职责 |
|---|---|---|
| `app/view.rs` | **新建** | `View { content, selections }` 编辑会话实体；按 SpaceId 索引 |
| `app/mod.rs` | 改 | 删 `cursors` 字段；加 `views: HashMap<SpaceId, View>`；`App::new` 遍历 scene Host space 建 View；`focused_content_id` 收进 View；`execute_operation` 取 `view.selections_mut()`；`AppQuery` 改 `selections(sid)`；`App impl ContentQuery` 的 `cursor` 方法改 `selections` |
| `app/executor.rs` | 改 | 签名 `cursors: &mut Cursors` → `selections: &mut Selections`；调 `move_selection_*`/`insert_at_selections`/`delete_at_selections` |
| `app/dispatcher.rs` / `app/content.rs` | 不变 | ContentLookup 按 cid 查 content，不涉 selection |

### tui——跟随 ContentQuery 签名

| 文件 | 变更 | 说明 |
|---|---|---|
| `tui/scene_renderer.rs` | 改 | `query.cursor(focused_cid)` → `query.selections(focused).primary().head()` |
| `tui/headless.rs` / `tui/tui_frontend.rs` | 不变 | 经 SceneRenderer，无结构变化 |

### 依赖方向

不变（沿用前置重构）：

```
protocol ← core ← app ← main
    ↑            ↑
    └── tui ─────┘
```

`Selection`/`Selections` 在 protocol（前后端共享）；编辑原语在 core（操作 Selection）；View 实体在 app（编辑会话编排）；tui 经 ContentQuery pull selections 渲染。

## 4. 核心类型

### 4.1 Selection / Selections（`protocol/selection.rs`）

```rust
/// 光标位置值类型。char_index 权威，row/col 派生缓存（由 core::buffer 维护）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub char_index: usize,
    pub row: usize,
    pub col: usize,
}

impl CursorPos {
    pub const fn origin() -> Self { Self { char_index: 0, row: 0, col: 0 } }
}

/// 选区：anchor 为选择起点，head 为光标位置（驱动编辑/渲染）。
/// 空 selection（cursor 退化形态）：anchor == head。
/// 方向隐含：head > anchor = forward，head < anchor = backward。不加 direction 字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CursorPos,
    pub head: CursorPos,
}

impl Selection {
    pub fn collapsed(at: CursorPos) -> Self { Self { anchor: at, head: at } }
    pub fn is_empty(&self) -> bool { self.anchor == self.head }
    pub fn head(&self) -> CursorPos { self.head }
}

/// 多选区容器（Helix 风）：统一有序 Vec + primary index。
/// ranges 恒按 head.char_index 升序（v0.2 单元素，约定在；v0.3 normalize 保证）。
/// v0.2 不变量：ranges.len()==1、primary_index==0、所有 Selection collapsed。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selections {
    ranges: Vec<Selection>,
    primary_index: usize,
}

impl Selections {
    pub fn single(sel: Selection) -> Self { Self { ranges: vec![sel], primary_index: 0 } }

    pub fn primary(&self) -> &Selection { &self.ranges[self.primary_index] }
    pub fn primary_mut(&mut self) -> &mut Selection { &mut self.ranges[self.primary_index] }
    pub fn all(&self) -> impl Iterator<Item = &Selection> { self.ranges.iter() }
    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut Selection> { self.ranges.iter_mut() }

    /// 清除 secondary ranges，仅保留 primary（v0.2 noop：ranges 本就 len==1）。
    /// v0.3 多光标时由 CursorMoveTo 等调用以放弃多光标。
    pub fn retain_primary(&mut self) {
        let primary = self.ranges[self.primary_index];
        self.ranges = vec![primary];
        self.primary_index = 0;
    }
}
```

### 4.2 View 实体（`app/view.rs`）

视图实例的编辑会话：绑定一个 content + 持选区。按 SpaceId 索引（`App.views`），同 content 可被多个 View 绑定（多视图铺路）。

```rust
use crate::protocol::ids::ContentId;
use crate::protocol::selection::{CursorPos, Selection, Selections};

pub struct View {
    content: ContentId,        // 此视图绑定的 content（创建时从 scene Host{content} 读）
    selections: Selections,    // 编辑会话态（v0.2 退化：单 collapsed selection）
}

impl View {
    pub fn new(content: ContentId) -> Self {
        Self {
            content,
            selections: Selections::single(Selection::collapsed(CursorPos::origin())),
        }
    }
    pub fn content(&self) -> ContentId { self.content }
    pub fn selections(&self) -> &Selections { &self.selections }
    pub fn selections_mut(&mut self) -> &mut Selections { &mut self.selections }
}
```

### 4.3 App 变化（`app/mod.rs`）

```rust
pub struct App {
    contents: HashMap<ContentId, Box<dyn ContentHandler>>,   // 数据（可被多 view 共享）
    views: HashMap<SpaceId, View>,                            // 替代原 cursors 字段
    scene: Scene,
    focused: SpaceId,
    dispatcher: Dispatcher,
    should_quit: bool,
    frontend: FrontendImpl,
    bg_tx: mpsc::Sender<BgResult>,
    bg_rx: mpsc::Receiver<BgResult>,
    pending_save: Option<ContentId>,
}
```

- 删 `cursors: HashMap<ContentId, Cursors>`。
- `App::new`：建 scene 后，遍历 scene 所有 `Host` space，为每个建 `View::new(host.content)` 插入 `views`。
- `focused_content_id()`：`self.views[&self.focused].content()`，不再从 scene 反查。
- `execute_operation`：`let view = self.views.get_mut(&self.focused).expect("focused view exists"); executor::execute(op, content, view.selections_mut());`

### 4.4 ContentQuery 变化（`protocol/content_query.rs`）

```rust
pub trait ContentQuery {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String>;       // 不变
    fn status_bar(&self, cid: ContentId) -> StatusBarData;                  // 不变
    fn line_count(&self, cid: ContentId) -> usize;                          // 不变
    fn selections(&self, sid: SpaceId) -> Selections;                       // 新（替代 cursor(cid)->CursorPos）
}
```

- 返回 owned `Selections`（clone）：`dyn ContentQuery` 返回引用有生命周期麻烦，且 trait 全 owned 风格一致。v0.2 `ranges.len()==1`，clone 两个 `CursorPos` 可忽略。
- 不保留 `cursor()` 便捷方法：cursor 是 `selections.primary().head()` 的派生，DRY 不单设查询。前端 SceneRenderer 是唯一消费者。

### 4.5 AppQuery 适配器（`app/mod.rs`）

```rust
struct AppQuery<'a> {
    contents: &'a HashMap<ContentId, Box<dyn ContentHandler>>,
    views: &'a HashMap<SpaceId, View>,
}

impl<'a> ContentQuery for AppQuery<'a> {
    fn selections(&self, sid: SpaceId) -> Selections {
        self.views
            .get(&sid)
            .map(|v| v.selections().clone())
            .unwrap_or_else(|| Selections::single(Selection::collapsed(CursorPos::origin())))
    }
    // lines/status_bar/line_count 不变（按 cid 查 contents）
}
```

`App impl ContentQuery` 委托 `AppQuery`（与现状同模式）。

### 4.6 编辑原语升级（`core/buffer.rs`）

**核心原则**：v0.2 所有 selection collapsed，编辑原语操作 `head`，末尾 `anchor = head` 守恒。

| 现签名 | 新签名 | 行为 |
|---|---|---|
| `recompute_cursor(&mut CursorPos)` | 保留 `pub` | recompute 单点 row/col（底层 + 测试用） |
| — | `recompute_selection(&mut Selection)` `pub` | recompute head + anchor（v0.2 幂等） |
| `move_cursor_*(cur: &mut CursorPos, n)` `pub` | 降级 `pub(crate)`（底层） | 操作单点（操作 head 用） |
| — | `move_selection_*(sel: &mut Selection, n)` `pub` | 调 `move_cursor_*` on `sel.head`，末尾 `sel.anchor = sel.head` |
| `set_cursor(cur, char_idx, line_idx)` `pub` | 降级 `pub(crate)` | 设单点 |
| — | `set_selection(sel, char_idx, line_idx)` `pub` | 设 head，`anchor = head` |
| `insert_at_cursors(&mut Cursors, text)` | `insert_at_selections(&mut Selections, text)` `pub` | 在每个 head 插入，head 前移 text_len，anchor=head |
| `delete_at_cursors(&mut Cursors, n)` | `delete_at_selections(&mut Selections, n)` `pub` | 在每个 head 方向删 n，head 回退，anchor=head |

`move_selection_*` 内部：`self.move_cursor_*(&mut sel.head, n); sel.anchor = sel.head;`

`insert_at_selections`/`delete_at_selections`：v0.2 collapsed 下在 head 点操作（等价原 cursor 行为）；未来非 collapsed 时处理 range（删 range + 插入），v0.3 实现。

### 4.7 executor（`app/executor.rs`）

```rust
pub fn execute(op: Operation, content: &mut dyn ContentHandler, selections: &mut Selections) {
    let Some(buf) = content.buffer_mut() else { return; };
    match op {
        Operation::CursorMoveBy { chars, lines } => {
            for sel in selections.all_mut() { buf.move_selection_by(sel, chars, lines); }
        }
        Operation::CursorMoveLeftBy(n)  => { for sel in selections.all_mut() { buf.move_selection_left(sel, n); } }
        Operation::CursorMoveRightBy(n) => { for sel in selections.all_mut() { buf.move_selection_right(sel, n); } }
        Operation::CursorMoveUpBy(n)    => { for sel in selections.all_mut() { buf.move_selection_up(sel, n); } }
        Operation::CursorMoveDownBy(n)  => { for sel in selections.all_mut() { buf.move_selection_down(sel, n); } }
        Operation::CursorMoveTo { char_idx, line_idx } => {
            buf.set_selection(selections.primary_mut(), char_idx, line_idx);
            selections.retain_primary();   // 清 secondaries（v0.2 noop）
        }
        Operation::CursorInsertText(text) => buf.insert_at_selections(selections, &text),
        Operation::CursorDelete(n)         => buf.delete_at_selections(selections, n),
        _ => {}
    }
}
```

`Operation` 枚举不变（`Cursor*` 前缀保留——用户面术语；buffer 方法用 `selection` 术语——实现面。两层分离）。

## 5. 数据流

### 5.1 事件流（输入 → 状态）

```
crossterm EventStream → Input → FrontendEvent → App::handle_event
  → Dispatcher.dispatch → Operation → executor.execute(op, content, view.selections_mut())
    → buf.move_selection_*/insert_at_selections/delete_at_selections（改 head + 守恒 collapsed）
```

executor 拿 `view.selections_mut()`（按 `self.focused` SpaceId 取 View），不再按 ContentId 取 cursor map。

### 5.2 渲染流（状态 → 输出）

```
App::render:
  frontend.render(&scene, &query, focused)
    → SceneRenderer.render:
        ├─ TaffyEngine.layout(scene) → ResolvedScene
        ├─ query.selections(focused) → Selections（owned clone）   ← 前端 pull 选区
        │    → primary().head() 算 cursor 屏坐标 + ensure viewport
        │    → 未来：遍历 all() 画选区高亮
        ├─ query.lines(cid, range) → Vec<String>                   ← 前端 pull 可见行
        └─ 逐行画到 Canvas + 光标定位
```

`query.selections(focused)` 按 SpaceId 查 View；`query.lines(cid)` 按 ContentId 查 content。单视图下 focused space 的 Host content 即 focused content，配对使用。

## 6. 关键决策记录

| 决策 | 选择 | 理由 |
|---|---|---|
| selection 元素表达 | `anchor/head`（非 `from/to+direction`） | 语义直接，方向隐含；不加 direction 字段；sort 时取 min |
| 多选区容器 | Helix 风 `ranges: Vec + primary_index` | 统一有序 Vec，多光标 normalize 自然；v0.2 index 冗余但就位，v0.3 不重构结构 |
| 模型深度 | v0.2 恒 collapsed（模型就位 + 退化占位） | 本次核心是归属 + 模型骨架；真选区编辑是独立大功能，YAGNI |
| selection 归属 | 后端 View 实体（非前端） | selection 是编辑态，输入处理在后端，须后端 executor 可达；放前端破坏 pull 边界 |
| View 放 app 层 | `app/view.rs` | View 是编排层"编辑会话"实体；core 保持纯领域（Buffer 文本模型）；protocol 保持纯数据 |
| ContentQuery 返回 | `selections(sid) -> Selections` owned | 前端未来画选区高亮需完整 selections；owned 与 trait 风格一致；v0.2 clone 便宜 |
| cursor 查询维度 | ContentId → SpaceId | selection 按 SpaceId 索引（多视图铺路）；cursor 是 selections 派生 |
| 不保留 cursor() 便捷方法 | DRY | cursor = `selections.primary().head()` 派生；前端唯一消费者 |
| Operation 前缀 | 保留 `Cursor*`（用户面） | cursor 是用户心智模型；buffer 方法用 selection 术语（实现面） |
| `move_cursor_*` 底层保留 | 降级 `pub(crate)` | 复用点移动逻辑操作 head；`move_selection_*` 包装守恒 collapsed |
| `retain_primary()` | 加（v0.2 noop） | 为 CursorMoveTo 清多光标语义就位，v0.3 多光标铺路 |
| `direction` 字段 | 永久不加 | anchor/head 已隐含方向 |

## 7. 测试策略

### 7.1 测试分层

- **`protocol/selection.rs`**：`Selection::collapsed`/`is_empty`/`head`、`Selections::single`/`primary`/`primary_mut`/`all`/`all_mut`/`retain_primary`、`primary_index` 索引正确、`ranges` 私有性（外部不能直接访问）。
- **`core/buffer.rs`**：
  - `move_selection_*`：断言 head 移动 + `anchor == head` 守恒。
  - `insert_at_selections`/`delete_at_selections`：head 前移/回退 + collapsed 守恒。
  - `recompute_selection`：head + anchor 双端 row/col 正确。
  - 现有 `move_cursor_*`/`insert_at_cursors`/`delete_at_cursors` 测试改写为新签名（底层 `move_cursor_*` 测试保留作 `pub(crate)` 验证）。
- **`app/executor.rs`**：`execute(op, content, &mut Selections)` 各 Operation 分支，断言 `selections.primary().head()`。
- **`app/view.rs`**：`View::new` 初始 selection 是 origin collapsed、`content()`/`selections()`/`selections_mut()` 访问。
- **`app/mod.rs`**：
  - `AppQuery::selections(sid)` 按 SpaceId 返回正确 Selections（替代原 `cursor(cid)` 测试）。
  - 现有集成测试（insert/backspace/arrow/save/resize/status_bar）改走 `views`：断言 `app.views.get(&focused).selections().primary().head()` 替代 `app.cursors.get(&cid).primary`。
  - 字节级断言（HeadlessFrontend 捕获）不变。

### 7.2 collapsed 守恒守护

每个 buffer 编辑原语测试断言操作后 `sel.anchor == sel.head`，守住 v0.2 不变量。漏守恒点会被测试捕获（虽不影响 v0.2 渲染——只用 head——但污染未来选区高亮）。

## 8. 迁移影响清单

### 新建

- `protocol/selection.rs`：`CursorPos`（从 cursor.rs 移入）+ `Selection` + `Selections`
- `app/view.rs`：`View` 实体

### 删除

- `protocol/cursor.rs`：内容移入 `selection.rs`，文件改名
- `core/content.rs` 的 `Cursors` 类型定义（移至 `protocol/selection.rs` 的 `Selections`）

### 改写

- `core/buffer.rs`：编辑原语签名 `CursorPos→Selection` / `Cursors→Selections`；import `cursor→selection`；底层 `move_cursor_*`/`set_cursor` 降级 `pub(crate)`；新增 `move_selection_*`/`set_selection`/`insert_at_selections`/`delete_at_selections`/`recompute_selection`
- `core/content.rs`：`Cursors` 引用 → `Selections`（类型在 protocol/selection.rs）
- `app/mod.rs`：`cursors` 字段 → `views` 字段；`App::new` 建 views；`focused_content_id` 收进 View；`execute_operation` 取 `view.selections_mut()`；`AppQuery` 改 `selections(sid)`；`App impl ContentQuery` 的 `cursor` 方法改 `selections`
- `app/executor.rs`：签名 `cursors: &mut Cursors` → `selections: &mut Selections`；调 `move_selection_*`/`insert_at_selections`/`delete_at_selections`
- `protocol/content_query.rs`：`cursor(cid)->CursorPos` → `selections(sid)->Selections`
- `tui/scene_renderer.rs`：`query.cursor(focused_cid)` → `query.selections(focused).primary().head()`
- 所有测试：`Cursors::single` → `Selections::single(Selection::collapsed(...))`；断言 `primary.head` 替代 `primary.char_index`

### 不变

- `Operation` 枚举（`Cursor*` 前缀保留）
- `protocol/space.rs`/`scene.rs`/`geometry.rs`/`ids.rs`/`status.rs`/`viewport.rs`（Space 仍纯几何）
- `app/dispatcher.rs`/`app/content.rs`（ContentLookup 按 cid 查 content，不涉 selection）
- `Frontend` trait 签名 `render(scene, query, focused)`
- `TuiFrontend`/`HeadlessFrontend` 结构（内部 query 调用改）

## 9. Non-goals / Follow-up

- **真选区编辑**：建选区（shift+方向键/拖拽）、按 selection 删除/替换输入、多 selection 交互——v0.3。本次只建模型，v0.2 恒 collapsed。
- **真多视图运行时场景**：同 content 在多个 space 显示、各自独立 selection——v0.3 多面板。本次架构按 SpaceId 索引就绪，但运行时仍单 editor space。
- **normalize（sort + merge overlapping + 调 primary_index）**：v0.3 多光标编辑原语。本次 `ranges` 有序约定写进类型注释，不实现 normalize。
- **`direction` 字段**：永久不加——`anchor/head` 已隐含方向。
- **selection 增量协议 / 零拷贝 pull**：v0.3，沿用前置重构 spec 的 follow-up。
- **`secondaries` 非空**：v0.2 恒空，`CursorAddAtNextMatch`/`CursorRemoveSecondary` 仍为预留死变体。

## 10. 风险

1. **collapsed 不变量守恒点散落**：每个 `move_selection_*`/`insert_at_selections`/`delete_at_selections` 末尾都要 `anchor=head`。漏一处不会立刻坏（v0.2 渲染只用 head），但会污染未来选区高亮。**缓解**：buffer 测试断言每次操作后 `sel.anchor == sel.head`。
2. **ContentQuery 维度混合**（`selections` 按 sid，其余按 cid）：前端 SceneRenderer 调用时 cid/sid 要传对。当前单视图下 focused space 的 Host content 即 focused content。**缓解**：SceneRenderer 从 ResolvedScene 的 Host item 同时拿 sid 和 cid，配对使用。
3. **View.content 与 scene Host{content} 一致性**：View 创建时从 scene 读 content，之后 View 是 source of truth。v0.2 scene 静态（`build_editor_scene` 一次性建），无同步问题；v0.3 scene 动态增删面板时需同步 View。**缓解**：本次 `App::new` 遍历 scene Host space 建 View，一致性由构造保证；v0.3 留 TODO。
4. **selection 返回 owned clone**：每帧 `query.selections(focused)` clone `Selections`。v0.2 `ranges.len()==1`，clone 两个 `CursorPos` 可忽略。未来多光标 clone 成本上升——v0.3 若需优化，改 `ContentQuery` 返回引用或用 Cow。本次不优化。
