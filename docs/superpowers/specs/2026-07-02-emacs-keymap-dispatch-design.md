# Emacs 风格多层 keymap 捕获与 content 自治——设计规格

> 日期：2026-07-02
> 状态：已确认，待写实现计划
> 关联：`docs/superpowers/specs/2026-07-01-evloop-frontend-decouple-design.md`（前置重构）

## 1. 背景与目标

当前事件处理的问题：
- `App` 硬编码持有 `editor_content` / `status_content` 两个 `ContentId`，`handle_event` 把所有 `Key` 直接塞给 `editor_content` 的 `Document`，`render` 把两个 ID 传给 `build_frame` 区分 `Editor` vs `StatusBar`。
- 没有捕获表概念——`handle_key(buf, cur, key)` 是自由函数，`Ctrl+S`/`Ctrl+Q` 硬编码在 `match` 里，不可配置。
- `Document` 把 `buffer` + `status` 捆绑，`StatusBar` 没有自己的身份，靠借聚焦文档的光渲染。

目标（借鉴 Emacs）：
1. **多层捕获机制**：global keymap → focused content keymap → parent 链上行 → default 兜底。任一层命中即止。
2. **每个 content 自治**：自持 keymap（可配置，未来脚本实时改）+ 自描述 render。content 仅查表返回 `Operation`，不执行。
3. **App 去角色化**：不持有 `editor_content` / `status_content` / 状态消息。退化为 evloop + dispatcher + executor + 副作用执行器。
4. **前缀键支持**（v0.2 就要）：`Keymap` 为前缀树，dispatcher 跨按键保持 pending 状态。
5. **多光标预留**：`Cursors{primary, secondaries}` 结构立起来，v0.2 `secondaries` 始终空，executor 在 `all()` 上跑（退化为单光标）。

## 2. 模块布局

### protocol 瘦身——只留前后端交互协议数据

| 文件 | 内容 |
|---|---|
| `protocol/ids.rs` | `ContentId`/`SpaceId`/`SceneId` |
| `protocol/frame.rs` | `Frame`/`FrameItem`/`FrameContent`/`Rect` |
| `protocol/frontend_event.rs` | `FrontendEvent`/`ResizeEvent` |
| `protocol/key_event.rs` | `KeyEvent`/`CtrlKey`/`ArrowKey` |
| `protocol/cursor.rs` | `CursorPos` |
| `protocol/viewport.rs` | `Viewport` |
| `protocol/status.rs` | `StatusMessage` |

**移出 protocol**：`keymap`/`Command`/`ContentHandler`/`EditView`/`ContentLookup`/`SpaceState`/`WrapMode`——这些是 content 与分发内部抽象，前端不感知。`protocol/edit_view.rs` 删除。

### core——领域逻辑

| 文件 | 变更 | 职责 |
|---|---|---|
| `core/operation.rs` | **新建** | `Operation` 枚举 + `Direction`。依赖 protocol（key_event, cursor） |
| `core/keymap.rs` | **新建** | `Keymap` 前缀树 + `KeyBinding`。依赖 protocol(key_event) + core/operation |
| `core/content.rs` | **新建** | `ContentHandler` trait + `ContentLookup` trait + `Cursors` + `SpaceState` + `WrapMode`。依赖 protocol + core/keymap + core/buffer（`buffer_mut` 返回 `&mut Buffer`） |
| `core/buffer.rs` | 改 | `Buffer`（文本 + path + modified + **`status: StatusMessage`** + keymap）impl `ContentHandler`；编辑原语方法（`move_cursor_*`/`insert_at_cursors`/`delete_at_cursors`/`set_cursor`/`recompute`，从 `edit.rs` 搬来）；`open_path` |
| `core/status_bar.rs` | **新建** | `StatusBar` content：持 `target_content_id` + 空 keymap；`render` 主动查 target content |
| `core/edit.rs` | **删除** | 逻辑拆入 `buffer.rs`（编辑原语）+ `operation.rs`（Direction） |
| `core/status.rs` | **删除** | `Status` 包装不再需要，消息归 `Buffer.status` |

> **同 crate 循环依赖**：`core/content.rs` 定义 `ContentHandler::buffer_mut -> Option<&mut Buffer>` 需 `use crate::core::buffer::Buffer`；`core/buffer.rs` impl `ContentHandler` 需 `use crate::core::content::ContentHandler`。Rust 同 crate 模块间相互引用合法，无问题。

### 其余模块

| 文件 | 变更 | 职责 |
|---|---|---|
| `layout/` | 不变 | scene/space/taffy_engine/resolved/ids |
| `frame/mod.rs` | 改 | `build_frame` 不再收 editor/status 角色 ID；对每个 item 调 `content.render(ctx)`。依赖 core（ContentHandler） |
| `app/dispatcher.rs` | **新建** | `Dispatcher`：global keymap + pending 前缀状态 + 捕获链遍历 |
| `app/executor.rs` | **新建** | `execute(op, content, state)`：调 `content.buffer_mut()` + buffer 原语执行 Operation |
| `app/content.rs` | **新建**（替 `document.rs`） | `ContentLookup for HashMap<ContentId, Box<dyn ContentHandler>>`；删 `Document` |
| `app/mod.rs` | 改 | 删 `editor_content`/`status_content`/`status_message` 字段；调 `Dispatcher` + `executor`；执行全局 Operation 副作用 |
| `app/frontend.rs` | 不变 | Frontend trait + FrontendImpl + HeadlessFrontend |
| `tui/tui_frontend.rs` | 不变 | 薄 painter |
| `main.rs` | 不变 | multi_thread + FrontendImpl::Tui |

### 依赖方向

`protocol` ← `core` ← `frame`/`layout` ← `app` ← `main`。protocol 纯数据无行为 trait。`app` 不 import `tui`（保持）。`core` 不 import `app`/`frame`。

## 3. 核心类型

### 3.1 Operation（`core/operation.rs`）

统一枚举，content 查表返回它，executor 执行它。命名 Subject-Predicate-Object（借鉴 rsvim，但不照抄——多光标友好）。

```rust
use crate::protocol::cursor::CursorPos;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction { Left, Right, Up, Down }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    // 光标——相对移动作用于"所有"光标（多光标友好）
    CursorMoveBy { chars: isize, lines: isize },
    CursorMoveLeftBy(usize),
    CursorMoveRightBy(usize),
    CursorMoveUpBy(usize),
    CursorMoveDownBy(usize),
    // 绝对移动——作用于主光标，清空次光标
    CursorMoveTo { char_idx: usize, line_idx: usize },

    // 文本——在每个光标处执行
    CursorInsertText(String),
    CursorDelete(isize),  // 负向左、正向右

    // 视口
    ViewportScrollBy { lines: isize },

    // 全局
    Save,
    Quit,
    FocusNext,
    FocusPrev,

    // 多光标（v0.2 预留，不绑键、executor noop）
    CursorAddAtNextMatch(String),
    CursorRemoveSecondary,
}
```

> `CursorInsertText(String)` / `CursorAddAtNextMatch(String)` 含 `String` 故非 `Copy`，但 `Clone`。其余变体可 `Copy`。整个枚举 `Clone` 即可（dispatcher/executor 用 `clone`）。

### 3.2 Keymap 前缀树（`core/keymap.rs`）

```rust
use std::collections::HashMap;
use crate::protocol::key_event::KeyEvent;
use crate::core::operation::Operation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyBinding {
    Operation(Operation),
    Prefix(Keymap),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    bindings: HashMap<KeyEvent, KeyBinding>,
}

impl Keymap {
    pub fn new() -> Self { Self::default() }
    pub fn lookup(&self, key: KeyEvent) -> Option<&KeyBinding> { self.bindings.get(&key) }
    pub fn bind(&mut self, key: KeyEvent, op: Operation) { self.bindings.insert(key, KeyBinding::Operation(op)); }
    pub fn bind_prefix(&mut self, key: KeyEvent, sub: Keymap) { self.bindings.insert(key, KeyBinding::Prefix(sub)); }
    pub fn unbind(&mut self, key: KeyEvent) { self.bindings.remove(&key); }
}
```

### 3.3 ContentHandler trait + Cursors（`core/content.rs`）

```rust
use std::borrow::Cow;
use crate::core::keymap::{Keymap, /* KeyBinding 不需要 */};
use crate::core::operation::Operation;
use crate::core::buffer::Buffer;  // 同 crate 循环引用，合法
use crate::protocol::cursor::CursorPos;
use crate::protocol::frame::FrameContent;
use crate::protocol::frontend_event::KeyEvent;  // default_binding 用
use crate::protocol::ids::ContentId;
use crate::protocol::status::StatusMessage;
use crate::protocol::viewport::Viewport;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapMode { None, Soft }  // 从 protocol 移入

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursors {
    pub primary: CursorPos,
    pub secondaries: Vec<CursorPos>,  // v0.2 始终空
}
impl Cursors {
    pub fn single(c: CursorPos) -> Self { Self { primary: c, secondaries: Vec::new() } }
    pub fn all(&self) -> impl Iterator<Item = &CursorPos> { std::iter::once(&self.primary).chain(self.secondaries.iter()) }
    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut CursorPos> { std::iter::once(&mut self.primary).chain(self.secondaries.iter_mut()) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceState {
    pub viewport: Viewport,
    pub cursors: Cursors,  // 原 cursor: CursorPos
}

pub struct RenderCtx<'a> {
    pub lookup: &'a dyn ContentLookup,
    pub focused_content_id: ContentId,
    pub state: SpaceState,
    pub rect_height: i32,
}

pub trait ContentLookup {
    fn get(&self, id: ContentId) -> Option<&dyn ContentHandler>;
}

/// content 多态契约：自持 keymap + 自描述 render + 暴露 buffer。
/// 不含事件执行逻辑——仅查表返回 Operation，执行在 executor。
pub trait ContentHandler {
    // —— 读视图（默认实现；文档类 content override，非文档 content 用默认）——
    fn line(&self, _idx: usize) -> Cow<str> { Cow::Borrowed("") }
    fn len_lines(&self) -> usize { 0 }
    fn file_name(&self) -> Option<&str> { None }
    fn modified(&self) -> bool { false }
    fn status(&self) -> StatusMessage { StatusMessage::None }

    // —— 捕获表（可被脚本运行时改）——
    fn keymap(&self) -> &Keymap;
    fn keymap_mut(&mut self) -> &mut Keymap;

    /// keymap 未命中时的兜底绑定（仍只返回 Operation，不执行）。
    /// 处理"任意可打印字符→插入"这类无法静态绑定的动态映射。
    fn default_binding(&self, _key: KeyEvent) -> Option<Operation> { None }

    // —— 暴露内部 Buffer 供 executor 操作（非 buffer content 返回 None）——
    fn buffer_mut(&mut self) -> Option<&mut Buffer> { None }

    // —— 自描述渲染 ——
    fn render(&self, ctx: &RenderCtx) -> FrameContent;
}
```

### 3.4 Buffer impl 要点（`core/buffer.rs`）

- 字段：`rope: Rope`, `path: Option<PathBuf>`, `modified: bool`, `status: StatusMessage`, `keymap: Keymap`
- `new()`：初始化 + 调 `default_buffer_keymap()` 填编辑键
- `impl ContentHandler`：
  - 读视图：`line`/`len_lines`/`file_name`/`modified`/`status` override
  - `keymap`/`keymap_mut` 返回 `&self.keymap` / `&mut self.keymap`
  - `default_binding`：`Char(ch)` → `Some(Operation::CursorInsertText((ch as char).to_string()))`，其余 `None`
  - `buffer_mut` → `Some(self)`
  - `render`：按 `ctx.state.viewport` + `ctx.rect_height` 收集可见行 → `FrameContent::Editor { lines }`
- 编辑原语方法（从 `core/edit.rs` 搬来，改为多光标友好）：
  - `move_cursor_by(&self, cur: &mut CursorPos, chars: isize, lines: isize)`
  - `move_cursor_left/right/up/down(&self, cur: &mut CursorPos, n: usize)`
  - `set_cursor(&self, cur: &mut CursorPos, char_idx: usize, line_idx: usize)`
  - `insert_at_cursors(&mut self, cursors: &mut Cursors, text: &str)` —— 按 char_index 降序插入避免索引偏移
  - `delete_at_cursors(&mut self, cursors: &mut Cursors, n: isize)`
  - `recompute(cur: &mut CursorPos)` —— 内部工具
  - `mark_saved(&mut self)` / `set_status(&mut self, msg: StatusMessage)`
  - `open_path(&mut self, path: &str) -> io::Result<()>` —— 设 `NewFile`/`OpenFailed`/正常

### 3.5 StatusBar impl 要点（`core/status_bar.rs`）

```rust
pub struct StatusBar {
    target_content_id: ContentId,
    keymap: Keymap,  // 空
}
impl StatusBar {
    pub fn new(target_content_id: ContentId) -> Self { Self { target_content_id, keymap: Keymap::new() } }
}
impl ContentHandler for StatusBar {
    // 读视图用默认（空/None/false/None）
    fn keymap(&self) -> &Keymap { &self.keymap }
    fn keymap_mut(&mut self) -> &mut Keymap { &mut self.keymap }
    // default_binding: None（用默认）
    // buffer_mut: None（用默认）
    fn render(&self, ctx: &RenderCtx) -> FrameContent {
        let target = ctx.lookup.get(self.target_content_id);
        FrameContent::StatusBar {
            file_name: target.and_then(|c| c.file_name().map(|s| s.to_string())),
            modified: target.map(|c| c.modified()).unwrap_or(false),
            message: target.map(|c| c.status()).unwrap_or(StatusMessage::None),
        }
    }
}
```

## 4. Dispatcher：捕获链 + 前缀状态机（`app/dispatcher.rs`）

```rust
pub struct Dispatcher {
    global_keymap: Keymap,
    pending: Option<Keymap>,  // None=Idle; Some=前缀子表，下一键在此查
}

enum LookupResult<'a> { Hit(Operation), Prefix(&'a Keymap), Miss }

impl Dispatcher {
    pub fn new(global_keymap: Keymap) -> Self { Self { global_keymap, pending: None } }
    pub fn is_pending(&self) -> bool { self.pending.is_some() }

    pub fn dispatch(
        &mut self, key: KeyEvent, focused: SpaceId,
        scene: &Scene, contents: &dyn ContentLookup,
    ) -> Option<Operation> {
        // 1) 前缀待续：在 pending 子表查
        if let Some(sub) = self.pending.take() {
            return match lookup_in(&sub, key) {
                LookupResult::Hit(op) => Some(op),                          // 完成，重置 Idle
                LookupResult::Prefix(sub2) => { self.pending = Some(sub2.clone()); None }
                LookupResult::Miss => None,                                 // 前缀中断，重置 Idle，丢弃 key
            };
        }
        // 2) Idle：沿捕获链查
        for km in self.capture_chain(focused, scene, contents) {
            match lookup_in(km, key) {
                LookupResult::Hit(op) => return Some(op),
                LookupResult::Prefix(sub) => { self.pending = Some(sub.clone()); return None; }
                LookupResult::Miss => continue,
            }
        }
        // 3) 全链未命中：focused content 的 default_binding 兜底
        focused_content_id(scene, focused)
            .and_then(|cid| contents.get(cid))
            .and_then(|c| c.default_binding(key))
    }

    fn capture_chain<'a>(&'a self, focused: SpaceId, scene: &'a Scene, contents: &'a dyn ContentLookup) -> Vec<&'a Keymap> {
        let mut chain = Vec::new();
        let mut cur = Some(focused);
        while let Some(sid) = cur {
            let node = scene.node(sid);
            if let SpaceKind::Host { content } = &node.space.kind {
                if let Some(c) = contents.get(*content) { chain.push(c.keymap()); }
            }
            cur = node.parent;  // container 无 content，自动跳过
        }
        chain.push(&self.global_keymap);
        chain
    }
}

fn lookup_in(keymap: &Keymap, key: KeyEvent) -> LookupResult {
    match keymap.lookup(key) {
        Some(KeyBinding::Operation(op)) => LookupResult::Hit(op.clone()),
        Some(KeyBinding::Prefix(sub)) => LookupResult::Prefix(sub),
        None => LookupResult::Miss,
    }
}

fn focused_content_id(scene: &Scene, focused: SpaceId) -> Option<ContentId> {
    let node = scene.node(focused);
    match &node.space.kind {
        SpaceKind::Host { content } => Some(*content),
        _ => None,
    }
}
```

**捕获链顺序**（content 优先，global 兜底，default 最后）：
1. focused content keymap
2. parent 链上行的 host content keymap（container 自动跳过）
3. global keymap
4. focused content 的 `default_binding`（兜底，处理任意可打印字符插入）

**前缀键语义**：
- `Idle` 状态按前缀键（`KeyBinding::Prefix(sub)`）→ `pending = sub`，返回 `None`，等下一键
- 下一键在 `sub` 查：命中 `Operation` → 执行并重置 `Idle`；命中嵌套 `Prefix` → 继续 pending；未命中 → 中断、重置 `Idle`、丢弃该键
- 前缀中途的键不走捕获链、不走 default（已锁定在某层子表）
- `Resize`/`QuitRequest` 不经 dispatcher，App 直接处理
- `focused` 假定为 host space（v0.2 焦点总在 host 上；"最上层"= focused，无 z-order 多层）

## 5. Executor：Operation 执行（`app/executor.rs`）

```rust
use crate::core::content::{ContentHandler, SpaceState};
use crate::core::operation::Operation;

/// 执行局部 Operation（光标/文本/视口）。全局/多光标变体不进此处（App 分流）。
pub fn execute(op: Operation, content: &mut dyn ContentHandler, state: &mut SpaceState) {
    let Some(buf) = content.buffer_mut() else { return; };  // 非 buffer content noop
    match op {
        Operation::CursorMoveBy { chars, lines } =>
            for c in state.cursors.all_mut() { buf.move_cursor_by(c, chars, lines); },
        Operation::CursorMoveLeftBy(n)  => for c in state.cursors.all_mut() { buf.move_cursor_left(c, n); },
        Operation::CursorMoveRightBy(n) => for c in state.cursors.all_mut() { buf.move_cursor_right(c, n); },
        Operation::CursorMoveUpBy(n)    => for c in state.cursors.all_mut() { buf.move_cursor_up(c, n); },
        Operation::CursorMoveDownBy(n)  => for c in state.cursors.all_mut() { buf.move_cursor_down(c, n); },
        Operation::CursorMoveTo { char_idx, line_idx } => {
            buf.set_cursor(&mut state.cursors.primary, char_idx, line_idx);
            state.cursors.secondaries.clear();
        }
        Operation::CursorInsertText(text) => buf.insert_at_cursors(&mut state.cursors, &text),
        Operation::CursorDelete(n)        => buf.delete_at_cursors(&mut state.cursors, n),
        Operation::ViewportScrollBy { lines } => state.viewport.scroll_by(lines),
        // 全局/多光标变体不进 executor
        _ => {}
    }
}
```

> `Viewport::scroll_by(lines)` 需在 `protocol/viewport.rs` 新增（或 core 提供）。若 v0.2 不绑滚动键，可预留方法空实现。

## 6. App 去角色化与数据流（`app/mod.rs`）

### App 结构

```rust
pub struct App {
    contents: HashMap<ContentId, Box<dyn ContentHandler>>,
    scene: Scene,            // 不再包 EditorScene
    engine: TaffyEngine,
    focused: SpaceId,
    dispatcher: Dispatcher,
    should_quit: bool,
    frontend: FrontendImpl,
    bg_tx: mpsc::Sender<BgResult>,
    bg_rx: mpsc::Receiver<BgResult>,
    pending_save: Option<ContentId>,
}
```

**删去**：`editor_content`/`status_content`/`status_message` 字段。两个角色 ID 只在 `new` 局部用于构建。

### App::new

```rust
pub fn new(path: Option<&str>, width: usize, height: usize, frontend: FrontendImpl) -> io::Result<Self> {
    let editor_content = ContentId(0);
    let status_content = ContentId(1);
    let mut buffer = Buffer::new();
    if let Some(p) = path { buffer.open_path(p)?; }  // 设 NewFile/OpenFailed
    let status_bar = StatusBar::new(editor_content);  // 观察编辑器 content
    let mut contents: HashMap<ContentId, Box<dyn ContentHandler>> = HashMap::new();
    contents.insert(editor_content, Box::new(buffer));
    contents.insert(status_content, Box::new(status_bar));
    let (scene, editor_space) = build_editor_scene(width as i32, height as i32, editor_content, status_content);
    let dispatcher = Dispatcher::new(default_global_keymap());
    let (bg_tx, bg_rx) = mpsc::channel::<BgResult>(8);
    Ok(Self {
        contents, scene, engine: TaffyEngine::new(), focused: editor_space,
        dispatcher, should_quit: false, frontend, bg_tx, bg_rx, pending_save: None,
    })
}
```

> `build_editor_scene` 改返回 `(Scene, SpaceId)`（去掉 `EditorScene` 包装）。

### 事件循环

```rust
pub async fn run(&mut self) -> io::Result<()> {
    self.render()?;
    loop {
        tokio::select! {
            ev = self.frontend.next_event() => if let Some(e) = ev? { self.handle_event(e).await?; },
            res = self.bg_rx.recv() => if let Some(r) = res { self.handle_bg_result(r)?; },
        }
        if self.should_quit { break; }
        self.render()?;
    }
    Ok(())
}

async fn handle_event(&mut self, event: FrontendEvent) -> io::Result<()> {
    match event {
        FrontendEvent::Resize(r) => self.scene.resize(r.width as i32, r.height as i32),
        FrontendEvent::QuitRequest => self.should_quit = true,
        FrontendEvent::Key(k) => {
            if let Some(op) = self.dispatcher.dispatch(k, self.focused, &self.scene, &self.contents) {
                self.execute_operation(op)?;
            }
        }
    }
    Ok(())
}

fn execute_operation(&mut self, op: Operation) -> io::Result<()> {
    match op {
        Operation::Save => { self.spawn_save(self.focused_content_id()); }
        Operation::Quit => self.should_quit = true,
        Operation::FocusNext | Operation::FocusPrev => {}      // v0.2 预留
        Operation::CursorAddAtNextMatch(_) | Operation::CursorRemoveSecondary => {}  // 多光标预留 noop
        _ => {
            let (content, state) = self.focused_content_and_state_mut();  // disjoint borrow
            executor::execute(op, content, state);
        }
    }
    Ok(())
}
```

### 保存回环

```rust
fn handle_bg_result(&mut self, res: BgResult) -> io::Result<()> {
    match res {
        BgResult::SaveResult(id, result) => {
            self.pending_save = None;
            let buf = self.contents.get_mut(&id).and_then(|c| c.buffer_mut()).expect("saved buffer exists");
            match result {
                Ok(()) => { buf.mark_saved(); buf.set_status(StatusMessage::Saved); }
                Err(_) => buf.set_status(StatusMessage::SaveFailed),
            }
        }
    }
    Ok(())
}
```

> `spawn_save` 内 `path` 为 `None` 时：`buf.set_status(SaveFailed)` + 返回 false（不发起）。

### render

```rust
fn render(&mut self) -> io::Result<()> {
    let resolved = self.engine.layout(&self.scene);
    // viewport 跟随 focused primary cursor
    if let Some(item) = resolved.items.iter().find(|i| i.content_id == self.focused_content_id()) {
        let space = self.scene.node_mut(self.focused);
        let row = space.space.cursors.primary.row;
        space.space.viewport.ensure_cursor_visible(row, item.rect.height as usize);
    }
    let focused_cursor = self.scene.node(self.focused).space.cursors.primary;
    let frame = build_frame(
        &resolved, &self.contents as &dyn ContentLookup,
        self.focused_content_id(), Some(focused_cursor),
    );
    self.frontend.render(&frame)
}
```

> `Space.space.cursors` 替代 `Space.space.cursor`（layout/space.rs 改 cursor 字段类型为 `Cursors`）。

### 辅助方法

```rust
fn focused_content_id(&self) -> ContentId {
    match &self.scene.node(self.focused).space.kind {
        SpaceKind::Host { content } => *content,
        _ => ContentId(0),  // 不应发生
    }
}

fn focused_content_and_state_mut(&mut self) -> (&mut dyn ContentHandler, &mut SpaceState) {
    let cid = self.focused_content_id();
    let content: &mut dyn ContentHandler = self.contents.get_mut(&cid).expect("focused content exists");
    let state: &mut SpaceState = &mut self.scene.node_mut(self.focused).space.state_snapshot_mut();
    (content, state)
}
```

> `Space` 需暴露 `&mut SpaceState`（cursor + viewport）。`SpaceState` 在 `core/content.rs`，`Space` 在 `layout/space.rs`——`layout` 依赖 `core` 取 `SpaceState`/`Cursors` 类型。`Space` 字段 `cursor: CursorPos` 改为 `cursors: Cursors`，`viewport: Viewport` 保留；提供 `fn state_mut(&mut self) -> SpaceState` 构造快照引用或直接暴露字段。具体由实现决定（见 plan）。

## 7. build_frame 改造（`frame/mod.rs`）

```rust
pub fn build_frame(
    scene: &ResolvedScene,
    contents: &dyn ContentLookup,
    focused_content_id: ContentId,
    focused_cursor: Option<CursorPos>,
) -> Frame {
    let mut items = Vec::new();
    for ri in &scene.items {
        let content = match contents.get(ri.content_id) { Some(c) => c, None => continue };
        let ctx = RenderCtx {
            lookup: contents,
            focused_content_id,
            state: ri.state,
            rect_height: ri.rect.height,
        };
        let frame_content = content.render(&ctx);
        items.push(FrameItem {
            content_id: ri.content_id,
            rect: FrameRect { x: ri.rect.x, y: ri.rect.y, width: ri.rect.width, height: ri.rect.height },
            state: ri.state,
            content: frame_content,
        });
    }
    Frame { items, focused_content: focused_content_id, focused_cursor }
}
```

> `Frame.focused_content` 字段保留（前端 painter 用其识别焦点 item 算屏坐标）。

## 8. 默认 keymap

### global keymap（`default_global_keymap()`）

- `Ctrl+Q` → `Operation::Quit`
- `Ctrl+S` → `Operation::Save`

### Buffer 默认 keymap（`default_buffer_keymap()`）

- `Enter` → `CursorInsertText("\n")`
- `Backspace` → `CursorDelete(-1)`
- `Arrow(Left)` → `CursorMoveLeftBy(1)`
- `Arrow(Right)` → `CursorMoveRightBy(1)`
- `Arrow(Up)` → `CursorMoveUpBy(1)`
- `Arrow(Down)` → `CursorMoveDownBy(1)`
- `Char(ch)` → 不静态绑，走 `default_binding` → `CursorInsertText(ch)`
- `Escape`/`Unknown` → 不绑，`default_binding` 返回 `None`，丢弃

### StatusBar keymap

空。

### 前缀键

v0.2 默认无前缀键绑定，但机制支持；测试中用临时 `Ctrl+X` 前缀（如 `Ctrl+X Ctrl+S` → Save）验证状态机。

## 9. 错误处理

- `Buffer::open_path`：文件不存在→`set_status(NewFile)` 返回 Ok；非 UTF-8→`set_status(OpenFailed)` 返回 Ok；其他 IO 错→返回 `Err`，`App::new` 传播，main 打印退出
- 保存：`spawn_save` 时 `path` 为 `None`（未命名 buffer）→ `set_status(SaveFailed)` + 不发起；`tokio::fs::write` 失败→回环 `Err`→`set_status(SaveFailed)`；成功→`mark_saved()` + `set_status(Saved)`
- dispatcher / executor 不产生 `io::Error`（纯内存操作）
- `frontend.next_event`/`render` 的 `io::Error` 正常传播
- `App::run` 返回 `io::Result<()>`，main 打印错误退出

## 10. 测试策略

### core 单元

- `Keymap` 前缀树：`lookup`/`bind`/`unbind`/嵌套 `Prefix`/`bind_prefix`
- `Cursors`：`single`/`all`/`all_mut`（primary + secondaries 迭代）、`secondaries` 空时退化
- `Buffer` 编辑原语：`move_cursor_*`/`insert_at_cursors`/`delete_at_cursors`/`set_cursor`/`recompute`，含 `secondaries` 空的退化 + 多光标插入顺序（降序避免索引偏移）
- `Buffer::default_binding`：`Char` → `CursorInsertText`，其余 `None`
- `StatusBar::render`：查询 target content 的 `file_name`/`modified`/`status`；target 缺失时默认值
- `Operation` 枚举 `Clone`/`PartialEq`

### app 单元

- `Dispatcher` 捕获链顺序：focused 优先 → parent 链 → global → `default_binding`
- 前缀状态机：单层命中 / 嵌套 Prefix / 中断丢弃 / `is_pending`
- `default_binding` 兜底（Char 插入）
- content 可覆盖 global（focused keymap 命中优先于 global）
- `executor::execute` 各 Operation 变体：光标移动改 `cursors`、文本改 buffer + 多光标、视口滚、非 buffer content noop、全局/多光标变体不进 executor

### headless 集成

复用 `HeadlessFrontend`（脚本事件 + 捕获帧）。`App::run` 跑脚本→验证：
- buffer 文本、`cursors.primary`、`Buffer.status`、`frames` 内容
- 插入/删除/移动
- `Ctrl+S` 保存（tempfile）→ `Saved` + 文件内容正确
- `Ctrl+Q` 退出
- `Resize` 改 scene.size
- 前缀键序列（临时绑 `Ctrl+X Ctrl+S`）
- StatusBar 渲染聚焦 Buffer 的 file_name/modified/message

### 测试迁移

`core/edit.rs` 测试拆解：编辑逻辑（`recompute`/`move_cursor`）迁 `core/buffer.rs`；`open_path` 测试迁 `buffer.rs`；`handle_key` 测试由 headless 集成测试替代（dispatch+executor 全链）。

## 11. 多光标预留

- `Cursors{primary, secondaries}` 结构立起来，v0.2 `secondaries` 始终空 Vec
- executor 在 `cursors.all_mut()` 上跑（退化为单光标）
- `Operation::CursorAddAtNextMatch`/`CursorRemoveSecondary` 枚举存在但不绑键、executor noop
- `CursorMoveTo` 清空 `secondaries`（语义正确，v0.2 单光标无影响）
- 渲染只画 `primary`（`TuiFrontend` painter 用 `focused_cursor` 单点）
- 未来实现多光标时：扩展 executor、绑定多光标键、painter 画所有光标——`Operation` 枚举与 `Cursors` 接口不变

## 12. 迁移与删除

### 新建

- `core/operation.rs`、`core/keymap.rs`、`core/content.rs`、`core/status_bar.rs`
- `app/dispatcher.rs`、`app/executor.rs`、`app/content.rs`

### 删除

- `core/edit.rs`（逻辑拆入 buffer + operation）
- `core/status.rs`（消息归 Buffer）
- `protocol/edit_view.rs`（EditView/ContentLookup/SpaceState/WrapMode 移入 core/content.rs）
- `app/document.rs`（Document 删除，替为 app/content.rs 的 ContentLookup impl）

### 改造

- `core/buffer.rs`：加 `status`/`keymap` 字段、impl `ContentHandler`、编辑原语方法、`open_path`/`set_status`/`mark_saved`
- `core/mod.rs`：模块列表更新
- `protocol/mod.rs`：删 `edit_view`，其余不变
- `layout/space.rs`：`Space.cursor: CursorPos` → `Space.cursors: Cursors`；提供 `SpaceState` 访问
- `layout/scene.rs`：`SpaceNode`/`Scene` 适配 `Cursors`；`build_editor_scene` 返回 `(Scene, SpaceId)`，删 `EditorScene`
- `layout/resolved.rs`：`SpaceState` 引用调整（类型从 protocol 移到 core）
- `frame/mod.rs`：`build_frame` 签名与实现改造（调 `content.render`）
- `app/mod.rs`：结构体去角色字段、`handle_event` 调 dispatcher、`execute_operation`、保存回环改 `set_status`
- `app/frontend.rs`、`tui/tui_frontend.rs`、`main.rs`：基本不变

## 13. 内存更新

更新 `memory/frontend-boxed-future-runtime.md` 或新增一条 memory，记录：
- 事件分发从 App 内联改为独立 Dispatcher（捕获链 + 前缀状态机）
- content 自治模型：`Buffer`/`StatusBar` impl `ContentHandler`，仅查表返回 `Operation`，executor 执行
- `Operation` 统一枚举（多光标友好，v0.2 预留）
- App 不持 content 角色 ID；`StatusBar` 持 `target_content_id` 主动查
- protocol 瘦身为纯前后端协议数据

---

## 决策汇总

| 决策点 | 选择 |
|---|---|
| content 多态模型 | `Buffer` + `StatusBar` + trait 对象 |
| 执行权 | content 仅查表返回 `Operation`，App/executor 执行 |
| 捕获链顺序 | content 先 → parent 链 → global 兜底 → `default_binding` 最后 |
| global 优先级 | content 可覆盖 global（content 优先） |
| StatusBar 数据 | 主动查 target content（`file_name`/`modified`/`status` 全归 Buffer） |
| 捕获表形态 | 数据 keymap（前缀树），可配置，未来脚本实时改 |
| 前缀键 | v0.2 就要，`Keymap` 前缀树 + dispatcher pending 状态机 |
| 多光标 | v0.2 仅预留结构（`Cursors{primary, secondaries}`，secondaries 空） |
| protocol 范围 | 仅前后端交互协议数据，行为 trait 移出 |
| `core/edit.rs` | 删除，逻辑入 `buffer.rs` + `operation.rs` |
| `core/status.rs` | 删除，消息归 `Buffer.status` |
