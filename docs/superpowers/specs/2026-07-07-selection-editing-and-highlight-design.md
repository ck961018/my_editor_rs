# 真选区编辑 + 高亮渲染——设计规格

> 日期：2026-07-07
> 状态：已确认，待写实现计划
> 前置：`docs/superpowers/specs/2026-07-07-selection-model-and-view-ownership-design.md`（selection 模型 + View 实体归属，已落地 commit 477195a）
> 对照：`docs/design/current-architecture.md`（当前架构事实描述）

## 1. 背景与动机

前置重构（2026-07-07，commit 477195a）把 cursor 升级为 selection 模型（`Selection { anchor, head }` + `Selections { ranges, primary_index }`），View 实体按 SpaceId 索引，ContentQuery 返回 `Selections`。但该次重构的核心是**归属 + 模型骨架**，明确把"真选区编辑"留作 v0.3 Non-goal（前置 spec §9）。

当前 selection 在前端和 core 层"名不副实"：

1. **core 层恒守恒 collapsed**：`buffer.rs` 的 `move_selection_*` 末尾 `anchor = sel.head`，根本无法产生非空 selection；`insert_at_selections`/`delete_at_selections` 在 collapsed 下按点操作，没有"删 range 再插入"的真选区编辑逻辑；`recompute_selection`/`Selection::is_empty` 是 `#[allow(dead_code)]` 预留。
2. **KeyEvent 不支持 Shift 修饰**：`translate_key` 只认 `KeyModifiers::CONTROL`，shift+方向键落入 `_ => Unknown`（且 `KeyCode::Left` 分支不检查 modifiers，shift+Left 当前被当普通 Arrow，丢 shift）。
3. **Operation 无选区变体**：无 `Extend*`/`Cancel`；`CursorAddAtNextMatch`/`CursorRemoveSecondary` 是死预留。
4. **前端只画光标定位**：`scene_renderer` 只用 `primary().head()` 定位光标，没有遍历选区画高亮；`Canvas` 纯文本无反白/属性支持。

目标（v0.3 最小真选区）：

1. **core 真选区编辑原语**：head/anchor 独立操作（`move_head_*`/`set_head`/`collapse_to_head`），去掉硬守恒 collapsed；`insert_at_selections`/`delete_at_selections` 非空时删 range + collapse（替换语义）。
2. **建选区交互**：shift+方向键扩展（anchor 钉住，head 沿方向键移动）；Escape 取消；普通方向键取消选区（Left/Right 收缩到端点，Up/Down 继续移动）。
3. **前端反白高亮**：Canvas 加 `set_reverse`；scene_renderer 遍历非空 selection 的 `[min,max]` 区间反白画（跨行分段）。
4. **单选区不变量保持**：`ranges.len()==1`、`primary_index==0`、无多光标、无 normalize、不加 `direction` 字段。

## 2. 工业对照

| 编辑器 | 建选区 | 取消选区 | 非空编辑 | 高亮 |
|---|---|---|---|---|
| VSCode | shift+方向键（anchor 钉住，head 移动） | 方向键（Left→min，Right→max，Up/Down 从 head 继续移） | 输入/backspace 替换选区 | 反白背景 |
| Helix | 永久 selection（移动即扩展，无 collapse） | `;` collapse | 输入替换 | 反白 |
| Vim | v 进入 visual，方向键扩展 head | Esc | 输入替换 | 反白 |

本项目选 **VSCode 风**：shift+方向键建/扩展（anchor 钉住 head 移动），普通方向键取消（Left/Right 收缩到端点，Up/Down 从 head 继续），输入/backspace 替换选区。理由：用户最熟悉的主流行为；与现有 collapsed 模型平滑过渡（空 selection 时行为同现状）。

**取消选区 Left/Right 用"收缩到端点不额外移"**（VSCode/Word/浏览器主流）：`Left → head = min(anchor,head)`，`Right → head = max(anchor,head)`，落点只取决于选区端点，与 head 朝向无关。backward 选区按 Left 不会跳到意外位置。Up/Down 无"端点"语义，统一 `collapse_to_head + move_up/down`（head 移动后 anchor 跟随 collapse）。

## 3. 模块布局

### protocol——Shift 事件 + 选区变体

| 文件 | 变更 | 说明 |
|---|---|---|
| `protocol/key_event.rs` | 改 | 新增 `Shift(ArrowKey)` 变体；`translate_key` 检查 `KeyModifiers::SHIFT` 分流 shift+方向键 |
| `protocol/selection.rs` | 改 | `Selection::is_empty`/`Buffer::recompute_selection` 去掉 `#[allow(dead_code)]`（真选区启用） |
| `protocol/content_query.rs` | 不变 | `selections(sid)` 已就位 |
| 其余 protocol 文件 | 不变 | `space.rs`/`scene.rs`/`geometry.rs`/`ids.rs`/`status.rs`/`viewport.rs`/`frontend_event.rs` |

### core——Operation 重命名 + 原语重构

| 文件 | 变更 | 说明 |
|---|---|---|
| `core/operation.rs` | 改 | 全量去 `Cursor` 前缀；新增 `ExtendLeftBy/RightBy/UpBy/DownBy(usize)` + `Cancel` |
| `core/buffer.rs` | 改 | 删 `move_selection_*`/`set_selection`（守恒版）；新增 `move_head_*`/`set_head`/`collapse_to_head`；重写 `insert_at_selections`/`delete_at_selections`（非空删 range + collapse）；`recompute_selection` 去 dead_code；`keymap` 绑定改名 + shift+方向键 + Escape |
| `core/keymap.rs` | 不变 | 类型不变（绑定值改名在 buffer/dispatcher） |
| `core/content.rs` / `core/status_bar.rs` | 不变 | |

### app——executor 分发新 Operation

| 文件 | 变更 | 说明 |
|---|---|---|
| `app/executor.rs` | 改 | 分发新 Operation：`MoveLeftBy/RightBy` 非空收缩/空移动，`MoveUpBy/DownBy` 统一 move+collapse，`Extend*` 不 collapse，`Cancel` collapse，`InsertText`/`Delete` 调 buffer |
| `app/dispatcher.rs` | 改 | `default_global_keymap` 绑定值去 Cursor 前缀（`Quit`/`Save` 本无前缀，实际不变） |
| `app/mod.rs` / `app/view.rs` / `app/content.rs` | 不变 | View 实体、AppQuery 不变 |

### tui——Canvas 反白 + 选区高亮

| 文件 | 变更 | 说明 |
|---|---|---|
| `terminal/output.rs` | 改 | `Canvas` trait 加 `set_reverse(on: bool)`；`Output<W>` 实现之（crossterm `Attribute::Reverse`/`NoReverse`） |
| `tui/scene_renderer.rs` | 改 | `paint_item` 遍历非空 selection 反白高亮 `[min,max]` 区间（跨行分段）；viewport 裁剪 |
| `tui/headless.rs` / `tui/tui_frontend.rs` | 不变 | 经 SceneRenderer，无结构变化 |

### 依赖方向

不变（沿用前置重构）：

```
protocol ← core ← app ← main
    ↑            ↑
    └── tui ─────┘
```

## 4. 核心变更

### 4.1 不变量变化（v0.2 → v0.3）

- **松动**：`Selection` 允许非空（`anchor != head`）。
- **保持**：`ranges.len()==1`（单选区，无多光标）、`primary_index==0`、无 normalize（单选区无需 sort/merge）、不加 `direction` 字段（anchor/head 隐含方向）。

### 4.2 交互模型

**建选区（shift+方向键 → `Extend*`）**：
- 首次：anchor 钉在当前 head 位置（不动），head 沿方向键移动一格。
- 继续：head 继续移动，anchor 始终不动。head 可跨过 anchor（forward ↔ backward 方向反转）。

**取消选区（普通方向键 → `MoveLeftBy/RightBy/UpBy/DownBy`，选区非空时）**：
- `MoveLeftBy` → `head = min(anchor,head)`，collapse（不额外左移）。
- `MoveRightBy` → `head = max(anchor,head)`，collapse。
- `MoveUpBy`/`MoveDownBy` → `move_head_up/down` + `collapse_to_head`（head 移动后 anchor 跟随，等价"取消并继续上下移"）。
- 选区为空（collapsed）时：`MoveLeftBy/RightBy/UpBy/DownBy` = `move_head_*` + `collapse`（同现状行为）。

**Escape → `Cancel`**：`collapse_to_head`（head 不动，anchor=head）+ `retain_primary`。

**编辑（选区非空时 → `InsertText`/`Delete`）**——替换语义：
- `InsertText`：先删 `[min,max]` 区间，插入文本，head 到插入末尾，collapse。
- `Delete`（backspace）：删 `[min,max]` 区间，head=min，collapse。
- 选区为空时：行为同现状（点操作）。

**shift 修饰范围**：只处理 shift+方向键（建/扩展选区）+ Escape（取消）。shift+其他键落 `Unknown`（YAGNI）。

### 4.3 KeyEvent（`protocol/key_event.rs`）

```rust
pub enum KeyEvent {
    Char(u8),
    Ctrl(CtrlKey),
    Arrow(ArrowKey),
    Shift(ArrowKey),   // 新增
    Backspace,
    Enter,
    Escape,
    Unknown,
}
```

`translate_key` 改：方向键分支检查 `KeyModifiers::SHIFT`：

```rust
KeyCode::Left if k.modifiers.contains(KeyModifiers::SHIFT) => KeyEvent::Shift(ArrowKey::Left),
KeyCode::Left => KeyEvent::Arrow(ArrowKey::Left),
// Right/Up/Down 同理
```

shift+其他键保持现状（落 `Unknown`）。

### 4.4 Operation（`core/operation.rs`）

全量去 `Cursor` 前缀 + 新增选区变体：

```rust
pub enum Operation {
    MoveBy { chars: isize, lines: isize },          // 原 CursorMoveBy
    MoveLeftBy(usize),                               // 原 CursorMoveLeftBy
    MoveRightBy(usize),
    MoveUpBy(usize),
    MoveDownBy(usize),
    MoveTo { char_idx: usize, line_idx: usize },     // 原 CursorMoveTo
    InsertText(String),                              // 原 CursorInsertText
    Delete(isize),                                   // 原 CursorDelete
    ExtendLeftBy(usize),                             // 新增
    ExtendRightBy(usize),                            // 新增
    ExtendUpBy(usize),                               // 新增
    ExtendDownBy(usize),                             // 新增
    Cancel,                                          // 新增（Escape）
    Save,
    Quit,
    FocusNext,
    FocusPrev,
    AddAtNextMatch(String),                          // 原 CursorAddAtNextMatch（仍死预留）
    RemoveSecondary,                                 // 原 CursorRemoveSecondary（仍死预留）
}
```

`Direction` 枚举不变。

### 4.5 buffer 原语（`core/buffer.rs`）

采用 **head/anchor 独立方案**。原语重构：

**删除**（守恒 collapsed 版）：`move_selection_*`/`set_selection`。

**新增**（head/anchor 独立）：

```rust
/// 移动 head，不碰 anchor（extend 语义：selection 变非空）。
pub fn move_head_left(&self, sel: &mut Selection, n: usize) {
    self.move_cursor_left(&mut sel.head, n);
    // 不碰 anchor
}
// move_head_right/up/down/by 同理

/// 设 head，不碰 anchor。
pub fn set_head(&self, sel: &mut Selection, char_idx: usize, line_idx: usize) {
    self.set_cursor(&mut sel.head, char_idx, line_idx);
}

/// anchor = head（collapsed 守恒，由调用方决定时机）。
pub fn collapse_to_head(sel: &mut Selection) {
    sel.anchor = sel.head;
}
```

**保留启用**：`recompute_selection`（recompute head + anchor 独立，去掉 `dead_code`）。

**重写**：

```rust
/// 在每个 selection 插入文本：非空时先删 [min,max] 再插入，head 到插入末尾，collapse。
/// 空时在 head 点插入，head 前移 text_len，collapse。
pub fn insert_at_selections(&mut self, selections: &mut Selections, text: &str) {
    let text_len = text.chars().count();
    // 1) 非空 selection 先删 range（按 min 降序，避免索引偏移）
    let mut del_ranges: Vec<(usize, usize)> = selections.all().map(|s| {
        if s.anchor != s.head {
            let (a, b) = (s.anchor.char_index, s.head.char_index);
            (a.min(b), a.max(b))
        } else {
            (s.head.char_index, s.head.char_index) // 空：不删
        }
    }).collect();
    del_ranges.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    del_ranges.dedup();
    for (start, end) in del_ranges {
        if end > start { self.rope.remove(start..end); }
    }
    // 2) 在 min 端点插入（空 selection 在 head）
    let mut insert_indices: Vec<usize> = selections.all().map(|s| {
        s.anchor.char_index.min(s.head.char_index)
    }).collect();
    insert_indices.sort_unstable_by(|a, b| b.cmp(a));
    insert_indices.dedup();
    for idx in insert_indices {
        self.rope.insert(idx, text);
    }
    self.modified = true;
    // 3) 更新每个 selection：head = 插入点 + text_len，collapse
    for sel in selections.all_mut() {
        let insert_at = sel.anchor.char_index.min(sel.head.char_index);
        sel.head.char_index = insert_at + text_len;
        self.recompute_cursor(&mut sel.head);
        Self::collapse_to_head(sel);
    }
}

/// 在每个 selection 删除：非空时删 [min,max]，head=min，collapse。
/// 空时按方向删 n，head 回退，collapse。
pub fn delete_at_selections(&mut self, selections: &mut Selections, n: isize) {
    let len = self.rope.len_chars();
    // 1) 计算每个 selection 的删除区间
    let mut ranges: Vec<(usize, usize)> = selections.all().map(|s| {
        if s.anchor != s.head {
            let (a, b) = (s.anchor.char_index, s.head.char_index);
            (a.min(b), a.max(b))
        } else {
            // 空：按方向删 n
            let ci = s.head.char_index.min(len);
            if n < 0 {
                let start = ci.saturating_sub((-n) as usize);
                (start, ci)
            } else {
                let end = (ci + n as usize).min(len);
                (ci, end)
            }
        }
    }).collect();
    ranges.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    ranges.dedup();
    for (start, end) in ranges {
        if end > start { self.rope.remove(start..end); }
    }
    self.modified = true;
    // 2) 更新每个 selection
    for sel in selections.all_mut() {
        if sel.anchor != sel.head {
            // 非空：head = min 端点
            sel.head.char_index = sel.anchor.char_index.min(sel.head.char_index);
        } else if n < 0 {
            // 空 backward：head 回退
            sel.head.char_index = sel.head.char_index.saturating_sub((-n) as usize);
        }
        // 空 forward：head 不动（删除在 head 之后）
        self.recompute_cursor(&mut sel.head);
        Self::collapse_to_head(sel);
    }
}
```

**anchor 维护**（v0.3 简化）：extend（`move_head_*`）钉住 anchor 不动，head 由 `move_cursor_*` 内部 recompute；文本编辑后总 `collapse_to_head` 重置 anchor，所以 anchor 不会 stale。多 selection 索引偏移按"区间降序处理"（v0.3 单选区但代码 future-proof）。

**底层保留**：`move_cursor_*`/`set_cursor`/`recompute_cursor`（`pub(crate)`/`pub`）不变，供 `move_head_*` 复用。

### 4.6 executor（`app/executor.rs`）

```rust
pub fn execute(op: Operation, content: &mut dyn ContentHandler, selections: &mut Selections) {
    let Some(buf) = content.buffer_mut() else { return; };
    match op {
        Operation::MoveLeftBy(n) => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index { sel.anchor } else { sel.head };
                } else {
                    buf.move_head_left(sel, n);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        Operation::MoveRightBy(n) => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index { sel.anchor } else { sel.head };
                } else {
                    buf.move_head_right(sel, n);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        Operation::MoveUpBy(n) => {
            for sel in selections.all_mut() {
                buf.move_head_up(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        Operation::MoveDownBy(n) => {
            for sel in selections.all_mut() {
                buf.move_head_down(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        Operation::ExtendLeftBy(n)  => { for sel in selections.all_mut() { buf.move_head_left(sel, n); } }
        Operation::ExtendRightBy(n) => { for sel in selections.all_mut() { buf.move_head_right(sel, n); } }
        Operation::ExtendUpBy(n)    => { for sel in selections.all_mut() { buf.move_head_up(sel, n); } }
        Operation::ExtendDownBy(n)  => { for sel in selections.all_mut() { buf.move_head_down(sel, n); } }
        Operation::Cancel => {
            for sel in selections.all_mut() { Buffer::collapse_to_head(sel); }
            selections.retain_primary();
        }
        Operation::MoveTo { char_idx, line_idx } => {
            buf.set_head(selections.primary_mut(), char_idx, line_idx);
            Buffer::collapse_to_head(selections.primary_mut());
            selections.retain_primary();
        }
        Operation::InsertText(text) => buf.insert_at_selections(selections, &text),
        Operation::Delete(n)        => buf.delete_at_selections(selections, n),
        _ => {}
    }
}
```

Left/Right 收缩时 `head` 取 min/max 端点的**完整 `CursorPos`**（含已 recompute 的 row/col），无需再 recompute。

### 4.7 Canvas（`terminal/output.rs`）

```rust
pub trait Canvas {
    fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()>;
    fn clear_line(&mut self) -> io::Result<()>;
    fn write_str(&mut self, s: &str) -> io::Result<()>;
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn set_reverse(&mut self, on: bool) -> io::Result<()>;   // 新增
    fn flush(&mut self) -> io::Result<()>;
}

impl<W: Write> Canvas for Output<W> {
    fn set_reverse(&mut self, on: bool) -> io::Result<()> {
        use crossterm::style::{Attribute, SetAttribute};
        let attr = if on { Attribute::Reverse } else { Attribute::NoReverse };
        queue!(self.out, SetAttribute(attr))
    }
    // 其余不变
}
```

### 4.8 scene_renderer 选区高亮（`tui/scene_renderer.rs`）

`paint_item` 改：对每个 Host item，pull `query.selections(sid)`，若非空计算选区屏幕区间并反白画。

选区区间：`start = min(anchor,head)`（按 char_index 比）的 `(row,col)`，`end = max` 的 `(row,col)`。映射屏幕：`screen_row = row - vp.top_row + item.rect.y`，`screen_col = col - vp.left_col + item.rect.x`。

**跨行选区分段画**：
- 首行（`start.row`）：`start.col` → 行尾。
- 中间行：整行反白。
- 末行（`end.row`）：`0` → `end.col`。
- 同行（`start.row == end.row`）：`start.col` → `end.col`。

渲染方式：当前"逐行 `clear_line` + `write_str` 整行"改为"按高亮区间分段：反白段 `set_reverse(true)` + `write` + `set_reverse(false)`，非高亮段正常 `write`"。

viewport 裁剪：选区行不在可见范围（`row < vp.top_row` 或 `>= top_row + height`）不画。

光标定位不变——仍 `primary().head()`。非空选区时光标在 head（选区一端），反白高亮覆盖 `[min,max]`，光标可见。

非聚焦 space：v0.3 单视图，只 focused space 有非空 selection；其他 space pull 到默认 collapsed（不高亮）。

## 5. 数据流

### 5.1 事件流（输入 → 状态）

```
crossterm EventStream → Input → FrontendEvent::Key(KeyEvent::Shift(Arrow))
  → App::handle_event → Dispatcher.dispatch → Operation::ExtendLeftBy
  → executor.execute(op, content, view.selections_mut())
    → buf.move_head_left(sel, n)（不碰 anchor）→ selection 非空

普通方向键 → MoveLeftBy → executor（非空收缩到 min / 空左移 + collapse）
输入/backspace → InsertText/Delete → buf.insert/delete_at_selections（非空删 range + collapse）
Escape → Cancel → collapse_to_head + retain_primary
```

### 5.2 渲染流（状态 → 输出）

```
App::render:
  frontend.render(&scene, &query, focused)
    → SceneRenderer.render:
        ├─ TaffyEngine.layout(scene) → ResolvedScene
        ├─ query.selections(focused) → Selections
        │    → primary().head() 算 cursor 屏坐标 + ensure viewport
        ├─ 逐 Host item paint_item:
        │    ├─ query.lines(cid, range) → Vec<String>
        │    ├─ query.selections(sid) → 若非空，算 [min,max] 屏幕区间
        │    └─ 按高亮分段画（反白段 set_reverse(true)+write+set_reverse(false)）
        └─ 光标定位 primary().head()
```

## 6. 关键决策记录

| 决策 | 选择 | 理由 |
|---|---|---|
| 范围深度 | 最小真选区（建选区 + 按 selection 编辑 + 高亮，单选区） | 让 selection 名副其实的最小完整集；多光标/normalize 留后续，YAGNI |
| core 原语承载扩展 | head/anchor 独立（`move_head_*` + `collapse_to_head`） | 原语最薄最灵活；守恒语义上移 executor（语义层）；破坏现有守恒契约但更清晰 |
| 建选区交互 | VSCode 风（shift+方向键，anchor 钉住 head 移动） | 用户最熟悉；与 collapsed 模型平滑过渡 |
| 取消选区 Left/Right | 收缩到端点不额外移（min/max） | VSCode/Word/浏览器主流；backward 选区不跳意外位置；落点与朝向无关 |
| 取消选区 Up/Down | collapse + move（统一） | 无"端点"语义；head 移动后 anchor 跟随 collapse |
| 非空编辑语义 | 替换（删 range + 插入/仅删） | 标准；编辑消费选区后 collapse |
| anchor 维护 | 靠"编辑后总 collapse"简化 | v0.3 单选区不会 stale；多 selection 留 v0.4 |
| Operation 命名 | 全量去 Cursor 前缀（Move/Extend 对称） | Operation 不再绑定 Cursor 概念；Move/Extend 对称清晰；改动面大但机械 |
| shift 修饰范围 | 仅 shift+方向键 + Escape | YAGNI；shift+其他键落 Unknown |
| Canvas 反白 API | `set_reverse(bool)` 单方法 | 最小够用；未来加颜色再扩 |
| 跨行选区高亮 | 分段（首行起 col→行尾、中间整行、末行 0→end col） | 终端选区标准画法 |
| 不变量保持 | ranges.len()==1、primary_index==0、无 normalize、无 direction | 单选区；多光标/normalize/direction 留后续 |

## 7. 测试策略

### 7.1 测试分层

- **`protocol/key_event.rs`**：shift+方向键 → `Shift(ArrowKey)`；shift+其他键 → `Unknown`；普通方向键不变；shift+Left 不再被当普通 Arrow（修 bug 点）。
- **`protocol/selection.rs`**：`is_empty`（去 dead_code 后仍测）；非空 selection 构造；现有测试不变。
- **`core/operation.rs`**：新变体 `ExtendLeftBy`/`Cancel` 构造；现有变体去 `Cursor` 前缀后全改构造测试。
- **`core/buffer.rs`**：
  - `move_head_*`：head 移动 + **anchor 不变**（断言 anchor 不动）。
  - `move_head_*` 后 selection 非空（`anchor != head`）。
  - `collapse_to_head`：`anchor == head`。
  - `insert_at_selections` 非空：删 `[min,max]` + 插入 + collapse（断言文本 + head + anchor==head）。
  - `delete_at_selections` 非空：删 `[min,max]` + head=min + collapse。
  - `insert/delete_at_selections` 空：同现状（点操作）。
  - 现有 `move_selection_*` 测试改写为 `move_head_*` + `collapse_to_head`。
- **`app/executor.rs`**：
  - `MoveLeftBy` 非空收缩到 min（不左移）；空左移 n。
  - `MoveRightBy` 非空收缩到 max；空右移。
  - `MoveUpBy/MoveDownBy`：collapse + move（非空/空统一）。
  - `Extend*`：head 移动 + anchor 不变（不 collapse）。
  - `Cancel`：collapse to head + retain_primary。
  - `InsertText` 非空替换 range（删 + 插 + collapse）。
- **`tui/scene_renderer.rs`**：
  - `StubQuery` 返回非空 selection，断言输出含反白 VT escape（`Attribute::Reverse` 序列 `\x1b[7m`）。
  - 跨行选区高亮（首行/中间/末行分段）。
  - viewport 裁剪：选区行不在可见范围不画反白。
  - 空 selection 不画反白（现状不变）。
- **集成（`tui/headless.rs` 驱动）**：shift+方向键建选区 → 输入替换选区 → 字节断言；Escape 取消选区后方向键正常移动。

### 7.2 守恒守护

每个编辑原语测试断言操作后语义正确（非空编辑后 `anchor==head` collapsed；extend 后 `anchor` 不变）。漏点会被测试捕获。

## 8. 迁移影响清单

### 新建

无新文件（全在现有文件改）。

### 改写

- `protocol/key_event.rs`：+ `Shift(ArrowKey)` 变体；`translate_key` 检查 SHIFT 分流。
- `protocol/selection.rs`：`is_empty`/`recompute_selection` 去掉 `#[allow(dead_code)]`。
- `core/operation.rs`：全量去 `Cursor` 前缀；+ `ExtendLeftBy/RightBy/UpBy/DownBy` + `Cancel`。
- `core/buffer.rs`：删 `move_selection_*`/`set_selection`；+ `move_head_*`/`set_head`/`collapse_to_head`；重写 `insert_at_selections`/`delete_at_selections`；`recompute_selection` 去 dead_code；`default_buffer_keymap` 绑定改名 + shift+方向键 → `Extend*` + Escape → `Cancel`。
- `app/executor.rs`：分发新 Operation（收缩/扩展/取消/替换）。
- `app/dispatcher.rs`：`default_global_keymap` 绑定值去 Cursor 前缀（`Quit`/`Save` 实际不变）。
- `terminal/output.rs`：`Canvas` + `set_reverse`；`Output<W>` 实现。
- `tui/scene_renderer.rs`：`paint_item` 选区高亮分段反白 + viewport 裁剪。
- 所有测试：`Cursor*` 变体 → 去前缀；`move_selection_*` → `move_head_*` + `collapse_to_head`；新增 shift+方向键/extend/非空编辑/反白高亮用例。

### 不变

- `protocol/space.rs`/`scene.rs`/`geometry.rs`/`ids.rs`/`status.rs`/`viewport.rs`/`frontend_event.rs`
- `protocol/content_query.rs`（`selections(sid)` 已就位）
- `protocol/selection.rs` 类型结构（`Selection`/`Selections` 字段不变，仅去 dead_code）
- `core/keymap.rs`/`core/content.rs`/`core/status_bar.rs`
- `app/mod.rs`/`app/view.rs`/`app/content.rs`（View 实体、AppQuery、App 结构不变）
- `Frontend` trait 签名
- `TuiFrontend`/`HeadlessFrontend` 结构

## 9. Non-goals / Follow-up

- **多光标**：`AddAtNextMatch`/`RemoveSecondary` 仍死预留；`ranges.len()>1` 不实现；`retain_primary` 仍 v0.2 noop 语义保留。
- **normalize（sort + merge overlapping + 调 primary_index）**：单选区无需；多光标时实现。
- **`direction` 字段**：永久不加——`anchor/head` 已隐含方向。
- **anchor 跨编辑 stale 处理**：v0.3 靠"编辑后总 collapse"简化；多 selection/别处编辑的 stale 留 v0.4。
- **shift+其他键**（shift+char 大写、shift+Enter 等）：本次不处理，落 `Unknown`。
- **鼠标选区**（拖拽建选区）：前端无鼠标事件支持，留后续。
- **selection 增量协议 / 零拷贝 pull**：沿用前置重构 spec 的 follow-up。
- **选区颜色/粗体等其他属性**：本次仅反白；Canvas 未来按需扩 `set_attribute`。

## 10. 风险

1. **Operation 全量重命名跨多文件**：`Cursor*` → 去前缀涉及 `operation.rs`/`buffer.rs`/`executor.rs`/`dispatcher.rs`/`keymap` 绑定 + 所有测试。漏一处编译错。**缓解**：机械重命名，`cargo check --all` 全量验证；先改 `operation.rs` 定义，编译错误驱动逐文件改。
2. **选区高亮跨行分段 + viewport 偏移易错**：`start.col`/`end.col` 减 `vp.left_col`、`row` 减 `vp.top_row` 映射屏幕，跨行分段边界易 off-by-one。**缓解**：renderer 单测断言反白 escape 位置 + 跨行用例（首行/中间/末行/同行）+ viewport 裁剪用例。
3. **anchor stale**：v0.3 靠"编辑后总 collapse"简化，单选区不会暴露。但若未来引入多 selection 或别处编辑（如全局替换），anchor 的 char_index/row/col 会 stale。**缓解**：v0.3 单选区不变量守住；`insert/delete_at_selections` 注释标明"编辑后 collapse 重置 anchor"；多 selection 留 v0.4 时重审。
4. **shift+方向键 translate 跨终端差异**：crossterm 在不同终端对 shift+方向键的 `KeyCode`/`KeyModifiers` 报告可能不同（某些终端 shift+方向键编码为 CSI u 或其他序列，crossterm 可能解析为 `Unknown`）。**缓解**：单测 `translate_key` 逻辑（输入构造的 `CrosstermKey` 验证映射）；实际终端（Windows Terminal / PowerShell）手动验证 shift+方向键建选区生效；若某些终端不识别，作为已知限制记录，不阻塞核心实现。
5. **MoveLeftBy/RightBy 收缩分支与 Up/Down 统一分支不对称**：executor 中 Left/Right 分空/非空，Up/Down 不分。不一致可能让维护者困惑。**缓解**：executor 注释说明"Left/Right 有端点语义收缩，Up/Down 无端点统一 collapse+move"；测试覆盖四个方向的非空行为。
