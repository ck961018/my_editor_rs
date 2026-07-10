# 真选区编辑 + 高亮渲染 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 selection 模型在前端与 core 层"名副其实"——支持 shift+方向键建选区、按选区替换编辑、反白高亮渲染（v0.3 最小真选区，单选区）。

**Architecture:** head/anchor 独立原语（`move_head_*`/`set_head`/`collapse_to_head`），守恒语义上移 executor；VSCode 风交互（shift+方向键扩展、普通方向键取消、Escape 取消、非空编辑替换 range）；Canvas 加 `set_reverse`，scene_renderer 跨行分段反白高亮。Operation 全量去 `Cursor` 前缀，Move/Extend 对称。

**Tech Stack:** Rust 2024, ropey, crossterm 0.29, taffy 0.11, tokio

**Spec:** `docs/superpowers/specs/2026-07-07-selection-editing-and-highlight-design.md`

---

## 前置说明

- **起点**：当前 HEAD = `a715af9`（spec 已提交）。`src/core/buffer.rs` 有一处未提交的 lint 修复（去掉 `move_selection_right`/`recompute_selection`/`move_selection_down` 调用处冗余的 `&mut`）。Task 4 会重写这些测试行，自然吸收该改动；执行计划前无需单独处理。
- **运行命令**：所有 `cargo` 命令在仓库根 `D:\workspace\my_editor_rs` 执行。PowerShell 中 `cargo test` 直接可用。
- **任务依赖**：Task 1→2→3→4→5→6→7→8 顺序执行。Task 3（Operation 重命名）和 Task 4（buffer 原语）改动面大但机械，必须 `cargo test` 全绿才进下一个。
- **不变量**（全程保持）：`ranges.len()==1`、`primary_index==0`、无 normalize、无 `direction` 字段。编辑后总 `collapse_to_head` 重置 anchor。

## 文件结构

| 文件 | 责任 | 本计划改动 |
|---|---|---|
| `src/protocol/selection.rs` | `Selection`/`Selections` 数据模型 | 去 `is_empty` dead_code |
| `src/protocol/key_event.rs` | `KeyEvent` + `translate_key` | + `Shift(ArrowKey)` 变体 + SHIFT 分流 |
| `src/core/operation.rs` | `Operation` 枚举 | 全量去 `Cursor` 前缀 + `Extend*`/`Cancel` |
| `src/core/buffer.rs` | 编辑原语 + keymap | 删 `move_selection_*`/`set_selection`；+ `move_head_*`/`set_head`/`collapse_to_head`；重写 insert/delete；keymap 改名 + shift/escape |
| `src/app/executor.rs` | Operation 分发 | 新分发（收缩/扩展/取消/替换） |
| `src/app/dispatcher.rs` | 全局 keymap + 捕获链 | 测试用例去 `Cursor` 前缀 |
| `src/terminal/output.rs` | `Canvas` trait + `Output<W>` | + `set_reverse(bool)` |
| `src/tui/scene_renderer.rs` | layout + paint | 选区高亮分段反白 + viewport 裁剪 |
| `src/tui/headless.rs` | 测试用 frontend | 不变（集成测试驱动） |

## 任务清单

- Task 1: `selection.rs` 去掉 `is_empty` 的 `dead_code`
- Task 2: `key_event.rs` 新增 `Shift(ArrowKey)` + `translate_key` SHIFT 分流
- Task 3: `operation.rs` 全量去 `Cursor` 前缀 + 新增 `Extend*`/`Cancel`（机械重命名，行为不变）
- Task 4: `buffer.rs` head/anchor 独立原语 + insert/delete 非空重写 + executor 适配（保持 collapsed 行为）
- Task 5: `executor.rs` 真选区分发（收缩/扩展/取消）+ buffer keymap 绑定 shift/escape
- Task 6: `output.rs` `Canvas::set_reverse` + `Output<W>` 实现
- Task 7: `scene_renderer.rs` 选区高亮分段反白 + viewport 裁剪
- Task 8: 集成测试（headless 驱动 shift+方向键建选区→替换→字节断言）

---

<!-- 任务详情按 Task 1..8 逐个用 Edit 追加到此处下方 -->

## Task 1: selection.rs 去掉 `is_empty` 的 `dead_code`

`Selection::is_empty` 已在 `selection.rs` 自身测试中被调用（`collapsed_is_empty`/`non_empty_selection`），`#[allow(dead_code)]` 已冗余。本任务仅清理该标注，为后续真选区启用做语义标记。无行为变化。

**Files:**
- Modify: `src/protocol/selection.rs:26-27`

- [ ] **Step 1: 移除 `is_empty` 上的 `#[allow(dead_code)]`**

把 `src/protocol/selection.rs` 第 26-27 行：

```rust
    #[allow(dead_code)] // v0.2 预留：真选区时判空
    pub fn is_empty(&self) -> bool { self.anchor == self.head }
```

改为：

```rust
    pub fn is_empty(&self) -> bool { self.anchor == self.head }
```

- [ ] **Step 2: 运行测试验证不回归**

Run: `cargo test protocol::selection`
Expected: PASS（`selection.rs` 内 7 个测试全过，无 `dead_code` 警告）

- [ ] **Step 3: 全量 check 无新警告**

Run: `cargo check --all 2>&1 | Select-String "warning"`
Expected: 无与 `is_empty` 相关的 `dead_code` 警告（其他既有警告不变）

- [ ] **Step 4: Commit**

```powershell
git add src/protocol/selection.rs
git commit -m "refactor(selection): 去掉 is_empty 的 dead_code 标注（已启用）"
```

## Task 2: key_event.rs 新增 `Shift(ArrowKey)` + `translate_key` SHIFT 分流

`KeyEvent` 新增 `Shift(ArrowKey)` 变体；`translate_key` 在方向键分支检查 `KeyModifiers::SHIFT`，shift+方向键 → `Shift(ArrowKey)`，普通方向键不变。修掉当前 `KeyCode::Left` 分支不检查 modifiers、shift+Left 被当普通 Arrow 丢 shift 的 bug。shift+其他键保持落 `Unknown`（YAGNI）。

**Files:**
- Modify: `src/protocol/key_event.rs:17-51`（enum + translate_key）
- Test: `src/protocol/key_event.rs`（`#[cfg(test)] mod tests`）

- [ ] **Step 1: 写失败测试**

在 `src/protocol/key_event.rs` 的 `mod tests` 末尾（`fn function_key_is_unknown` 之后、闭合 `}` 之前）追加：

```rust
    #[test]
    fn shift_arrow_becomes_shift_variant() {
        assert_eq!(translate_key(key(KeyCode::Left, KeyModifiers::SHIFT)), KeyEvent::Shift(ArrowKey::Left));
        assert_eq!(translate_key(key(KeyCode::Right, KeyModifiers::SHIFT)), KeyEvent::Shift(ArrowKey::Right));
        assert_eq!(translate_key(key(KeyCode::Up, KeyModifiers::SHIFT)), KeyEvent::Shift(ArrowKey::Up));
        assert_eq!(translate_key(key(KeyCode::Down, KeyModifiers::SHIFT)), KeyEvent::Shift(ArrowKey::Down));
    }

    #[test]
    fn shift_other_keys_still_unknown() {
        // shift+char 不在本次处理范围，落 Unknown
        assert_eq!(translate_key(key(KeyCode::Char('a'), KeyModifiers::SHIFT)), KeyEvent::Unknown);
        assert_eq!(translate_key(key(KeyCode::Enter, KeyModifiers::SHIFT)), KeyEvent::Unknown);
    }

    #[test]
    fn arrow_without_shift_unchanged() {
        // 回归：无 shift 时方向键仍为 Arrow（不被 shift 分流误伤）
        assert_eq!(translate_key(key(KeyCode::Left, KeyModifiers::empty())), KeyEvent::Arrow(ArrowKey::Left));
        assert_eq!(translate_key(key(KeyCode::Down, KeyModifiers::empty())), KeyEvent::Arrow(ArrowKey::Down));
    }
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test protocol::key_event`
Expected: 编译失败——`KeyEvent::Shift` 变体不存在（`no variant or associated item named Shift`）。

- [ ] **Step 3: 给 `KeyEvent` 加 `Shift(ArrowKey)` 变体**

把 `src/protocol/key_event.rs` 第 17-26 行的 enum：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyEvent {
    Char(u8),
    Ctrl(CtrlKey),
    Arrow(ArrowKey),
    Backspace,
    Enter,
    Escape,
    Unknown,
}
```

改为：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyEvent {
    Char(u8),
    Ctrl(CtrlKey),
    Arrow(ArrowKey),
    Shift(ArrowKey),
    Backspace,
    Enter,
    Escape,
    Unknown,
}
```

- [ ] **Step 4: 改 `translate_key` 方向键分支检查 SHIFT**

把 `src/protocol/key_event.rs` 第 45-48 行：

```rust
        KeyCode::Left => KeyEvent::Arrow(ArrowKey::Left),
        KeyCode::Right => KeyEvent::Arrow(ArrowKey::Right),
        KeyCode::Up => KeyEvent::Arrow(ArrowKey::Up),
        KeyCode::Down => KeyEvent::Arrow(ArrowKey::Down),
```

改为：

```rust
        KeyCode::Left if k.modifiers.contains(KeyModifiers::SHIFT) => KeyEvent::Shift(ArrowKey::Left),
        KeyCode::Left => KeyEvent::Arrow(ArrowKey::Left),
        KeyCode::Right if k.modifiers.contains(KeyModifiers::SHIFT) => KeyEvent::Shift(ArrowKey::Right),
        KeyCode::Right => KeyEvent::Arrow(ArrowKey::Right),
        KeyCode::Up if k.modifiers.contains(KeyModifiers::SHIFT) => KeyEvent::Shift(ArrowKey::Up),
        KeyCode::Up => KeyEvent::Arrow(ArrowKey::Up),
        KeyCode::Down if k.modifiers.contains(KeyModifiers::SHIFT) => KeyEvent::Shift(ArrowKey::Down),
        KeyCode::Down => KeyEvent::Arrow(ArrowKey::Down),
```

- [ ] **Step 5: 运行测试验证通过**

Run: `cargo test protocol::key_event`
Expected: PASS（原 6 个 + 新增 3 个 = 9 个测试全过）

- [ ] **Step 6: 全量 check 无新警告**

Run: `cargo check --all 2>&1 | Select-String "warning"`
Expected: 无新增警告（`Shift` 变体已被 `translate_key` 构造，非 dead_code）

- [ ] **Step 7: Commit**

```powershell
git add src/protocol/key_event.rs
git commit -m "feat(key_event): 新增 Shift(ArrowKey) 变体 + translate_key SHIFT 分流"
```

## Task 3: operation.rs 全量去 `Cursor` 前缀 + 新增 `Extend*`/`Cancel`（机械重命名，行为不变）

`Operation` 枚举全量去 `Cursor` 前缀（`CursorMoveLeftBy`→`MoveLeftBy`、`CursorInsertText`→`InsertText`、`CursorDelete`→`Delete`、`CursorMoveBy`→`MoveBy`、`CursorMoveTo`→`MoveTo`、`CursorAddAtNextMatch`→`AddAtNextMatch`、`CursorRemoveSecondary`→`RemoveSecondary`），新增 `ExtendLeftBy/RightBy/UpBy/DownBy(usize)` + `Cancel`（本任务暂不接线，标 `dead_code`，Task 5 启用）。同步改 `buffer.rs`（keymap + `default_binding`）、`executor.rs`（match 分支）、`dispatcher.rs` 测试、各处测试中的变体名。**行为不变**——executor 仍调用 `move_selection_*`/`set_selection`，只是 match 分支名改了。`cargo test` 必须全绿。

> 这是机械重命名任务，没有"新行为"可 TDD。验证标准 = 全量 `cargo test` 通过 + 无新警告。

**Files:**
- Modify: `src/core/operation.rs`（枚举 + 测试）
- Modify: `src/core/buffer.rs`（`default_binding` + `default_buffer_keymap` + 测试 `default_binding_char_to_insert`）
- Modify: `src/app/executor.rs`（match 分支名 + 测试）
- Modify: `src/app/dispatcher.rs`（测试中 `CursorInsertText`/`CursorMoveLeftBy`）

- [ ] **Step 1: 改 `operation.rs` 枚举定义**

把 `src/core/operation.rs` 第 10-32 行整个 enum：

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    #[allow(dead_code)] // 预留：chars+lines 复合移动，v0.2 仅用 Left/Right/Up/Down
    CursorMoveBy { chars: isize, lines: isize },
    CursorMoveLeftBy(usize),
    CursorMoveRightBy(usize),
    CursorMoveUpBy(usize),
    CursorMoveDownBy(usize),
    #[allow(dead_code)] // 预留：绝对定位，v0.2 无键绑定构造
    CursorMoveTo { char_idx: usize, line_idx: usize },
    CursorInsertText(String),
    CursorDelete(isize),
    Save,
    Quit,
    #[allow(dead_code)] // v0.2 预留：多 space 焦点切换
    FocusNext,
    #[allow(dead_code)] // v0.2 预留：多 space 焦点切换
    FocusPrev,
    #[allow(dead_code)] // v0.2 预留：多光标下一个匹配
    CursorAddAtNextMatch(String),
    #[allow(dead_code)] // v0.2 预留：移除副光标
    CursorRemoveSecondary,
}
```

改为：

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    #[allow(dead_code)] // 预留：chars+lines 复合移动
    MoveBy { chars: isize, lines: isize },
    MoveLeftBy(usize),
    MoveRightBy(usize),
    MoveUpBy(usize),
    MoveDownBy(usize),
    #[allow(dead_code)] // 预留：绝对定位
    MoveTo { char_idx: usize, line_idx: usize },
    InsertText(String),
    Delete(isize),
    #[allow(dead_code)] // Task 5 启用：shift+方向键扩展选区
    ExtendLeftBy(usize),
    #[allow(dead_code)] // Task 5 启用
    ExtendRightBy(usize),
    #[allow(dead_code)] // Task 5 启用
    ExtendUpBy(usize),
    #[allow(dead_code)] // Task 5 启用
    ExtendDownBy(usize),
    #[allow(dead_code)] // Task 5 启用：Escape 取消选区
    Cancel,
    Save,
    Quit,
    #[allow(dead_code)] // 预留：多 space 焦点切换
    FocusNext,
    #[allow(dead_code)] // 预留：多 space 焦点切换
    FocusPrev,
    #[allow(dead_code)] // 预留：多光标下一个匹配
    AddAtNextMatch(String),
    #[allow(dead_code)] // 预留：移除副光标
    RemoveSecondary,
}
```

- [ ] **Step 2: 改 `operation.rs` 测试**

把 `src/core/operation.rs` 的 `operation_clone_eq` 与 `operation_variants_construct` 测试（第 45-59 行）：

```rust
    #[test]
    fn operation_clone_eq() {
        let a = Operation::CursorInsertText("x".to_string());
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn operation_variants_construct() {
        let _ = Operation::CursorMoveBy { chars: 1, lines: -1 };
        let _ = Operation::CursorMoveTo { char_idx: 0, line_idx: 0 };
        let _ = Operation::CursorDelete(-1);
        let _ = Operation::Save;
        let _ = Operation::CursorAddAtNextMatch("foo".to_string());
        let _ = Operation::CursorRemoveSecondary;
    }
```

改为：

```rust
    #[test]
    fn operation_clone_eq() {
        let a = Operation::InsertText("x".to_string());
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn operation_variants_construct() {
        let _ = Operation::MoveBy { chars: 1, lines: -1 };
        let _ = Operation::MoveTo { char_idx: 0, line_idx: 0 };
        let _ = Operation::Delete(-1);
        let _ = Operation::Save;
        let _ = Operation::AddAtNextMatch("foo".to_string());
        let _ = Operation::RemoveSecondary;
    }
```

- [ ] **Step 3: 改 `buffer.rs` 的 `default_binding` 与 `default_buffer_keymap`**

把 `src/core/buffer.rs` 第 275-280 行 `default_binding`：

```rust
    fn default_binding(&self, key: KeyEvent) -> Option<Operation> {
        match key {
            KeyEvent::Char(ch) => Some(Operation::CursorInsertText((ch as char).to_string())),
            _ => None,
        }
    }
```

改为：

```rust
    fn default_binding(&self, key: KeyEvent) -> Option<Operation> {
        match key {
            KeyEvent::Char(ch) => Some(Operation::InsertText((ch as char).to_string())),
            _ => None,
        }
    }
```

把 `src/core/buffer.rs` 第 285-294 行 `default_buffer_keymap`：

```rust
fn default_buffer_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::Enter, Operation::CursorInsertText("\n".to_string()));
    km.bind(KeyEvent::Backspace, Operation::CursorDelete(-1));
    km.bind(KeyEvent::Arrow(ArrowKey::Left), Operation::CursorMoveLeftBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Right), Operation::CursorMoveRightBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Up), Operation::CursorMoveUpBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Down), Operation::CursorMoveDownBy(1));
    km
}
```

改为：

```rust
fn default_buffer_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::Enter, Operation::InsertText("\n".to_string()));
    km.bind(KeyEvent::Backspace, Operation::Delete(-1));
    km.bind(KeyEvent::Arrow(ArrowKey::Left), Operation::MoveLeftBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Right), Operation::MoveRightBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Up), Operation::MoveUpBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Down), Operation::MoveDownBy(1));
    km
}
```

- [ ] **Step 4: 改 `buffer.rs` 测试 `default_binding_char_to_insert`**

把 `src/core/buffer.rs` 第 393-398 行：

```rust
    #[test]
    fn default_binding_char_to_insert() {
        let b = Buffer::new();
        let op = b.default_binding(KeyEvent::Char(b'a')).unwrap();
        assert_eq!(op, Operation::CursorInsertText("a".to_string()));
    }
```

改为：

```rust
    #[test]
    fn default_binding_char_to_insert() {
        let b = Buffer::new();
        let op = b.default_binding(KeyEvent::Char(b'a')).unwrap();
        assert_eq!(op, Operation::InsertText("a".to_string()));
    }
```

- [ ] **Step 5: 改 `executor.rs` match 分支名（行为不变）**

把 `src/app/executor.rs` 第 8-32 行整个 match：

```rust
    match op {
        Operation::CursorMoveBy { chars, lines } => {
            for sel in selections.all_mut() { buf.move_selection_by(sel, chars, lines); }
        }
        Operation::CursorMoveLeftBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_left(sel, n); }
        }
        Operation::CursorMoveRightBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_right(sel, n); }
        }
        Operation::CursorMoveUpBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_up(sel, n); }
        }
        Operation::CursorMoveDownBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_down(sel, n); }
        }
        Operation::CursorMoveTo { char_idx, line_idx } => {
            buf.set_selection(selections.primary_mut(), char_idx, line_idx);
            selections.retain_primary();
        }
        Operation::CursorInsertText(text) => buf.insert_at_selections(selections, &text),
        Operation::CursorDelete(n) => buf.delete_at_selections(selections, n),
        // 全局/多光标变体不进 executor
        _ => {}
    }
```

改为：

```rust
    match op {
        Operation::MoveBy { chars, lines } => {
            for sel in selections.all_mut() { buf.move_selection_by(sel, chars, lines); }
        }
        Operation::MoveLeftBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_left(sel, n); }
        }
        Operation::MoveRightBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_right(sel, n); }
        }
        Operation::MoveUpBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_up(sel, n); }
        }
        Operation::MoveDownBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_down(sel, n); }
        }
        Operation::MoveTo { char_idx, line_idx } => {
            buf.set_selection(selections.primary_mut(), char_idx, line_idx);
            selections.retain_primary();
        }
        Operation::InsertText(text) => buf.insert_at_selections(selections, &text),
        Operation::Delete(n) => buf.delete_at_selections(selections, n),
        // 全局/多光标变体不进 executor
        _ => {}
    }
```

- [ ] **Step 6: 改 `executor.rs` 测试中的变体名**

把 `src/app/executor.rs` 测试模块（第 46-95 行）中所有 `Operation::Cursor*` 替换为去前缀版本。具体四处：

`insert_text_changes_buffer_and_selection` 中：
```rust
        execute(Operation::CursorInsertText("hi".to_string()), &mut buf, &mut s);
```
→
```rust
        execute(Operation::InsertText("hi".to_string()), &mut buf, &mut s);
```

`delete_left_removes_char` 中：
```rust
        execute(Operation::CursorDelete(-1), &mut buf, &mut s);
```
→
```rust
        execute(Operation::Delete(-1), &mut buf, &mut s);
```

`move_right_advances_head` 中：
```rust
        execute(Operation::CursorMoveRightBy(1), &mut buf, &mut s);
```
→
```rust
        execute(Operation::MoveRightBy(1), &mut buf, &mut s);
```

`move_to_retains_primary_clears_secondaries` 中：
```rust
        execute(Operation::CursorMoveTo { char_idx: 0, line_idx: 0 }, &mut buf, &mut s);
```
→
```rust
        execute(Operation::MoveTo { char_idx: 0, line_idx: 0 }, &mut buf, &mut s);
```

- [ ] **Step 7: 改 `dispatcher.rs` 测试中的变体名**

`src/app/dispatcher.rs` 测试模块中三处 `Operation::CursorInsertText` → `Operation::InsertText`，一处 `Operation::CursorMoveLeftBy` → `Operation::MoveLeftBy`：

`char_falls_through_to_default_binding`（第 137 行）：
```rust
        assert_eq!(op, Operation::CursorInsertText("a".to_string()));
```
→
```rust
        assert_eq!(op, Operation::InsertText("a".to_string()));
```

`buffer_keymap_enter_inserts_newline`（第 144 行）：
```rust
        assert_eq!(op, Operation::CursorInsertText("\n".to_string()));
```
→
```rust
        assert_eq!(op, Operation::InsertText("\n".to_string()));
```

`buffer_keymap_arrow_left`（第 151 行）：
```rust
        assert_eq!(op, Operation::CursorMoveLeftBy(1));
```
→
```rust
        assert_eq!(op, Operation::MoveLeftBy(1));
```

`content_overrides_global`（第 173、175 行）两处：
```rust
            .bind(KeyEvent::Ctrl(CtrlKey::Q), Operation::CursorInsertText("q".to_string()));
        let op = d.dispatch(KeyEvent::Ctrl(CtrlKey::Q), focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::CursorInsertText("q".to_string()));
```
→
```rust
            .bind(KeyEvent::Ctrl(CtrlKey::Q), Operation::InsertText("q".to_string()));
        let op = d.dispatch(KeyEvent::Ctrl(CtrlKey::Q), focused, &scene, &contents).unwrap();
        assert_eq!(op, Operation::InsertText("q".to_string()));
```

- [ ] **Step 7b: 改 `keymap.rs` 测试中的变体名**

`src/core/keymap.rs` 测试模块（第 38-89 行）有四处 `Cursor*` 引用，必须同步去前缀，否则 Task 3 的全量 `cargo test` 会编译失败。

`bind_and_lookup_operation`（第 46-48 行）两处：
```rust
        km.bind(KeyEvent::Enter, Operation::CursorInsertText("\n".to_string()));
        let b = km.lookup(KeyEvent::Enter).unwrap();
        assert_eq!(b, &KeyBinding::Operation(Operation::CursorInsertText("\n".to_string())));
```
→
```rust
        km.bind(KeyEvent::Enter, Operation::InsertText("\n".to_string()));
        let b = km.lookup(KeyEvent::Enter).unwrap();
        assert_eq!(b, &KeyBinding::Operation(Operation::InsertText("\n".to_string())));
```

`unbind_removes`（第 60 行）：
```rust
        km.bind(KeyEvent::Backspace, Operation::CursorDelete(-1));
```
→
```rust
        km.bind(KeyEvent::Backspace, Operation::Delete(-1));
```

`keymap_clone_eq`（第 85 行）：
```rust
        km.bind(KeyEvent::Arrow(ArrowKey::Left), Operation::CursorMoveLeftBy(1));
```
→
```rust
        km.bind(KeyEvent::Arrow(ArrowKey::Left), Operation::MoveLeftBy(1));
```

- [ ] **Step 7c: 改 `app/mod.rs` 的 `Cursor*` 引用**

`src/app/mod.rs` 第 142 行 `execute_operation` 中：

```rust
            Operation::CursorAddAtNextMatch(_) | Operation::CursorRemoveSecondary => {}
```

→

```rust
            Operation::AddAtNextMatch(_) | Operation::RemoveSecondary => {}
```

- [ ] **Step 8: 全量编译验证**

Run: `cargo check --all`
Expected: 编译通过（所有 `Cursor*` 引用已替换，无 `cannot find variant` 错误）。

- [ ] **Step 9: 全量测试验证不回归**

Run: `cargo test --all`
Expected: 全部测试通过（`Extend*`/`Cancel` 虽未接线但 `dead_code` 抑制警告，不影响测试）。

- [ ] **Step 10: 检查无残留 `Cursor` 前缀引用**

Run: `Select-String -Path "src\core\operation.rs","src\core\buffer.rs","src\core\keymap.rs","src\app\executor.rs","src\app\dispatcher.rs","src\app\mod.rs" -Pattern "Operation::Cursor"`
Expected: 无匹配（所有 `Operation::Cursor*` 已去前缀）。

- [ ] **Step 11: Commit**

```powershell
git add src/core/operation.rs src/core/buffer.rs src/app/executor.rs src/app/dispatcher.rs
git commit -m "refactor(operation): 全量去 Cursor 前缀 + 预留 Extend*/Cancel（行为不变）"
```

## Task 4: buffer.rs head/anchor 独立原语 + insert/delete 非空重写 + executor 适配（保持 collapsed 行为）

删掉守恒 collapsed 版的 `move_selection_*`/`set_selection`，新增 head/anchor 独立原语 `move_head_*`/`set_head`/`collapse_to_head`；`recompute_selection` 去 `dead_code`；重写 `insert_at_selections`/`delete_at_selections`（非空时删 `[min,max]` range + collapse，空时同现状）。executor 同步适配：`Move*` 改调 `move_head_*` + `collapse_to_head`，`MoveTo` 改调 `set_head` + `collapse_to_head` + `retain_primary`——**保持当前 collapsed 行为不变**（选区恒空，因为 Task 5 才引入 Extend/Cancel）。executor 的 `Extend*`/`Cancel` 仍落 `_ => {}`，不接线。

**Files:**
- Modify: `src/core/buffer.rs:180-265`（删旧原语 + 加新原语 + 重写 insert/delete）
- Modify: `src/core/buffer.rs:182-187`（`recompute_selection` 去 dead_code）
- Modify: `src/core/buffer.rs:304-441`（测试模块：重写旧测试 + 新增原语测试）
- Modify: `src/app/executor.rs:8-32`（适配新原语，保持 collapsed 行为）

- [ ] **Step 1: 写失败测试——`move_head_*` 与 `collapse_to_head` 的 anchor 不变性**

在 `src/core/buffer.rs` 测试模块（`mod tests` 内，`open_existing_sets_none_status` 之后、闭合 `}` 之前）追加：

```rust
    #[test]
    fn move_head_left_keeps_anchor_and_makes_non_empty() {
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        b.insert_char(2, 'c');
        let mut s = single_sel(cur(3));
        let anchor_before = s.primary().anchor;
        b.move_head_left(s.primary_mut(), 2);
        // head 移到 1，anchor 不动 → 非空
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, anchor_before);
        assert!(s.primary().anchor != s.primary().head());
    }

    #[test]
    fn collapse_to_head_makes_anchor_eq_head() {
        let mut s = single_sel(cur(0));
        s.primary_mut().head = cur(3);
        // 人为造出非空（anchor=0, head=3）
        Buffer::collapse_to_head(s.primary_mut());
        assert_eq!(s.primary().anchor, s.primary().head());
        assert_eq!(s.primary().anchor.char_index, 3);
    }

    #[test]
    fn move_head_up_down_keeps_anchor() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello\nab\nworld");
        let mut s = single_sel(cur(4)); // row 0 col 4
        let anchor_before = s.primary().anchor;
        b.move_head_down(s.primary_mut(), 1);
        assert_eq!(s.primary().head().row, 1);
        assert_eq!(s.primary().anchor, anchor_before); // anchor 不动
        assert!(s.primary().anchor != s.primary().head());
    }
```

- [ ] **Step 2: 写失败测试——insert/delete 非空替换**

继续追加：

```rust
    #[test]
    fn insert_at_non_empty_selection_replaces_range() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello");
        // 选区 [1,4) = "ell"，插入 "XY" 替换
        let mut s = {
            let mut sel = Selection::collapsed(cur(1));
            sel.head = cur(4);
            Selections::single(sel)
        };
        b.insert_at_selections(&mut s, "XY");
        assert_eq!(b.slice().to_string(), "hXYo");
        assert_eq!(s.primary().head().char_index, 3); // 插入末尾
        assert_eq!(s.primary().anchor, s.primary().head()); // collapse
    }

    #[test]
    fn delete_at_non_empty_selection_removes_range() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello");
        let mut s = {
            let mut sel = Selection::collapsed(cur(1));
            sel.head = cur(4);
            Selections::single(sel)
        };
        b.delete_at_selections(&mut s, -1); // n 被忽略，删 [1,4)
        assert_eq!(b.slice().to_string(), "ho");
        assert_eq!(s.primary().head().char_index, 1); // head=min 端点
        assert_eq!(s.primary().anchor, s.primary().head()); // collapse
    }

    #[test]
    fn insert_at_collapsed_keeps_point_semantics() {
        // 回归：空 selection 仍是点插入（行为不变）
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        let mut s = single_sel(cur(1));
        b.insert_at_selections(&mut s, "X");
        assert_eq!(b.slice().to_string(), "aXb");
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }
```

- [ ] **Step 3: 运行测试验证失败**

Run: `cargo test core::buffer`
Expected: 编译失败——`move_head_left`/`move_head_down`/`collapse_to_head` 方法不存在（`no method named move_head_left`）。

- [ ] **Step 4: 替换 buffer 原语——删 `move_selection_*`/`set_selection`，加 `move_head_*`/`set_head`/`collapse_to_head`，`recompute_selection` 去 dead_code**

把 `src/core/buffer.rs` 第 180-218 行（从 `// ——编辑原语：selection 层` 注释到 `set_selection` 结束）：

```rust
    // ——编辑原语：selection 层（pub，守恒 collapsed）——

    /// recompute head + anchor 的 row/col（v0.2 anchor==head，幂等）。
    #[allow(dead_code)] // v0.2 预留：真选区编辑时 recompute head/anchor 独立
    pub fn recompute_selection(&self, sel: &mut Selection) {
        self.recompute_cursor(&mut sel.head);
        self.recompute_cursor(&mut sel.anchor);
    }

    /// v0.2：移动 head 并保持 collapsed（anchor=head）。
    pub fn move_selection_by(&self, sel: &mut Selection, chars: isize, lines: isize) {
        self.move_cursor_by(&mut sel.head, chars, lines);
        sel.anchor = sel.head;
    }

    pub fn move_selection_left(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_left(&mut sel.head, n);
        sel.anchor = sel.head;
    }

    pub fn move_selection_right(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_right(&mut sel.head, n);
        sel.anchor = sel.head;
    }

    pub fn move_selection_up(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_up(&mut sel.head, n);
        sel.anchor = sel.head;
    }

    pub fn move_selection_down(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_down(&mut sel.head, n);
        sel.anchor = sel.head;
    }

    pub fn set_selection(&self, sel: &mut Selection, char_idx: usize, line_idx: usize) {
        self.set_cursor(&mut sel.head, char_idx, line_idx);
        sel.anchor = sel.head;
    }
```

改为：

```rust
    // ——编辑原语：selection 层（pub，head/anchor 独立，守恒由调用方决定）——

    /// recompute head + anchor 的 row/col（独立 recompute，v0.3 真选区启用）。
    pub fn recompute_selection(&self, sel: &mut Selection) {
        self.recompute_cursor(&mut sel.head);
        self.recompute_cursor(&mut sel.anchor);
    }

    /// 移动 head，不碰 anchor（extend 语义：selection 变非空）。
    pub fn move_head_by(&self, sel: &mut Selection, chars: isize, lines: isize) {
        self.move_cursor_by(&mut sel.head, chars, lines);
    }

    pub fn move_head_left(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_left(&mut sel.head, n);
    }

    pub fn move_head_right(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_right(&mut sel.head, n);
    }

    pub fn move_head_up(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_up(&mut sel.head, n);
    }

    pub fn move_head_down(&self, sel: &mut Selection, n: usize) {
        self.move_cursor_down(&mut sel.head, n);
    }

    /// 设 head，不碰 anchor。
    pub fn set_head(&self, sel: &mut Selection, char_idx: usize, line_idx: usize) {
        self.set_cursor(&mut sel.head, char_idx, line_idx);
    }

    /// anchor = head（collapsed 守恒，由调用方决定时机）。
    pub fn collapse_to_head(sel: &mut Selection) {
        sel.anchor = sel.head;
    }
```

- [ ] **Step 5: 重写 `insert_at_selections`（非空删 range + 插入 + collapse）**

把 `src/core/buffer.rs` 第 220-235 行 `insert_at_selections`：

```rust
    /// 在每个 selection 的 head 插入文本，head 前移 text_len，anchor=head（守恒 collapsed）。
    pub fn insert_at_selections(&mut self, selections: &mut Selections, text: &str) {
        let text_len = text.chars().count();
        let mut indices: Vec<usize> = selections.all().map(|s| s.head.char_index).collect();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();
        for idx in indices {
            self.rope.insert(idx, text);
        }
        self.modified = true;
        for sel in selections.all_mut() {
            sel.head.char_index += text_len;
            self.recompute_cursor(&mut sel.head);
            sel.anchor = sel.head;
        }
    }
```

改为：

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
        let mut insert_indices: Vec<usize> = selections.all()
            .map(|s| s.anchor.char_index.min(s.head.char_index))
            .collect();
        insert_indices.sort_unstable_by(|a, b| b.cmp(a));
        insert_indices.dedup();
        for idx in insert_indices {
            self.rope.insert(idx, text);
        }
        self.modified = true;
        // 3) 更新每个 selection：head = 插入点 + text_len，collapse（编辑后重置 anchor）
        for sel in selections.all_mut() {
            let insert_at = sel.anchor.char_index.min(sel.head.char_index);
            sel.head.char_index = insert_at + text_len;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }
```

- [ ] **Step 6: 重写 `delete_at_selections`（非空删 range + head=min + collapse）**

把 `src/core/buffer.rs` 第 237-265 行 `delete_at_selections`：

```rust
    /// 在每个 selection 的 head 方向删 n，head 回退，anchor=head（守恒 collapsed）。
    pub fn delete_at_selections(&mut self, selections: &mut Selections, n: isize) {
        let len = self.rope.len_chars();
        let mut ranges: Vec<(usize, usize)> = selections.all().map(|s| {
            let ci = s.head.char_index.min(len);
            if n < 0 {
                let start = ci.saturating_sub((-n) as usize);
                (start, ci)
            } else {
                let end = (ci + n as usize).min(len);
                (ci, end)
            }
        }).collect();
        ranges.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        ranges.dedup();
        for (start, end) in ranges {
            if end > start {
                self.rope.remove(start..end);
            }
        }
        self.modified = true;
        for sel in selections.all_mut() {
            if n < 0 {
                sel.head.char_index = sel.head.char_index.saturating_sub((-n) as usize);
            }
            self.recompute_cursor(&mut sel.head);
            sel.anchor = sel.head;
        }
    }
```

改为：

```rust
    /// 在每个 selection 删除：非空时删 [min,max]，head=min，collapse。
    /// 空时按方向删 n，head 回退（backward）或不动（forward），collapse。
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

- [ ] **Step 7: 改 `executor.rs` 适配新原语（保持 collapsed 行为）**

先在 `src/app/executor.rs` 顶部 import 区（第 1-3 行）把：

```rust
use crate::core::content::ContentHandler;
use crate::core::operation::Operation;
use crate::protocol::selection::Selections;
```

改为（加 `Buffer`，供关联函数 `Buffer::collapse_to_head` 调用）：

```rust
use crate::core::buffer::Buffer;
use crate::core::content::ContentHandler;
use crate::core::operation::Operation;
use crate::protocol::selection::Selections;
```

然后把 `src/app/executor.rs` 第 8-32 行整个 match（注：行号以 Task 3 改后为准，match 内容如下）：

```rust
    match op {
        Operation::MoveBy { chars, lines } => {
            for sel in selections.all_mut() { buf.move_selection_by(sel, chars, lines); }
        }
        Operation::MoveLeftBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_left(sel, n); }
        }
        Operation::MoveRightBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_right(sel, n); }
        }
        Operation::MoveUpBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_up(sel, n); }
        }
        Operation::MoveDownBy(n) => {
            for sel in selections.all_mut() { buf.move_selection_down(sel, n); }
        }
        Operation::MoveTo { char_idx, line_idx } => {
            buf.set_selection(selections.primary_mut(), char_idx, line_idx);
            selections.retain_primary();
        }
        Operation::InsertText(text) => buf.insert_at_selections(selections, &text),
        Operation::Delete(n) => buf.delete_at_selections(selections, n),
        // 全局/多光标变体不进 executor
        _ => {}
    }
```

改为（`Move*` 调 `move_head_*` + `collapse_to_head`，行为同前 collapsed）：

```rust
    match op {
        Operation::MoveBy { chars, lines } => {
            for sel in selections.all_mut() {
                buf.move_head_by(sel, chars, lines);
                Buffer::collapse_to_head(sel);
            }
        }
        Operation::MoveLeftBy(n) => {
            for sel in selections.all_mut() {
                buf.move_head_left(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        Operation::MoveRightBy(n) => {
            for sel in selections.all_mut() {
                buf.move_head_right(sel, n);
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
        Operation::MoveTo { char_idx, line_idx } => {
            buf.set_head(selections.primary_mut(), char_idx, line_idx);
            Buffer::collapse_to_head(selections.primary_mut());
            selections.retain_primary();
        }
        Operation::InsertText(text) => buf.insert_at_selections(selections, &text),
        Operation::Delete(n) => buf.delete_at_selections(selections, n),
        // Extend*/Cancel 未接线（Task 5）；全局/多光标变体不进 executor
        _ => {}
    }
```

- [ ] **Step 8: 重写 buffer 旧测试——`move_selection_*` → `move_head_*` + `collapse_to_head`**

`src/core/buffer.rs` 测试模块中，`move_selection_right_clamps_and_collapsed`（第 371-380 行）和 `move_selection_up_down_clamps_col`（第 382-391 行）引用了已删除的 `move_selection_right`/`move_selection_down`。重写为 `move_head_*` + 显式 `collapse_to_head`：

把：

```rust
    #[test]
    fn move_selection_right_clamps_and_collapsed() {
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        let mut s = single_sel(CursorPos::origin());
        b.move_selection_right(s.primary_mut(), 5);
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_selection_up_down_clamps_col() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello\nab\nworld");
        let mut s = single_sel(CursorPos { char_index: 4, row: 0, col: 0 });
        b.recompute_selection(s.primary_mut());
        b.move_selection_down(s.primary_mut(), 1);
        assert_eq!((s.primary().head().row, s.primary().head().col), (1, 2));
        assert_eq!(s.primary().anchor, s.primary().head());
    }
```

改为：

```rust
    #[test]
    fn move_head_right_clamps_and_collapsed() {
        let mut b = Buffer::new();
        b.insert_char(0, 'a');
        b.insert_char(1, 'b');
        let mut s = single_sel(CursorPos::origin());
        b.move_head_right(s.primary_mut(), 5);
        Buffer::collapse_to_head(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_head_down_clamps_col_then_collapse() {
        let mut b = Buffer::new();
        b.insert_at_selections(&mut single_sel(CursorPos::origin()), "hello\nab\nworld");
        let mut s = single_sel(CursorPos { char_index: 4, row: 0, col: 0 });
        b.recompute_selection(s.primary_mut());
        b.move_head_down(s.primary_mut(), 1);
        assert_eq!((s.primary().head().row, s.primary().head().col), (1, 2));
        Buffer::collapse_to_head(s.primary_mut());
        assert_eq!(s.primary().anchor, s.primary().head());
    }
```

- [ ] **Step 9: 运行 buffer 测试验证通过**

Run: `cargo test core::buffer`
Expected: PASS（旧重写测试 + 新增 `move_head_*`/`collapse_to_head`/insert/delete 非空测试全过）。

- [ ] **Step 10: 运行 executor 测试验证不回归**

Run: `cargo test app::executor`
Expected: PASS（executor 仍保持 collapsed 行为，4 个旧测试不变）。

- [ ] **Step 11: 全量测试 + 无残留旧 API 引用**

Run: `cargo test --all`
Expected: 全部通过。

Run: `Select-String -Path "src\core\buffer.rs","src\app\executor.rs" -Pattern "move_selection_|set_selection\b"`
Expected: 无匹配（旧 API 已全部删除）。

- [ ] **Step 12: Commit**

```powershell
git add src/core/buffer.rs src/app/executor.rs
git commit -m "refactor(buffer): head/anchor 独立原语 + insert/delete 非空替换（保持 collapsed 行为）"
```

## Task 5: executor.rs 真选区分发（收缩/扩展/取消）+ buffer keymap 绑定 shift/escape

executor 引入真选区语义：`MoveLeftBy`/`MoveRightBy` 非空时收缩到 min/max 端点（不额外移），空时 `move_head_*`；`MoveUpBy`/`MoveDownBy` 统一 `move_head_*` + `collapse`（无端点语义）；`Extend*` 只动 head 不 collapse；`Cancel` collapse + retain_primary；`MoveTo` set_head + collapse + retain_primary。buffer keymap 绑定 `Shift(ArrowKey)` → `Extend*`、`Escape` → `Cancel`。去掉 `operation.rs` 中 `Extend*`/`Cancel` 的 `dead_code`。

**Files:**
- Modify: `src/core/operation.rs:10-32`（去 `Extend*`/`Cancel` 的 `#[allow(dead_code)]`）
- Modify: `src/app/executor.rs:8-32`（新分发）
- Modify: `src/app/executor.rs` 测试模块（新增收缩/扩展/取消测试）
- Modify: `src/core/buffer.rs:285-294`（`default_buffer_keymap` 加 shift/escape 绑定）
- Modify: `src/core/buffer.rs` 测试模块（新增 keymap 绑定测试）

- [ ] **Step 1: 写失败测试——executor 收缩/扩展/取消**

在 `src/app/executor.rs` 测试模块（`move_to_retains_primary_clears_secondaries` 之后、闭合 `}` 之前）追加：

```rust
    fn non_empty_sel(anchor_idx: usize, head_idx: usize, buf: &Buffer) -> Selections {
        let mut a = CursorPos::origin();
        a.char_index = anchor_idx;
        buf.recompute_cursor(&mut a);
        let mut h = a;
        h.char_index = head_idx;
        buf.recompute_cursor(&mut h);
        let sel = Selection { anchor: a, head: h };
        Selections::single(sel)
    }

    #[test]
    fn move_left_on_non_empty_shrinks_to_min() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        buf.insert_char(2, 'c');
        // 选区 [1,3)，head=3 forward
        let mut s = non_empty_sel(1, 3, &buf);
        execute(Operation::MoveLeftBy(1), &mut buf, &mut s);
        // 收缩到 min=1，不额外左移
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head()); // collapse
    }

    #[test]
    fn move_left_on_backward_selection_shrinks_to_min() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        buf.insert_char(2, 'c');
        // backward 选区：anchor=3, head=1（head<anchor）
        let mut s = non_empty_sel(3, 1, &buf);
        execute(Operation::MoveLeftBy(1), &mut buf, &mut s);
        // min=1，head 已在 1，不动；落点与朝向无关
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_right_on_non_empty_shrinks_to_max() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        buf.insert_char(2, 'c');
        let mut s = non_empty_sel(1, 3, &buf);
        execute(Operation::MoveRightBy(1), &mut buf, &mut s);
        // 收缩到 max=3
        assert_eq!(s.primary().head().char_index, 3);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_left_on_collapsed_moves_head() {
        // 回归：空 selection 左移 n（同现状）
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        let mut s = non_empty_sel(2, 2, &buf);
        execute(Operation::MoveLeftBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn extend_left_moves_head_keeps_anchor() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        buf.insert_char(2, 'c');
        let mut s = non_empty_sel(2, 2, &buf); // collapsed at 2
        execute(Operation::ExtendLeftBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor.char_index, 2); // anchor 钉住
        assert!(s.primary().anchor != s.primary().head()); // 非空，未 collapse
    }

    #[test]
    fn cancel_collapses_and_retains_primary() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut s = non_empty_sel(0, 1, &buf);
        execute(Operation::Cancel, &mut buf, &mut s);
        assert_eq!(s.primary().anchor, s.primary().head()); // collapse to head
        assert_eq!(s.primary().head().char_index, 1); // head 不动
        assert_eq!(s.all().count(), 1); // retain_primary
    }

    #[test]
    fn insert_on_non_empty_replaces_range() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut Selections::single(Selection::collapsed(CursorPos::origin())), "hello");
        let mut s = non_empty_sel(1, 4, &buf); // [1,4) = "ell"
        execute(Operation::InsertText("XY".to_string()), &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "hXYo");
        assert_eq!(s.primary().head().char_index, 3);
        assert_eq!(s.primary().anchor, s.primary().head());
    }
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test app::executor`
Expected: `move_left_on_non_empty_shrinks_to_min` 等 FAIL——当前 `MoveLeftBy` 对非空也调 `move_head_left`（head 左移到 0 而非收缩到 1）；`extend_left_moves_head_keeps_anchor` FAIL（`Extend*` 落 `_ => {}` 不动）；`cancel_collapses_and_retains_primary` FAIL。

- [ ] **Step 3: 改 executor 新分发**

把 `src/app/executor.rs` 第 8-32 行整个 match：

```rust
    match op {
        Operation::MoveBy { chars, lines } => {
            for sel in selections.all_mut() {
                buf.move_head_by(sel, chars, lines);
                Buffer::collapse_to_head(sel);
            }
        }
        Operation::MoveLeftBy(n) => {
            for sel in selections.all_mut() {
                buf.move_head_left(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        Operation::MoveRightBy(n) => {
            for sel in selections.all_mut() {
                buf.move_head_right(sel, n);
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
        Operation::MoveTo { char_idx, line_idx } => {
            buf.set_head(selections.primary_mut(), char_idx, line_idx);
            Buffer::collapse_to_head(selections.primary_mut());
            selections.retain_primary();
        }
        Operation::InsertText(text) => buf.insert_at_selections(selections, &text),
        Operation::Delete(n) => buf.delete_at_selections(selections, n),
        // Extend*/Cancel 未接线（Task 5）；全局/多光标变体不进 executor
        _ => {}
    }
```

改为：

```rust
    match op {
        // Left/Right 有端点语义：非空收缩到 min/max（不额外移），空则移动 head
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
        // Up/Down 无端点语义：统一 move_head + collapse（取消并继续上下移）
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
        Operation::MoveBy { chars, lines } => {
            for sel in selections.all_mut() {
                buf.move_head_by(sel, chars, lines);
                Buffer::collapse_to_head(sel);
            }
        }
        // Extend：只动 head 不碰 anchor，不 collapse（选区变非空）
        Operation::ExtendLeftBy(n)  => { for sel in selections.all_mut() { buf.move_head_left(sel, n); } }
        Operation::ExtendRightBy(n) => { for sel in selections.all_mut() { buf.move_head_right(sel, n); } }
        Operation::ExtendUpBy(n)    => { for sel in selections.all_mut() { buf.move_head_up(sel, n); } }
        Operation::ExtendDownBy(n)  => { for sel in selections.all_mut() { buf.move_head_down(sel, n); } }
        // Escape：collapse to head + 仅留 primary
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
        Operation::Delete(n) => buf.delete_at_selections(selections, n),
        // 全局/多光标变体不进 executor
        _ => {}
    }
```

- [ ] **Step 4: 去 `operation.rs` 中 `Extend*`/`Cancel` 的 `dead_code`**

把 `src/core/operation.rs` 中（Task 3 新增的）这几行的 `#[allow(dead_code)]` 注释删掉。即将：

```rust
    #[allow(dead_code)] // Task 5 启用：shift+方向键扩展选区
    ExtendLeftBy(usize),
    #[allow(dead_code)] // Task 5 启用
    ExtendRightBy(usize),
    #[allow(dead_code)] // Task 5 启用
    ExtendUpBy(usize),
    #[allow(dead_code)] // Task 5 启用
    ExtendDownBy(usize),
    #[allow(dead_code)] // Task 5 启用：Escape 取消选区
    Cancel,
```

改为（去掉两行标注）：

```rust
    ExtendLeftBy(usize),
    ExtendRightBy(usize),
    ExtendUpBy(usize),
    ExtendDownBy(usize),
    Cancel,
```

- [ ] **Step 5: 给 buffer keymap 加 shift/escape 绑定**

把 `src/core/buffer.rs` 的 `default_buffer_keymap`（Task 3 改后版本）：

```rust
fn default_buffer_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::Enter, Operation::InsertText("\n".to_string()));
    km.bind(KeyEvent::Backspace, Operation::Delete(-1));
    km.bind(KeyEvent::Arrow(ArrowKey::Left), Operation::MoveLeftBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Right), Operation::MoveRightBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Up), Operation::MoveUpBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Down), Operation::MoveDownBy(1));
    km
}
```

改为（新增 4 个 Shift 绑定 + Escape）：

```rust
fn default_buffer_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::Enter, Operation::InsertText("\n".to_string()));
    km.bind(KeyEvent::Backspace, Operation::Delete(-1));
    km.bind(KeyEvent::Arrow(ArrowKey::Left), Operation::MoveLeftBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Right), Operation::MoveRightBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Up), Operation::MoveUpBy(1));
    km.bind(KeyEvent::Arrow(ArrowKey::Down), Operation::MoveDownBy(1));
    km.bind(KeyEvent::Shift(ArrowKey::Left), Operation::ExtendLeftBy(1));
    km.bind(KeyEvent::Shift(ArrowKey::Right), Operation::ExtendRightBy(1));
    km.bind(KeyEvent::Shift(ArrowKey::Up), Operation::ExtendUpBy(1));
    km.bind(KeyEvent::Shift(ArrowKey::Down), Operation::ExtendDownBy(1));
    km.bind(KeyEvent::Escape, Operation::Cancel);
    km
}
```

- [ ] **Step 6: 写 keymap 绑定测试**

先在 `src/core/buffer.rs` 顶部 `use` 区（第 6-11 行）把：

```rust
use crate::core::keymap::Keymap;
```

改为：

```rust
use crate::core::keymap::{KeyBinding, Keymap};
```

然后在 `src/core/buffer.rs` 测试模块（`default_binding_non_char_is_none` 之后）追加。注意 `Keymap::lookup` 返回 `Option<&KeyBinding>`，断言用 `Some(&KeyBinding::Operation(...))`：

```rust
    #[test]
    fn buffer_keymap_shift_arrow_binds_extend() {
        let b = Buffer::new();
        let km = b.keymap();
        assert_eq!(km.lookup(KeyEvent::Shift(ArrowKey::Left)), Some(&KeyBinding::Operation(Operation::ExtendLeftBy(1))));
        assert_eq!(km.lookup(KeyEvent::Shift(ArrowKey::Right)), Some(&KeyBinding::Operation(Operation::ExtendRightBy(1))));
        assert_eq!(km.lookup(KeyEvent::Shift(ArrowKey::Up)), Some(&KeyBinding::Operation(Operation::ExtendUpBy(1))));
        assert_eq!(km.lookup(KeyEvent::Shift(ArrowKey::Down)), Some(&KeyBinding::Operation(Operation::ExtendDownBy(1))));
    }

    #[test]
    fn buffer_keymap_escape_binds_cancel() {
        let b = Buffer::new();
        let km = b.keymap();
        assert_eq!(km.lookup(KeyEvent::Escape), Some(&KeyBinding::Operation(Operation::Cancel)));
    }
```

- [ ] **Step 7: 运行 executor + buffer 测试验证通过**

Run: `cargo test app::executor`
Expected: PASS（4 旧 + 8 新 = 12 个测试全过）。

Run: `cargo test core::buffer`
Expected: PASS（含新 keymap 绑定测试 + `KeyBinding` 导入）。

- [ ] **Step 8: 全量测试 + 无 dead_code 警告**

Run: `cargo test --all`
Expected: 全部通过。

Run: `cargo check --all 2>&1 | Select-String "Extend|Cancel|dead_code"`
Expected: 无 `Extend*`/`Cancel` 相关 `dead_code` 警告（已接线）。

- [ ] **Step 9: Commit**

```powershell
git add src/core/operation.rs src/core/buffer.rs src/app/executor.rs
git commit -m "feat(executor): 真选区分发（收缩/扩展/取消）+ keymap 绑定 shift+方向键/Escape"
```

## Task 6: output.rs `Canvas::set_reverse` + `Output<W>` 实现

`Canvas` trait 加 `set_reverse(on: bool)`；`Output<W>` 用 crossterm `SetAttribute(Attribute::Reverse)`/`NoReverse` 实现。反白 escape：开 = `\x1b[7m`，关 = `\x1b[27m`。SceneRenderer（Task 7）将调用它画选区高亮。

**Files:**
- Modify: `src/terminal/output.rs:1-24`（trait + impl + import）
- Test: `src/terminal/output.rs`（`#[cfg(test)] mod tests`）

- [ ] **Step 1: 写失败测试**

在 `src/terminal/output.rs` 测试模块（`canvas_dispatches_to_output` 之后、闭合 `}` 之前）追加：

```rust
    #[test]
    fn set_reverse_on_emits_sgr_7() {
        let mut out = Output::new(Vec::new());
        out.set_reverse(true).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[7m"), "got: {s}");
    }

    #[test]
    fn set_reverse_off_emits_sgr_27() {
        let mut out = Output::new(Vec::new());
        out.set_reverse(false).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[27m"), "got: {s}");
    }

    #[test]
    fn canvas_dispatches_set_reverse() {
        let mut out = Output::new(Vec::new());
        let c: &mut dyn Canvas = &mut out;
        c.set_reverse(true).unwrap();
        c.set_reverse(false).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[7m"), "on: {s}");
        assert!(s.contains("\x1b[27m"), "off: {s}");
    }
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test terminal::output`
Expected: 编译失败——`set_reverse` 方法不存在（`no method named set_reverse`）。

- [ ] **Step 3: 给 `Canvas` trait 加 `set_reverse`**

把 `src/terminal/output.rs` 第 8-15 行 trait：

```rust
pub trait Canvas {
    fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()>;
    fn clear_line(&mut self) -> io::Result<()>;
    fn write_str(&mut self, s: &str) -> io::Result<()>;
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}
```

改为：

```rust
pub trait Canvas {
    fn move_cursor(&mut self, row: usize, col: usize) -> io::Result<()>;
    fn clear_line(&mut self) -> io::Result<()>;
    fn write_str(&mut self, s: &str) -> io::Result<()>;
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn set_reverse(&mut self, on: bool) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}
```

- [ ] **Step 4: `Output<W>` 加固有方法 + trait 实现**

在 `src/terminal/output.rs` 第 1 行 import：

```rust
use crossterm::{cursor, queue, terminal};
```

改为：

```rust
use crossterm::style::{Attribute, SetAttribute};
use crossterm::{cursor, queue, terminal};
```

在 `Output<W>` impl 块内（`show_cursor` 方法之后、`move_cursor` 之前，约第 45-46 行之间）插入固有方法：

```rust
    pub fn set_reverse(&mut self, on: bool) -> io::Result<()> {
        let attr = if on { Attribute::Reverse } else { Attribute::NoReverse };
        queue!(self.out, SetAttribute(attr))
    }
```

在 `impl<W: Write> Canvas for Output<W>` 块内（`show_cursor` 之后、`flush` 之前）加 trait 转发：

```rust
    fn set_reverse(&mut self, on: bool) -> io::Result<()> { Output::set_reverse(self, on) }
```

- [ ] **Step 5: 运行测试验证通过**

Run: `cargo test terminal::output`
Expected: PASS（原 5 个 + 新增 3 个 = 8 个测试全过）。

- [ ] **Step 6: 全量 check 确认 trait 实现完整**

Run: `cargo check --all`
Expected: 编译通过（`Canvas` 唯一 implementor `Output<W>` 已实现 `set_reverse`，无 `not all trait items implemented` 错误）。

- [ ] **Step 7: Commit**

```powershell
git add src/terminal/output.rs
git commit -m "feat(output): Canvas 加 set_reverse + Output 实现（反白 SGR 7/27）"
```

## Task 7: scene_renderer.rs 选区高亮分段反白 + viewport 裁剪

`paint_item` 的 editor 分支：pull `query.selections(sid)`，若 `primary()` 非空，按 `[min(anchor,head), max(anchor,head)]` 的 `(row,col)` 端点对每行分段反白画（首行 `start.col`→行尾、中间整行、末行 `0`→`end.col`、同行 `start.col`→`end.col`）。新增自由函数 `paint_line_with_highlight` 按 char 边界切段，反白段 `set_reverse(true)`+write+`set_reverse(false)`。viewport 裁剪由 `query.lines` 仅拉可见行天然满足（选区行不在可见范围不画）。光标定位不变。

> **v0.3 不实现水平滚动**：`Viewport.left_col` 恒 0（`ensure_cursor_visible` 只调 `top_row`），高亮列直接用 buffer `col`，与现有文本写法（整行从 `rect.x` 起）一致。

**Files:**
- Modify: `src/tui/scene_renderer.rs:75-113`（`paint_item` editor 分支）
- Add: `src/tui/scene_renderer.rs` 自由函数 `paint_line_with_highlight`
- Test: `src/tui/scene_renderer.rs:145-208`（`#[cfg(test)] mod tests`）

- [ ] **Step 1: 写失败测试——非空选区反白 + collapsed 不反白**

在 `src/tui/scene_renderer.rs` 测试模块（`viewport_follows_cursor_below` 之后、闭合 `}` 之前）追加：

```rust
    #[test]
    fn renders_non_empty_selection_with_reverse() {
        let (scene, ed) = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string()],
            selections: Selections::single(Selection {
                anchor: CursorPos { char_index: 1, row: 0, col: 1 },
                head: CursorPos { char_index: 4, row: 0, col: 4 },
            }),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[7m"), "should contain reverse-on: {s}");
        assert!(s.contains("\x1b[27m"), "should contain reverse-off: {s}");
    }

    #[test]
    fn renders_collapsed_selection_without_reverse() {
        let (scene, ed) = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string()],
            selections: Selections::single(Selection::collapsed(CursorPos::origin())),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(!s.contains("\x1b[7m"), "collapsed should not reverse: {s}");
    }
```

- [ ] **Step 2: 写失败测试——跨行选区分段**

继续追加：

```rust
    #[test]
    fn renders_multiline_selection_reverse_spans_lines() {
        let (scene, ed) = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        // "hello\nworld"：row0 col2 = idx2；row1 col2 = idx8
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string(), "world".to_string()],
            selections: Selections::single(Selection {
                anchor: CursorPos { char_index: 2, row: 0, col: 2 },
                head: CursorPos { char_index: 8, row: 1, col: 2 },
            }),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas).unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        // 首行 [2,行尾) + 末行 [0,2) 两段反白 → 至少 2 个 reverse-on
        let count = s.matches("\x1b[7m").count();
        assert!(count >= 2, "multiline should have >=2 reverse segments, got {count}: {s}");
    }
```

- [ ] **Step 3: 写失败测试——viewport 裁剪（不可见行不画）**

继续追加：

```rust
    #[test]
    fn selection_clipped_to_viewport_does_not_draw_invisible_rows() {
        let (scene, ed) = build_editor_scene(40, 5, ContentId(0), ContentId(1));
        let lines: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        // 第一次：cursor row 25 → viewport top_row=21
        let q1 = StubQuery {
            editor_cid: ContentId(0),
            lines: lines.clone(),
            selections: Selections::single(Selection::collapsed(CursorPos { char_index: 0, row: 25, col: 0 })),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &q1, ed, &mut out as &mut dyn Canvas).unwrap();
        // 第二次：selection 跨 row 0-25，head 在 row 25 维持 viewport（top_row=21）
        // line0..line29 每行 5 chars + \n = 6 chars；row25 col0 → char_index=150
        let q2 = StubQuery {
            editor_cid: ContentId(0),
            lines,
            selections: Selections::single(Selection {
                anchor: CursorPos { char_index: 1, row: 0, col: 1 },
                head: CursorPos { char_index: 150, row: 25, col: 0 },
            }),
        };
        let mut out2 = Output::new(Vec::new());
        r.render(&scene, &q2, ed, &mut out2 as &mut dyn Canvas).unwrap();
        let s = String::from_utf8(out2.into_inner()).unwrap();
        // row 0 不可见（top_row=21）→ 不应出现 line0
        assert!(!s.contains("line0"), "invisible row should not be drawn: {s}");
        // 可见中间行（21-24）在选区内 → 应反白
        assert!(s.contains("\x1b[7m"), "visible middle rows should reverse: {s}");
    }
```

- [ ] **Step 4: 运行测试验证失败**

Run: `cargo test tui::scene_renderer`
Expected: `renders_non_empty_selection_with_reverse` FAIL（当前 `paint_item` 不画反白，输出无 `\x1b[7m`）；`renders_collapsed_selection_without_reverse` PASS（现状本就不反白）。

- [ ] **Step 5: 改 `paint_item` editor 分支 pull 选区 + 分段画**

把 `src/tui/scene_renderer.rs` 第 87-103 行 editor 分支：

```rust
    let line_count = query.line_count(item.content_id);
    if line_count > 0 {
        // editor：拉可见行
        let height = item.rect.height as usize;
        let start = vp.top_row;
        let lines = query.lines(item.content_id, RowRange { start, end: start + height });
        for (row, line) in lines.iter().enumerate() {
            let screen_row = (item.rect.y + row as i32) as usize;
            canvas.move_cursor(screen_row, item.rect.x as usize)?;
            canvas.clear_line()?;
            canvas.write_str(line)?;
        }
        for row in lines.len()..height {
            let screen_row = (item.rect.y + row as i32) as usize;
            canvas.move_cursor(screen_row, item.rect.x as usize)?;
            canvas.clear_line()?;
        }
    } else {
```

改为：

```rust
    let line_count = query.line_count(item.content_id);
    if line_count > 0 {
        // editor：拉可见行
        let height = item.rect.height as usize;
        let start = vp.top_row;
        let lines = query.lines(item.content_id, RowRange { start, end: start + height });
        // 选区高亮：primary 非空时算 [start,end] 端点（按 char_index 排序）
        let prim = query.selections(sid).primary();
        let non_empty = prim.anchor != prim.head;
        let (sel_start, sel_end) = if non_empty {
            if prim.anchor.char_index <= prim.head.char_index {
                (prim.anchor, prim.head)
            } else {
                (prim.head, prim.anchor)
            }
        } else {
            (prim.anchor, prim.head) // collapsed：不会触发高亮
        };
        for (row, line) in lines.iter().enumerate() {
            let buf_row = start + row;
            let screen_row = (item.rect.y + row as i32) as usize;
            canvas.move_cursor(screen_row, item.rect.x as usize)?;
            canvas.clear_line()?;
            let hi = if non_empty && buf_row >= sel_start.row && buf_row <= sel_end.row {
                let hs = if buf_row == sel_start.row { sel_start.col } else { 0 };
                let he = if buf_row == sel_end.row { sel_end.col } else { usize::MAX };
                Some((hs, he))
            } else {
                None
            };
            paint_line_with_highlight(canvas, line, hi)?;
        }
        for row in lines.len()..height {
            let screen_row = (item.rect.y + row as i32) as usize;
            canvas.move_cursor(screen_row, item.rect.x as usize)?;
            canvas.clear_line()?;
        }
    } else {
```

- [ ] **Step 6: 加自由函数 `paint_line_with_highlight`**

在 `src/tui/scene_renderer.rs` 中 `paint_item` 函数之后（`find_space_by_content` 之前）插入：

```rust
/// 画一行文本，可选反白高亮区间 [hi_start_col, hi_end_col)（按 char 列，end 用 usize::MAX 表示到行尾）。
/// hi=None 时整行正常画。行尾换行符（若有）始终正常画，不参与反白。
fn paint_line_with_highlight(
    canvas: &mut dyn Canvas,
    line: &str,
    hi: Option<(usize, usize)>,
) -> io::Result<()> {
    let (content, tail) = match line.strip_suffix('\n') {
        Some(c) => (c, "\n"),
        None => (line, ""),
    };
    // char 边界（byte offset, char），用于按列切 byte 范围
    let bounds: Vec<(usize, char)> = content.char_indices().collect();
    let content_len = bounds.len();
    let write_seg = |canvas: &mut dyn Canvas, from: usize, to: usize, reverse: bool| -> io::Result<()> {
        if to <= from { return Ok(()); }
        let from = from.min(content_len);
        let to = to.min(content_len);
        if to <= from { return Ok(()); }
        let start_byte = bounds[from].0;
        let end_byte = if to >= content_len { content.len() } else { bounds[to].0 };
        if reverse { canvas.set_reverse(true)?; }
        canvas.write_str(&content[start_byte..end_byte])?;
        if reverse { canvas.set_reverse(false)?; }
        Ok(())
    };
    match hi {
        None => { canvas.write_str(content)?; }
        Some((hs, he)) => {
            write_seg(canvas, 0, hs, false)?;
            write_seg(canvas, hs, he, true)?;
            write_seg(canvas, he, content_len, false)?;
        }
    }
    canvas.write_str(tail)?;
    Ok(())
}
```

- [ ] **Step 7: 运行 scene_renderer 测试验证通过**

Run: `cargo test tui::scene_renderer`
Expected: PASS（原 2 个 + 新增 4 个 = 6 个测试全过）。

- [ ] **Step 8: 全量测试**

Run: `cargo test --all`
Expected: 全部通过。

- [ ] **Step 9: Commit**

```powershell
git add src/tui/scene_renderer.rs
git commit -m "feat(scene_renderer): 非空选区跨行分段反白高亮 + viewport 裁剪"
```

## Task 8: 集成测试（headless 驱动 shift+方向键建选区→替换→字节断言）

用 `App::run` + `HeadlessFrontend` 跑完整事件链：shift+方向键建选区 → 输入替换选区（字节断言 buffer 文本）；Escape 取消选区后方向键正常移动；建选区后渲染帧含反白 escape。复用 `app/mod.rs` 测试模块已有的 `make_app` helper。

**Files:**
- Modify: `src/app/mod.rs:288-435`（`#[cfg(test)] mod tests` 内追加 3 个 `#[tokio::test]`）

- [ ] **Step 1: 写集成测试——shift 建选区 + 输入替换**

在 `src/app/mod.rs` 测试模块（`prefix_key_sequence_saves` 之后、闭合 `}` 之前）追加：

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn shift_arrow_builds_selection_then_input_replaces() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::Char(b'a')),
                FrontendEvent::Key(KeyEvent::Char(b'b')),
                FrontendEvent::Key(KeyEvent::Char(b'c')),
                FrontendEvent::Key(KeyEvent::Shift(ArrowKey::Left)), // 选区 [2,3)
                FrontendEvent::Key(KeyEvent::Char(b'X')),            // 替换 [2,3) 为 X
                FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
            ],
            None,
        );
        app.run().await.unwrap();
        let buf = app.contents.get_mut(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
        assert_eq!(buf.slice().to_string(), "abX");
        let head = app.views.get(&app.focused).unwrap().selections().primary().head();
        assert_eq!(head.char_index, 3);
        assert_eq!(app.views.get(&app.focused).unwrap().selections().primary().anchor, head); // collapse
    }
```

- [ ] **Step 2: 写集成测试——Escape 取消后方向键正常移动**

继续追加：

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn escape_cancels_selection_then_arrow_moves() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::Char(b'a')),
                FrontendEvent::Key(KeyEvent::Char(b'b')),
                FrontendEvent::Key(KeyEvent::Char(b'c')),
                FrontendEvent::Key(KeyEvent::Shift(ArrowKey::Left)), // 选区 [2,3)
                FrontendEvent::Key(KeyEvent::Escape),                 // 取消 → head=2 collapse
                FrontendEvent::Key(KeyEvent::Arrow(ArrowKey::Left)),  // collapsed 左移 → head=1
                FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
            ],
            None,
        );
        app.run().await.unwrap();
        let buf = app.contents.get_mut(&editor_cid()).and_then(|c| c.buffer_mut()).unwrap();
        assert_eq!(buf.slice().to_string(), "abc"); // Escape/方向键不改文本
        let head = app.views.get(&app.focused).unwrap().selections().primary().head();
        assert_eq!(head.col, 1);
        assert_eq!(app.views.get(&app.focused).unwrap().selections().primary().anchor, head); // collapsed
    }
```

- [ ] **Step 3: 写集成测试——建选区后渲染帧含反白**

继续追加：

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn selection_renders_reverse_in_frame() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::Char(b'a')),
                FrontendEvent::Key(KeyEvent::Char(b'b')),
                FrontendEvent::Key(KeyEvent::Char(b'c')),
                FrontendEvent::Key(KeyEvent::Shift(ArrowKey::Left)), // 选区 [2,3)
                FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),      // quit 前 render 已发生
            ],
            None,
        );
        app.run().await.unwrap();
        if let FrontendImpl::Headless(h) = &app.frontend {
            // 最后一帧是 Shift(Left) 事件后 render 的（Ctrl+Q break 前不再 render）
            let frame = h.frames.last().expect("frame captured");
            let s = String::from_utf8(frame.clone()).unwrap();
            assert!(s.contains("\x1b[7m"), "selection should render reverse: {s}");
        } else {
            panic!("expected headless frontend");
        }
    }
```

- [ ] **Step 4: 运行集成测试验证通过**

Run: `cargo test app::tests`
Expected: PASS（原 7 个 + 新增 3 个 = 10 个测试全过）。

> 若 `shift_arrow_builds_selection_then_input_replaces` 失败（buffer 非 "abX"），先检查：Shift(ArrowKey::Left) 是否被 `translate_key` 正确映射（Task 2）、buffer keymap 是否绑定 ExtendLeftBy（Task 5 Step 5）、executor Extend 分支是否只动 head 不 collapse（Task 5 Step 3）。

- [ ] **Step 5: 全量测试**

Run: `cargo test --all`
Expected: 全部通过（v0.3 真选区完整链路验证）。

- [ ] **Step 6: Commit**

```powershell
git add src/app/mod.rs
git commit -m "test(app): 集成 shift 建选区→替换 / Escape 取消 / 反白渲染字节断言"
```

---

## 完成后

全部 8 个任务完成后，v0.3 最小真选区已落地。建议：

- 跑 `cargo test --all` 全量绿；
- 手动在 Windows Terminal / PowerShell 验证 shift+方向键建选区、输入替换、Escape 取消（spec §10 风险4：shift+方向键跨终端解析差异，需手动确认）；
- 用 `superpowers:finishing-a-development-branch` 收尾分支（merge 回 main 或建 PR）。
