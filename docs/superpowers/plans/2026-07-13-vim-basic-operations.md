# Vim 基础操作扩展 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 25 个 vim 基础操作（Insert 的 Ctrl+U/K/J/M，Normal 的 w/b/e/0/^/$/G/{/}/x/X/J/D/~ 和模式切换 o/O/I/A/s/C/S）。

**Architecture:** 新增 16 个 `EditCommand` 变体 + 7 个 mode action。词移动扩展已有 3 类词模型；行移动用 ropey 的 `char_to_line`/`line_to_char`；编辑操作在 Buffer 层新增原语方法。mode action 通过 `Mode::execute` 返回 `EditCommand` 实现模式切换+编辑的复合操作。

**Tech Stack:** Rust 2024, ropey, crossterm, tokio

## Global Constraints

- Rust 2024, MSRV 1.85
- `core` 层不依赖 `crossterm`/`taffy`/`tokio`/前端渲染概念
- selection 模型使用 anchor/head，collapsed = anchor==head
- 几何使用整数；`f32` 只在 Taffy adapter 边界
- 不修改 Dispatcher prefix 状态机、App 分发、协议按键模型
- `x`/`X` 复用 `Delete(1)`/`Delete(-1)`，`Ctrl+J`/`Ctrl+M` 复用 `InsertText("\n")`，`s` 的 mode action 返回 `Delete(1)`——不新增变体
- 移动命令对非空 selection 遵循 `MoveLeftBy` 收缩语义：非空缩到 min/max，空时移动，始终 collapse

---

## File Structure

| 文件 | 职责 |
| --- | --- |
| `src/core/command.rs` | `EditCommand` 枚举——新增 16 个变体 |
| `src/core/buffer.rs` | Buffer 原语——新增辅助函数 + selection 层方法 |
| `src/core/edit.rs` | `apply_edit`——新增 16 个 match 分支 |
| `src/core/mode.rs` | 键映射 + mode action——新增 25 个绑定 + 7 个 action |

---

### Task 1: 新增 EditCommand 变体

**Files:**
- Modify: `src/core/command.rs:26-52`

**Interfaces:**
- Produces: `EditCommand` 新增 16 个变体供后续 Task 的 `apply_edit` 和 keymap 使用

- [ ] **Step 1: 在 `EditCommand` 枚举末尾（`CollapseSelections` 后）新增 16 个变体**

在 `src/core/command.rs` 的 `EditCommand` 枚举中，在 `CollapseSelections` 之后添加：

```rust
    CollapseSelections,
    DeleteToLineStart,
    DeleteToLineEnd,
    MoveWordForward,
    MoveWordBackward,
    MoveWordEnd,
    MoveToLineStart,
    MoveToFirstNonBlank,
    MoveToLineEnd,
    MoveToLastLine,
    MoveToPrevParagraph,
    MoveToNextParagraph,
    JoinLines,
    ToggleCase,
    InsertNewLineBelow,
    InsertNewLineAbove,
    MoveAfterLineEnd,
    DeleteLineContent,
```

- [ ] **Step 2: 运行 cargo check 确认编译（变体未使用会有 dead_code 警告，后续 Task 消除）**

Run: `cargo check 2>&1 | Select-String "error"`
Expected: 无 error 输出（dead_code 警告正常）

- [ ] **Step 3: Commit**

```bash
git add src/core/command.rs
git commit -m "feat: add 16 new EditCommand variants for vim basic operations"
```

---

### Task 2: 词移动辅助函数 + Buffer 原语

**Files:**
- Modify: `src/core/buffer.rs` (辅助函数区 ~line 413-441, selection 层方法区 ~line 230-258)

**Interfaces:**
- Consumes: `is_word_char`（已有，line 439）
- Produces: `forward_word_start`, `forward_word_end` 模块函数；`move_head_word_forward`, `move_head_word_backward`, `move_head_word_end` Buffer 方法

- [ ] **Step 1: 写失败测试——`forward_word_start` 和 `forward_word_end`**

在 `src/core/buffer.rs` 的 `#[cfg(test)] mod tests` 中添加（在 `delete_word_backward` 测试块之后）：

```rust
    #[test]
    fn forward_word_start_skips_word_then_whitespace() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_start(rope, 0), 4); // f -> b
        assert_eq!(forward_word_start(rope, 4), 7); // b -> end
    }

    #[test]
    fn forward_word_start_treats_punctuation_as_unit() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo.bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_start(rope, 0), 3); // f -> .
        assert_eq!(forward_word_start(rope, 3), 4); // . -> b
        assert_eq!(forward_word_start(rope, 4), 7); // b -> end
    }

    #[test]
    fn forward_word_end_lands_on_last_char_of_word() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo.bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_end(rope, 0), 2); // f -> o (foo end)
        assert_eq!(forward_word_end(rope, 2), 3); // o -> . (punct end)
        assert_eq!(forward_word_end(rope, 3), 6); // . -> r (bar end)
    }

    #[test]
    fn forward_word_end_skips_whitespace_to_next_word() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo  bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_end(rope, 0), 2); // f -> o
        assert_eq!(forward_word_end(rope, 2), 7); // o -> r (skips spaces)
    }

    #[test]
    fn forward_word_start_at_end_stays_at_end() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_start(rope, 3), 3);
    }

    #[test]
    fn forward_word_end_at_end_stays_at_end() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(forward_word_end(rope, 3), 3);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib forward_word 2>&1 | Select-String "error|FAILED|cannot find"`
Expected: 编译错误 `cannot find function forward_word_start` / `forward_word_end`

- [ ] **Step 3: 实现 `forward_word_start` 和 `forward_word_end`**

在 `src/core/buffer.rs` 中 `backward_word_start` 函数之后（`is_word_char` 之前）添加：

```rust
fn forward_word_start(rope: &Rope, char_index: usize) -> usize {
    let len = rope.len_chars();
    let mut pos = char_index.min(len);
    if pos >= len {
        return len;
    }
    // Skip current word/punct unit (same class as char at pos)
    let start_class = char_class(rope.char(pos));
    while pos < len && char_class(rope.char(pos)) == start_class {
        pos += 1;
    }
    // Skip whitespace
    while pos < len && rope.char(pos).is_whitespace() {
        pos += 1;
    }
    pos
}

fn forward_word_end(rope: &Rope, char_index: usize) -> usize {
    let len = rope.len_chars();
    let mut pos = char_index.min(len);
    if pos >= len {
        return len;
    }
    // If on whitespace or at end of current unit, skip whitespace first
    if rope.char(pos).is_whitespace() {
        while pos < len && rope.char(pos).is_whitespace() {
            pos += 1;
        }
        if pos >= len {
            return len;
        }
    } else {
        // If not at end of current unit, the loop below advances to end.
        // If already at end of current unit, skip to next.
        let start_class = char_class(rope.char(pos));
        if pos + 1 < len && char_class(rope.char(pos + 1)) != start_class {
            // Already at end of unit; skip whitespace to next word
            while pos < len && rope.char(pos).is_whitespace() {
                pos += 1;
            }
            if pos >= len {
                return len;
            }
        }
    }
    // Advance to end of current word/punct unit
    let end_class = char_class(rope.char(pos));
    while pos + 1 < len && char_class(rope.char(pos + 1)) == end_class {
        pos += 1;
    }
    pos
}

fn char_class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if is_word_char(ch) {
        1
    } else {
        2
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib forward_word 2>&1 | Select-String "test result|FAILED|error"`
Expected: 6 tests passed, 0 failed

- [ ] **Step 5: 写失败测试——`move_head_word_forward`/`backward`/`end` Buffer 方法**

在 `src/core/buffer.rs` 测试模块中添加：

```rust
    #[test]
    fn move_head_word_forward_advances_to_next_word() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_word_forward(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn move_head_word_backward_advances_to_prev_word() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 7);
        buffer.move_head_word_backward(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn move_head_word_end_advances_to_word_end() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo.bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_word_end(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 2);
    }
```

- [ ] **Step 6: 运行测试确认失败**

Run: `cargo test --lib move_head_word 2>&1 | Select-String "error|cannot find"`
Expected: 编译错误 `no method named move_head_word_forward`

- [ ] **Step 7: 实现 `move_head_word_forward`/`backward`/`end` 方法**

在 `src/core/buffer.rs` 的 selection 层方法区（`move_head_down` 之后、`set_head` 之前，约 line 248）添加：

```rust
    pub fn move_head_word_forward(&self, sel: &mut Selection) {
        let target = forward_word_start(&self.rope, sel.head.char_index);
        sel.head.char_index = target;
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_word_backward(&self, sel: &mut Selection) {
        let target = backward_word_start(&self.rope, sel.head.char_index);
        sel.head.char_index = target;
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_word_end(&self, sel: &mut Selection) {
        let target = forward_word_end(&self.rope, sel.head.char_index);
        sel.head.char_index = target;
        self.recompute_cursor(&mut sel.head);
    }
```

- [ ] **Step 8: 运行测试确认通过**

Run: `cargo test --lib move_head_word 2>&1 | Select-String "test result|FAILED|error"`
Expected: 3 tests passed, 0 failed

- [ ] **Step 9: Commit**

```bash
git add src/core/buffer.rs
git commit -m "feat: add word motion helpers and Buffer move_head_word_* methods"
```

---

### Task 3: 行移动辅助函数 + Buffer 原语

**Files:**
- Modify: `src/core/buffer.rs` (辅助函数区, selection 层方法区)

**Interfaces:**
- Produces: `first_non_blank_in_line`, `line_end_char`, `line_end_insert`, `prev_paragraph`, `next_paragraph` 模块函数；8 个 `move_head_*` Buffer 方法

- [ ] **Step 1: 写失败测试——行移动辅助函数**

在 `src/core/buffer.rs` 测试模块中添加：

```rust
    #[test]
    fn first_non_blank_finds_first_non_whitespace() {
        let mut buffer = Buffer::new();
        for (i, ch) in "  foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(first_non_blank_in_line(rope, 0), 2);
    }

    #[test]
    fn first_non_blank_all_blank_returns_line_start() {
        let mut buffer = Buffer::new();
        for (i, ch) in "   \n".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(first_non_blank_in_line(rope, 0), 0);
    }

    #[test]
    fn line_end_char_returns_last_non_newline_index() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(line_end_char(rope, 0), 2); // 'o' of "foo"
        assert_eq!(line_end_char(rope, 1), 6); // 'r' of "bar"
    }

    #[test]
    fn line_end_char_empty_line_returns_line_start() {
        let mut buffer = Buffer::new();
        for (i, ch) in "\n".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(line_end_char(rope, 0), 0);
    }

    #[test]
    fn line_end_insert_returns_position_after_last_char() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(line_end_insert(rope, 0), 3); // after 'o', before '\n'
    }

    #[test]
    fn prev_paragraph_finds_previous_empty_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        // char_index 5 is 'b' in "bar" on line 2; prev empty line is line 1 (char 4)
        assert_eq!(prev_paragraph(rope, 5), 4);
    }

    #[test]
    fn next_paragraph_finds_next_empty_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        // char_index 0 is 'f' on line 0; next empty line is line 1 (char 4)
        assert_eq!(next_paragraph(rope, 0), 4);
    }

    #[test]
    fn prev_paragraph_no_empty_line_stays_at_first_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        assert_eq!(prev_paragraph(rope, 5), 0);
    }

    #[test]
    fn next_paragraph_no_empty_line_stays_at_last_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let rope = buffer.slice();
        // No empty line; last line starts at char 4
        assert_eq!(next_paragraph(rope, 0), 4);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib first_non_blank line_end prev_paragraph next_paragraph 2>&1 | Select-String "cannot find"`
Expected: 编译错误 `cannot find function`

- [ ] **Step 3: 实现行移动辅助函数**

在 `src/core/buffer.rs` 的 `char_class` 函数之后添加：

```rust
fn first_non_blank_in_line(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    let line = rope.line(row);
    let mut offset = 0;
    for (i, ch) in line.chars().enumerate() {
        if ch == '\n' {
            break;
        }
        if !ch.is_whitespace() {
            offset = i;
            break;
        }
        offset = i + 1;
    }
    line_start + offset
}

fn line_end_char(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    let content_len = line_content_len(rope, row);
    if content_len == 0 {
        line_start
    } else {
        line_start + content_len - 1
    }
}

fn line_end_insert(rope: &Rope, row: usize) -> usize {
    let line_start = rope.line_to_char(row);
    line_start + line_content_len(rope, row)
}

fn is_empty_line(rope: &Rope, row: usize) -> bool {
    line_content_len(rope, row) == 0
}

fn prev_paragraph(rope: &Rope, char_index: usize) -> usize {
    let cur_row = rope.char_to_line(char_index.min(rope.len_chars()));
    if cur_row == 0 {
        return 0;
    }
    let mut row = cur_row - 1;
    loop {
        if is_empty_line(rope, row) {
            return rope.line_to_char(row);
        }
        if row == 0 {
            break;
        }
        row -= 1;
    }
    0
}

fn next_paragraph(rope: &Rope, char_index: usize) -> usize {
    let cur_row = rope.char_to_line(char_index.min(rope.len_chars()));
    let max_row = rope.len_lines().saturating_sub(1);
    for row in (cur_row + 1)..=max_row {
        if is_empty_line(rope, row) {
            return rope.line_to_char(row);
        }
    }
    rope.line_to_char(max_row)
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib first_non_blank line_end prev_paragraph next_paragraph 2>&1 | Select-String "test result|FAILED|error"`
Expected: all passed

- [ ] **Step 5: 写失败测试——`move_head_to_*` Buffer 方法**

在 `src/core/buffer.rs` 测试模块中添加：

```rust
    #[test]
    fn move_head_to_line_start_goes_to_column_zero() {
        let mut buffer = Buffer::new();
        for (i, ch) in "  foo\n  bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 7); // on 'b' of line 2
        buffer.move_head_to_line_start(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 6); // line 2 start
    }

    #[test]
    fn move_head_to_first_non_blank_skips_leading_ws() {
        let mut buffer = Buffer::new();
        for (i, ch) in "  foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_to_first_non_blank(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 2);
    }

    #[test]
    fn move_head_to_line_end_lands_on_last_char() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_to_line_end(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 2); // last 'o'
    }

    #[test]
    fn move_head_after_line_end_lands_after_last_char() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_after_line_end(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 3); // after 'o', before '\n'
    }

    #[test]
    fn move_head_to_last_line_goes_to_last_line_start() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar\nbaz".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.move_head_to_last_line(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 8); // start of "baz"
    }

    #[test]
    fn move_head_to_prev_paragraph_jumps_to_empty_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 5); // 'b' of "bar"
        buffer.move_head_to_prev_paragraph(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 4); // empty line
    }

    #[test]
    fn move_head_to_next_paragraph_jumps_to_empty_line() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0); // 'f' of "foo"
        buffer.move_head_to_next_paragraph(s.primary_mut());
        assert_eq!(s.primary().head().char_index, 4); // empty line
    }
```

- [ ] **Step 6: 运行测试确认失败**

Run: `cargo test --lib move_head_to_ 2>&1 | Select-String "cannot find|error"`
Expected: 编译错误 `no method named move_head_to_line_start`

- [ ] **Step 7: 实现 `move_head_to_*` 方法**

在 `src/core/buffer.rs` 的 selection 层方法区（`move_head_word_end` 之后、`set_head` 之前）添加：

```rust
    pub fn move_head_to_line_start(&self, sel: &mut Selection) {
        let row = self.rope.char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = self.rope.line_to_char(row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_first_non_blank(&self, sel: &mut Selection) {
        let row = self.rope.char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = first_non_blank_in_line(&self.rope, row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_line_end(&self, sel: &mut Selection) {
        let row = self.rope.char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = line_end_char(&self.rope, row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_after_line_end(&self, sel: &mut Selection) {
        let row = self.rope.char_to_line(sel.head.char_index.min(self.rope.len_chars()));
        sel.head.char_index = line_end_insert(&self.rope, row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_last_line(&self, sel: &mut Selection) {
        let max_row = self.rope.len_lines().saturating_sub(1);
        sel.head.char_index = self.rope.line_to_char(max_row);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_prev_paragraph(&self, sel: &mut Selection) {
        sel.head.char_index = prev_paragraph(&self.rope, sel.head.char_index);
        self.recompute_cursor(&mut sel.head);
    }

    pub fn move_head_to_next_paragraph(&self, sel: &mut Selection) {
        sel.head.char_index = next_paragraph(&self.rope, sel.head.char_index);
        self.recompute_cursor(&mut sel.head);
    }
```

- [ ] **Step 8: 运行测试确认通过**

Run: `cargo test --lib move_head_to_ 2>&1 | Select-String "test result|FAILED|error"`
Expected: all passed

- [ ] **Step 9: Commit**

```bash
git add src/core/buffer.rs
git commit -m "feat: add line motion helpers and Buffer move_head_to_* methods"
```

---

### Task 4: 删除/编辑原语 — DeleteToLineStart, DeleteToLineEnd

**Files:**
- Modify: `src/core/buffer.rs` (selection 层方法区)

**Interfaces:**
- Produces: `delete_to_line_start_at_selections`, `delete_to_line_end_at_selections` Buffer 方法

- [ ] **Step 1: 写失败测试——`delete_to_line_start_at_selections`**

在 `src/core/buffer.rs` 测试模块中添加：

```rust
    #[test]
    fn delete_to_line_start_removes_from_line_start_to_cursor() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 6); // on 'b' of line 2
        buffer.delete_to_line_start_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\nar");
        assert_eq!(s.primary().head().char_index, 4); // line 2 start
    }

    #[test]
    fn delete_to_line_start_at_line_start_is_noop() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.delete_to_line_start_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo");
        assert_eq!(s.primary().head().char_index, 0);
    }

    #[test]
    fn delete_to_line_start_non_empty_selection_deletes_range() {
        let mut buffer = Buffer::new();
        for (i, ch) in "abcdef".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 2);
        s.primary_mut().head = selection_at(&buffer, 5).primary().head;
        buffer.delete_to_line_start_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "af");
        assert_eq!(s.primary().head().char_index, 2);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib delete_to_line_start 2>&1 | Select-String "cannot find"`
Expected: `no method named delete_to_line_start_at_selections`

- [ ] **Step 3: 实现 `delete_to_line_start_at_selections`**

在 `src/core/buffer.rs` 的 `delete_word_backward_at_selections` 之后（`}` 闭合 impl 块之前）添加：

```rust
    pub fn delete_to_line_start_at_selections(&mut self, selections: &mut Selections) {
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b))
                } else {
                    let row = self.rope.char_to_line(s.head.char_index.min(self.rope.len_chars()));
                    let line_start = self.rope.line_to_char(row);
                    (line_start, s.head.char_index)
                }
            })
            .collect();
        let mut sorted = ranges.clone();
        sorted.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
        sorted.dedup();
        for (start, end) in &sorted {
            if end > start {
                self.rope.remove(*start..*end);
            }
        }
        self.modified = true;
        for (sel, (start, _)) in selections.all_mut().zip(ranges.iter()) {
            let mut deleted_before = 0;
            for &(r_start, r_end) in &sorted {
                if r_end <= *start {
                    deleted_before += r_end - r_start;
                }
            }
            sel.head.char_index = start - deleted_before;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib delete_to_line_start 2>&1 | Select-String "test result|FAILED"`
Expected: 3 passed, 0 failed

- [ ] **Step 5: 写失败测试——`delete_to_line_end_at_selections`**

```rust
    #[test]
    fn delete_to_line_end_removes_from_cursor_to_line_end() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1); // on first 'o'
        buffer.delete_to_line_end_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "f\nbar");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn delete_to_line_end_at_line_end_is_noop() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 3); // past end
        buffer.delete_to_line_end_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo");
        assert_eq!(s.primary().head().char_index, 3);
    }

    #[test]
    fn delete_to_line_end_non_empty_selection_deletes_range() {
        let mut buffer = Buffer::new();
        for (i, ch) in "abcdef".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 2);
        s.primary_mut().head = selection_at(&buffer, 4).primary().head;
        buffer.delete_to_line_end_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "abef");
        assert_eq!(s.primary().head().char_index, 2);
    }
```

- [ ] **Step 6: 运行测试确认失败**

Run: `cargo test --lib delete_to_line_end 2>&1 | Select-String "cannot find"`
Expected: `no method named delete_to_line_end_at_selections`

- [ ] **Step 7: 实现 `delete_to_line_end_at_selections`**

在 `delete_to_line_start_at_selections` 之后添加：

```rust
    pub fn delete_to_line_end_at_selections(&mut self, selections: &mut Selections) {
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b))
                } else {
                    let row = self.rope.char_to_line(s.head.char_index.min(self.rope.len_chars()));
                    let end = line_end_insert(&self.rope, row);
                    (s.head.char_index.min(end), end)
                }
            })
            .collect();
        let mut sorted = ranges.clone();
        sorted.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
        sorted.dedup();
        for (start, end) in &sorted {
            if end > start {
                self.rope.remove(*start..*end);
            }
        }
        self.modified = true;
        for (sel, (start, end)) in selections.all_mut().zip(ranges.iter()) {
            let mut deleted_before = 0;
            for &(r_start, r_end) in &sorted {
                if r_end <= *start {
                    deleted_before += r_end - r_start;
                }
            }
            sel.head.char_index = start - deleted_before;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }
```

- [ ] **Step 8: 运行测试确认通过**

Run: `cargo test --lib delete_to_line_end 2>&1 | Select-String "test result|FAILED"`
Expected: 3 passed, 0 failed

- [ ] **Step 9: Commit**

```bash
git add src/core/buffer.rs
git commit -m "feat: add delete_to_line_start/end_at_selections Buffer methods"
```

---

### Task 5: 编辑原语 — JoinLines, ToggleCase

**Files:**
- Modify: `src/core/buffer.rs`

**Interfaces:**
- Produces: `join_lines_at_selections`, `toggle_case_at_selections` Buffer 方法

- [ ] **Step 1: 写失败测试——`join_lines_at_selections`**

```rust
    #[test]
    fn join_lines_merges_two_lines_with_space() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.join_lines_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo bar");
        assert_eq!(s.primary().head().char_index, 3); // at the space
    }

    #[test]
    fn join_lines_strips_next_line_leading_whitespace() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\n  bar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.join_lines_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo bar");
        assert_eq!(s.primary().head().char_index, 3);
    }

    #[test]
    fn join_lines_on_last_line_is_noop() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 4); // on 'b' of last line
        buffer.join_lines_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\nbar");
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib join_lines 2>&1 | Select-String "cannot find"`
Expected: `no method named join_lines_at_selections`

- [ ] **Step 3: 实现 `join_lines_at_selections`**

在 `delete_to_line_end_at_selections` 之后添加：

```rust
    pub fn join_lines_at_selections(&mut self, selections: &mut Selections) {
        let max_row = self.rope.len_lines().saturating_sub(1);
        let mut joins: Vec<(usize, usize, usize)> = selections
            .all()
            .map(|s| {
                let row = self.rope.char_to_line(s.head.char_index.min(self.rope.len_chars()));
                if row >= max_row {
                    return None;
                }
                let newline_pos = self.rope.line_to_char(row) + line_content_len(&self.rope, row);
                let next_line_start = newline_pos + 1;
                let next_row = row + 1;
                let next_content_len = line_content_len(&self.rope, next_row);
                let next_content_start = next_line_start;
                // Count leading whitespace on next line
                let mut ws_len = 0;
                for i in 0..next_content_len {
                    if self.rope.char(next_content_start + i).is_whitespace() {
                        ws_len += 1;
                    } else {
                        break;
                    }
                }
                Some((newline_pos, next_content_start + ws_len, next_line_start))
            })
            .collect::<Vec<_>>();
        joins.retain(|j| j.is_some());
        let joins: Vec<(usize, usize, usize)> = joins.into_iter().map(|j| j.unwrap()).collect();
        // Remove in reverse: delete [next_content_start, next_line_start) (leading ws) then remove newline
        // Simpler: remove [newline_pos, next_content_start + ws_len) and insert " "
        // Actually: remove range [newline_pos, next_line_start + ws_len) then insert " " at newline_pos
        let mut sorted_joins = joins.clone();
        sorted_joins.sort_unstable_by_key(|j| std::cmp::Reverse(j.0));
        for (newline_pos, strip_end, _) in &sorted_joins {
            self.rope.remove(*newline_pos..*strip_end);
            self.rope.insert(*newline_pos, " ");
        }
        self.modified = true;
        for (sel, (newline_pos, _, _)) in selections.all_mut().zip(joins.iter()) {
            sel.head.char_index = *newline_pos;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib join_lines 2>&1 | Select-String "test result|FAILED"`
Expected: 3 passed, 0 failed

- [ ] **Step 5: 写失败测试——`toggle_case_at_selections`**

```rust
    #[test]
    fn toggle_case_flips_char_and_advances() {
        let mut buffer = Buffer::new();
        for (i, ch) in "aBc".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        buffer.toggle_case_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "ABc");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn toggle_case_at_line_end_does_not_advance() {
        let mut buffer = Buffer::new();
        for (i, ch) in "ab".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1);
        buffer.toggle_case_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "aB");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn toggle_case_non_empty_selection_flips_all_in_range() {
        let mut buffer = Buffer::new();
        for (i, ch) in "abc".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 0);
        s.primary_mut().head = selection_at(&buffer, 3).primary().head;
        buffer.toggle_case_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "ABC");
        assert_eq!(s.primary().head().char_index, 3);
    }
```

- [ ] **Step 6: 运行测试确认失败**

Run: `cargo test --lib toggle_case 2>&1 | Select-String "cannot find"`
Expected: `no method named toggle_case_at_selections`

- [ ] **Step 7: 实现 `toggle_case_at_selections`**

在 `join_lines_at_selections` 之后添加：

```rust
    pub fn toggle_case_at_selections(&mut self, selections: &mut Selections) {
        let len = self.rope.len_chars();
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                if s.anchor != s.head {
                    let (a, b) = (s.anchor.char_index, s.head.char_index);
                    (a.min(b), a.max(b))
                } else {
                    let ci = s.head.char_index.min(len);
                    if ci < len {
                        (ci, ci + 1)
                    } else {
                        (ci, ci)
                    }
                }
            })
            .collect();
        for (start, end) in &ranges {
            if end > start {
                let slice = self.rope.slice(*start..*end);
                let flipped: String = slice
                    .chars()
                    .map(|c| {
                        if c.is_uppercase() {
                            c.to_lowercase().next().unwrap_or(c)
                        } else if c.is_lowercase() {
                            c.to_uppercase().next().unwrap_or(c)
                        } else {
                            c
                        }
                    })
                    .collect();
                self.rope.remove(*start..*end);
                self.rope.insert(*start, &flipped);
            }
        }
        self.modified = true;
        for (sel, (start, end)) in selections.all_mut().zip(ranges.iter()) {
            if sel.anchor == sel.head {
                // Collapsed: advance head by 1 unless at end
                if sel.head.char_index < len {
                    sel.head.char_index += 1;
                }
            } else {
                sel.head.char_index = *end;
            }
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }
```

- [ ] **Step 8: 运行测试确认通过**

Run: `cargo test --lib toggle_case 2>&1 | Select-String "test result|FAILED"`
Expected: 3 passed, 0 failed

- [ ] **Step 9: Commit**

```bash
git add src/core/buffer.rs
git commit -m "feat: add join_lines and toggle_case Buffer methods"
```

---

### Task 6: 编辑原语 — InsertNewLineBelow/Above, DeleteLineContent

**Files:**
- Modify: `src/core/buffer.rs`

**Interfaces:**
- Produces: `insert_new_line_below_at_selections`, `insert_new_line_above_at_selections`, `delete_line_content_at_selections` Buffer 方法

- [ ] **Step 1: 写失败测试——`insert_new_line_below_at_selections`**

```rust
    #[test]
    fn insert_new_line_below_adds_line_and_moves_cursor() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1);
        buffer.insert_new_line_below_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\n");
        assert_eq!(s.primary().head().char_index, 4); // start of new line
    }

    #[test]
    fn insert_new_line_below_multiline() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1); // on 'o' of line 1
        buffer.insert_new_line_below_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\n\nbar");
        assert_eq!(s.primary().head().char_index, 4); // new empty line
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib insert_new_line_below 2>&1 | Select-String "cannot find"`
Expected: `no method named insert_new_line_below_at_selections`

- [ ] **Step 3: 实现 `insert_new_line_below_at_selections`**

在 `toggle_case_at_selections` 之后添加：

```rust
    pub fn insert_new_line_below_at_selections(&mut self, selections: &mut Selections) {
        let insert_points: Vec<usize> = selections
            .all()
            .map(|s| {
                let row = self.rope.char_to_line(s.head.char_index.min(self.rope.len_chars()));
                self.rope.line_to_char(row) + line_content_len(&self.rope, row)
            })
            .collect();
        let mut sorted = insert_points.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        sorted.dedup();
        for pos in &sorted {
            self.rope.insert(*pos, "\n");
        }
        self.modified = true;
        for (sel, pos) in selections.all_mut().zip(insert_points.iter()) {
            sel.head.char_index = *pos + 1;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib insert_new_line_below 2>&1 | Select-String "test result|FAILED"`
Expected: 2 passed, 0 failed

- [ ] **Step 5: 写失败测试——`insert_new_line_above_at_selections`**

```rust
    #[test]
    fn insert_new_line_above_adds_line_and_keeps_cursor() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1);
        buffer.insert_new_line_above_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "\nfoo");
        assert_eq!(s.primary().head().char_index, 0); // start of new line
    }

    #[test]
    fn insert_new_line_above_multiline() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 5); // on 'a' of line 2
        buffer.insert_new_line_above_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\n\nbar");
        assert_eq!(s.primary().head().char_index, 4); // new empty line start
    }
```

- [ ] **Step 6: 运行测试确认失败**

Run: `cargo test --lib insert_new_line_above 2>&1 | Select-String "cannot find"`
Expected: `no method named insert_new_line_above_at_selections`

- [ ] **Step 7: 实现 `insert_new_line_above_at_selections`**

在 `insert_new_line_below_at_selections` 之后添加：

```rust
    pub fn insert_new_line_above_at_selections(&mut self, selections: &mut Selections) {
        let insert_points: Vec<usize> = selections
            .all()
            .map(|s| {
                let row = self.rope.char_to_line(s.head.char_index.min(self.rope.len_chars()));
                self.rope.line_to_char(row)
            })
            .collect();
        let mut sorted = insert_points.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        sorted.dedup();
        for pos in &sorted {
            self.rope.insert(*pos, "\n");
        }
        self.modified = true;
        for (sel, pos) in selections.all_mut().zip(insert_points.iter()) {
            sel.head.char_index = *pos;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }
```

- [ ] **Step 8: 运行测试确认通过**

Run: `cargo test --lib insert_new_line_above 2>&1 | Select-String "test result|FAILED"`
Expected: 2 passed, 0 failed

- [ ] **Step 9: 写失败测试——`delete_line_content_at_selections`**

```rust
    #[test]
    fn delete_line_content_clears_line_keeps_newline() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 1); // on 'o' of line 1
        buffer.delete_line_content_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "\nbar");
        assert_eq!(s.primary().head().char_index, 0);
    }

    #[test]
    fn delete_line_content_last_line_no_newline() {
        let mut buffer = Buffer::new();
        for (i, ch) in "foo\nbar".chars().enumerate() {
            buffer.insert_char(i, ch);
        }
        let mut s = selection_at(&buffer, 5); // on 'a' of line 2
        buffer.delete_line_content_at_selections(&mut s);
        assert_eq!(buffer.slice().to_string(), "foo\n");
        assert_eq!(s.primary().head().char_index, 4);
    }
```

- [ ] **Step 10: 运行测试确认失败**

Run: `cargo test --lib delete_line_content 2>&1 | Select-String "cannot find"`
Expected: `no method named delete_line_content_at_selections`

- [ ] **Step 11: 实现 `delete_line_content_at_selections`**

在 `insert_new_line_above_at_selections` 之后添加：

```rust
    pub fn delete_line_content_at_selections(&mut self, selections: &mut Selections) {
        let ranges: Vec<(usize, usize)> = selections
            .all()
            .map(|s| {
                let row = self.rope.char_to_line(s.head.char_index.min(self.rope.len_chars()));
                let line_start = self.rope.line_to_char(row);
                let content_end = line_start + line_content_len(&self.rope, row);
                (line_start, content_end)
            })
            .collect();
        let mut sorted = ranges.clone();
        sorted.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
        sorted.dedup();
        for (start, end) in &sorted {
            if end > start {
                self.rope.remove(*start..*end);
            }
        }
        self.modified = true;
        for (sel, (start, _)) in selections.all_mut().zip(ranges.iter()) {
            sel.head.char_index = *start;
            self.recompute_cursor(&mut sel.head);
            Self::collapse_to_head(sel);
        }
    }
```

- [ ] **Step 12: 运行测试确认通过**

Run: `cargo test --lib delete_line_content 2>&1 | Select-String "test result|FAILED"`
Expected: 2 passed, 0 failed

- [ ] **Step 13: Commit**

```bash
git add src/core/buffer.rs
git commit -m "feat: add insert_new_line_below/above and delete_line_content Buffer methods"
```

---

### Task 7: apply_edit 分支 — 移动命令

**Files:**
- Modify: `src/core/edit.rs:7-92`

**Interfaces:**
- Consumes: `move_head_word_forward`/`backward`/`end`/`to_line_start`/`to_first_non_blank`/`to_line_end`/`to_last_line`/`to_prev_paragraph`/`to_next_paragraph`/`after_line_end` (Task 2, 3)
- Produces: `apply_edit` 能处理 10 个新移动命令

- [ ] **Step 1: 写失败测试——移动命令在 edit.rs 中的分发**

在 `src/core/edit.rs` 测试模块中添加：

```rust
    #[test]
    fn move_word_forward_advances_head() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo bar");
        let mut s = single_sel({
            let mut c = CursorPos::origin();
            c.char_index = 0;
            buf.recompute_cursor(&mut c);
            c
        });
        apply_edit(EditCommand::MoveWordForward, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 4);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_word_backward_advances_head() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo bar");
        let mut s = single_sel({
            let mut c = CursorPos::origin();
            c.char_index = 7;
            buf.recompute_cursor(&mut c);
            c
        });
        apply_edit(EditCommand::MoveWordBackward, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 4);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_word_end_advances_head() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo.bar");
        let mut s = single_sel({
            let mut c = CursorPos::origin();
            c.char_index = 0;
            buf.recompute_cursor(&mut c);
            c
        });
        apply_edit(EditCommand::MoveWordEnd, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_to_line_start_goes_to_column_zero() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "  foo\n  bar");
        let mut s = single_sel({
            let mut c = CursorPos::origin();
            c.char_index = 7;
            buf.recompute_cursor(&mut c);
            c
        });
        apply_edit(EditCommand::MoveToLineStart, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 6);
    }

    #[test]
    fn move_to_first_non_blank_skips_whitespace() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "  foo");
        let mut s = single_sel(CursorPos::origin());
        apply_edit(EditCommand::MoveToFirstNonBlank, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 2);
    }

    #[test]
    fn move_to_line_end_lands_on_last_char() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo\nbar");
        let mut s = single_sel(CursorPos::origin());
        apply_edit(EditCommand::MoveToLineEnd, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 2);
    }

    #[test]
    fn move_to_last_line_goes_to_last_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo\nbar\nbaz");
        let mut s = single_sel(CursorPos::origin());
        apply_edit(EditCommand::MoveToLastLine, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 8);
    }

    #[test]
    fn move_to_prev_paragraph_jumps_to_empty_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo\n\nbar");
        let mut s = single_sel({
            let mut c = CursorPos::origin();
            c.char_index = 5;
            buf.recompute_cursor(&mut c);
            c
        });
        apply_edit(EditCommand::MoveToPrevParagraph, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn move_to_next_paragraph_jumps_to_empty_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo\n\nbar");
        let mut s = single_sel(CursorPos::origin());
        apply_edit(EditCommand::MoveToNextParagraph, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn move_after_line_end_lands_after_last_char() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo\n");
        let mut s = single_sel(CursorPos::origin());
        apply_edit(EditCommand::MoveAfterLineEnd, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 3);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib move_word_forward move_word_backward move_word_end move_to_line_start move_to_first_non_blank move_to_line_end move_to_last_line move_to_prev_paragraph move_to_next_paragraph move_after_line_end 2>&1 | Select-String "error"`
Expected: 编译错误，match arms missing

- [ ] **Step 3: 实现移动命令的 apply_edit 分支**

在 `src/core/edit.rs` 的 `apply_edit` 函数中，在 `EditCommand::MoveDownBy(n)` 分支之后（`MoveBy` 之前）添加所有 10 个移动命令分支：

```rust
        EditCommand::MoveWordForward => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_word_forward(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveWordBackward => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_word_backward(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveWordEnd => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_word_end(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToLineStart => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_line_start(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToFirstNonBlank => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_first_non_blank(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToLineEnd => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_line_end(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveAfterLineEnd => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_after_line_end(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToLastLine => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_last_line(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToPrevParagraph => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_prev_paragraph(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToNextParagraph => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_next_paragraph(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib move_word_forward move_word_backward move_word_end move_to_line_start move_to_first_non_blank move_to_line_end move_to_last_line move_to_prev_paragraph move_to_next_paragraph move_after_line_end 2>&1 | Select-String "test result|FAILED"`
Expected: 10 passed, 0 failed

- [ ] **Step 5: Commit**

```bash
git add src/core/edit.rs
git commit -m "feat: add apply_edit branches for 10 motion commands"
```

---

### Task 8: apply_edit 分支 — 删除/编辑命令

**Files:**
- Modify: `src/core/edit.rs`

**Interfaces:**
- Consumes: `delete_to_line_start_at_selections`/`delete_to_line_end_at_selections`/`join_lines_at_selections`/`toggle_case_at_selections`/`insert_new_line_below_at_selections`/`insert_new_line_above_at_selections`/`delete_line_content_at_selections` (Task 4, 5, 6)
- Produces: `apply_edit` 能处理 7 个新编辑命令

- [ ] **Step 1: 写失败测试——编辑命令在 edit.rs 中的分发**

在 `src/core/edit.rs` 测试模块中添加：

```rust
    #[test]
    fn delete_to_line_start_removes_text() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo\nbar");
        let mut s = single_sel({
            let mut c = CursorPos::origin();
            c.char_index = 6;
            buf.recompute_cursor(&mut c);
            c
        });
        apply_edit(EditCommand::DeleteToLineStart, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "foo\nar");
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn delete_to_line_end_removes_text() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo\nbar");
        let mut s = single_sel({
            let mut c = CursorPos::origin();
            c.char_index = 1;
            buf.recompute_cursor(&mut c);
            c
        });
        apply_edit(EditCommand::DeleteToLineEnd, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "f\nbar");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn join_lines_merges_lines() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo\nbar");
        let mut s = single_sel(CursorPos::origin());
        apply_edit(EditCommand::JoinLines, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "foo bar");
    }

    #[test]
    fn toggle_case_flips_char() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "aBc");
        let mut s = single_sel(CursorPos::origin());
        apply_edit(EditCommand::ToggleCase, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "ABc");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn insert_new_line_below_adds_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo");
        let mut s = single_sel(CursorPos::origin());
        apply_edit(EditCommand::InsertNewLineBelow, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "foo\n");
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn insert_new_line_above_adds_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo");
        let mut s = single_sel(CursorPos::origin());
        apply_edit(EditCommand::InsertNewLineAbove, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "\nfoo");
        assert_eq!(s.primary().head().char_index, 0);
    }

    #[test]
    fn delete_line_content_clears_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(CursorPos::origin()), "foo\nbar");
        let mut s = single_sel({
            let mut c = CursorPos::origin();
            c.char_index = 1;
            buf.recompute_cursor(&mut c);
            c
        });
        apply_edit(EditCommand::DeleteLineContent, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "\nbar");
        assert_eq!(s.primary().head().char_index, 0);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib delete_to_line_start_removes delete_to_line_end_removes join_lines_merges toggle_case_flips insert_new_line_below_adds insert_new_line_above_adds delete_line_content_clears 2>&1 | Select-String "error"`
Expected: 编译错误，match arms missing

- [ ] **Step 3: 实现编辑命令的 apply_edit 分支**

在 `src/core/edit.rs` 的 `apply_edit` 函数中，在 `EditCommand::DeleteWordBackward` 分支之后（函数末尾 `}` 之前）添加：

```rust
        EditCommand::DeleteToLineStart => {
            buffer.delete_to_line_start_at_selections(selections);
        }
        EditCommand::DeleteToLineEnd => {
            buffer.delete_to_line_end_at_selections(selections);
        }
        EditCommand::JoinLines => {
            buffer.join_lines_at_selections(selections);
        }
        EditCommand::ToggleCase => {
            buffer.toggle_case_at_selections(selections);
        }
        EditCommand::InsertNewLineBelow => {
            buffer.insert_new_line_below_at_selections(selections);
        }
        EditCommand::InsertNewLineAbove => {
            buffer.insert_new_line_above_at_selections(selections);
        }
        EditCommand::DeleteLineContent => {
            buffer.delete_line_content_at_selections(selections);
        }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib delete_to_line_start_removes delete_to_line_end_removes join_lines_merges toggle_case_flips insert_new_line_below_adds insert_new_line_above_adds delete_line_content_clears 2>&1 | Select-String "test result|FAILED"`
Expected: 7 passed, 0 failed

- [ ] **Step 5: Commit**

```bash
git add src/core/edit.rs
git commit -m "feat: add apply_edit branches for 7 edit commands"
```

---

### Task 9: Insert 模式键映射 — Ctrl+U/K/J/M

**Files:**
- Modify: `src/core/mode.rs:250-257`

**Interfaces:**
- Consumes: `EditCommand::DeleteToLineStart`, `DeleteToLineEnd` (Task 1)
- Produces: Insert keymap 绑定 4 个新 Ctrl 键

- [ ] **Step 1: 写失败测试——Insert 模式 Ctrl 键解析**

在 `src/core/mode.rs` 测试模块中添加：

```rust
    #[test]
    fn vim_insert_ctrl_u_resolves_to_delete_to_line_start() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('u')),
            Some(EditCommand::DeleteToLineStart.into()),
        );
    }

    #[test]
    fn vim_insert_ctrl_k_resolves_to_delete_to_line_end() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('k')),
            Some(EditCommand::DeleteToLineEnd.into()),
        );
    }

    #[test]
    fn vim_insert_ctrl_j_resolves_to_insert_newline() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('j')),
            Some(EditCommand::InsertText("\n".to_string()).into()),
        );
    }

    #[test]
    fn vim_insert_ctrl_m_resolves_to_insert_newline() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        modes.execute(
            &mut runtime,
            ModeId::new("vim"),
            ModeActionId::new("enter-insert"),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::ctrl('m')),
            Some(EditCommand::InsertText("\n".to_string()).into()),
        );
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib vim_insert_ctrl_u vim_insert_ctrl_k vim_insert_ctrl_j vim_insert_ctrl_m 2>&1 | Select-String "FAILED|assertion"`
Expected: 4 tests FAILED (resolve_key returns None)

- [ ] **Step 3: 在 `vim_insert_keymap` 中添加 Ctrl+U/K/J/M 绑定**

在 `src/core/mode.rs` 的 `vim_insert_keymap` 函数中，在 `Ctrl+W` 绑定之后添加：

```rust
    km.bind_edit(KeyEvent::ctrl('u'), EditCommand::DeleteToLineStart);
    km.bind_edit(KeyEvent::ctrl('k'), EditCommand::DeleteToLineEnd);
    km.bind_edit(KeyEvent::ctrl('j'), EditCommand::InsertText("\n".to_string()));
    km.bind_edit(KeyEvent::ctrl('m'), EditCommand::InsertText("\n".to_string()));
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib vim_insert_ctrl_u vim_insert_ctrl_k vim_insert_ctrl_j vim_insert_ctrl_m 2>&1 | Select-String "test result|FAILED"`
Expected: 4 passed, 0 failed

- [ ] **Step 5: Commit**

```bash
git add src/core/mode.rs
git commit -m "feat: bind Ctrl+U/K/J/M in vim insert keymap"
```

---

### Task 10: Normal 模式键映射 — 移动命令 (w/b/e/0/^/$/G/{/})

**Files:**
- Modify: `src/core/mode.rs:306-328`

**Interfaces:**
- Consumes: 10 个移动 `EditCommand` 变体 (Task 1)
- Produces: Normal keymap 绑定 10 个移动键

- [ ] **Step 1: 写失败测试——Normal 模式移动键解析**

在 `src/core/mode.rs` 测试模块中添加：

```rust
    #[test]
    fn vim_normal_w_resolves_to_move_word_forward() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('w')),
            Some(EditCommand::MoveWordForward.into()),
        );
    }

    #[test]
    fn vim_normal_b_resolves_to_move_word_backward() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('b')),
            Some(EditCommand::MoveWordBackward.into()),
        );
    }

    #[test]
    fn vim_normal_e_resolves_to_move_word_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('e')),
            Some(EditCommand::MoveWordEnd.into()),
        );
    }

    #[test]
    fn vim_normal_zero_resolves_to_move_to_line_start() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('0')),
            Some(EditCommand::MoveToLineStart.into()),
        );
    }

    #[test]
    fn vim_normal_caret_resolves_to_move_to_first_non_blank() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('^')),
            Some(EditCommand::MoveToFirstNonBlank.into()),
        );
    }

    #[test]
    fn vim_normal_dollar_resolves_to_move_to_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('$')),
            Some(EditCommand::MoveToLineEnd.into()),
        );
    }

    #[test]
    fn vim_normal_capital_g_resolves_to_move_to_last_line() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('G')),
            Some(EditCommand::MoveToLastLine.into()),
        );
    }

    #[test]
    fn vim_normal_open_brace_resolves_to_prev_paragraph() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('{')),
            Some(EditCommand::MoveToPrevParagraph.into()),
        );
    }

    #[test]
    fn vim_normal_close_brace_resolves_to_next_paragraph() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('}')),
            Some(EditCommand::MoveToNextParagraph.into()),
        );
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib vim_normal_w vim_normal_b vim_normal_e vim_normal_zero vim_normal_caret vim_normal_dollar vim_normal_capital_g vim_normal_open_brace vim_normal_close_brace 2>&1 | Select-String "FAILED"`
Expected: 9 tests FAILED

- [ ] **Step 3: 在 `vim_normal_keymap` 中添加移动键绑定**

在 `src/core/mode.rs` 的 `vim_normal_keymap` 函数中，在 `l` 绑定之后、`i` 绑定之前添加：

```rust
    km.bind_edit(KeyEvent::char('w'), EditCommand::MoveWordForward);
    km.bind_edit(KeyEvent::char('b'), EditCommand::MoveWordBackward);
    km.bind_edit(KeyEvent::char('e'), EditCommand::MoveWordEnd);
    km.bind_edit(KeyEvent::char('0'), EditCommand::MoveToLineStart);
    km.bind_edit(KeyEvent::char('^'), EditCommand::MoveToFirstNonBlank);
    km.bind_edit(KeyEvent::char('$'), EditCommand::MoveToLineEnd);
    km.bind_edit(KeyEvent::char('G'), EditCommand::MoveToLastLine);
    km.bind_edit(KeyEvent::char('{'), EditCommand::MoveToPrevParagraph);
    km.bind_edit(KeyEvent::char('}'), EditCommand::MoveToNextParagraph);
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib vim_normal_w vim_normal_b vim_normal_e vim_normal_zero vim_normal_caret vim_normal_dollar vim_normal_capital_g vim_normal_open_brace vim_normal_close_brace 2>&1 | Select-String "test result|FAILED"`
Expected: 9 passed, 0 failed

- [ ] **Step 5: Commit**

```bash
git add src/core/mode.rs
git commit -m "feat: bind w/b/e/0/^/$/G/{/} in vim normal keymap"
```

---

### Task 11: Normal 模式键映射 — 编辑命令 (x/X/J/D/~)

**Files:**
- Modify: `src/core/mode.rs:306-328`

**Interfaces:**
- Consumes: `Delete(1)`, `Delete(-1)`, `DeleteToLineEnd`, `JoinLines`, `ToggleCase` (已有 + Task 1)
- Produces: Normal keymap 绑定 5 个编辑键

- [ ] **Step 1: 写失败测试——Normal 模式编辑键解析**

在 `src/core/mode.rs` 测试模块中添加：

```rust
    #[test]
    fn vim_normal_x_resolves_to_delete_forward() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('x')),
            Some(EditCommand::Delete(1).into()),
        );
    }

    #[test]
    fn vim_normal_capital_x_resolves_to_delete_backward() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('X')),
            Some(EditCommand::Delete(-1).into()),
        );
    }

    #[test]
    fn vim_normal_capital_j_resolves_to_join_lines() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('J')),
            Some(EditCommand::JoinLines.into()),
        );
    }

    #[test]
    fn vim_normal_capital_d_resolves_to_delete_to_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('D')),
            Some(EditCommand::DeleteToLineEnd.into()),
        );
    }

    #[test]
    fn vim_normal_tilde_resolves_to_toggle_case() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('~')),
            Some(EditCommand::ToggleCase.into()),
        );
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib vim_normal_x vim_normal_capital_x vim_normal_capital_j vim_normal_capital_d vim_normal_tilde 2>&1 | Select-String "FAILED"`
Expected: 5 tests FAILED

- [ ] **Step 3: 在 `vim_normal_keymap` 中添加编辑键绑定**

在 `src/core/mode.rs` 的 `vim_normal_keymap` 函数中，在 `}` (close brace) 绑定之后、`i` 绑定之前添加：

```rust
    km.bind_edit(KeyEvent::char('x'), EditCommand::Delete(1));
    km.bind_edit(KeyEvent::char('X'), EditCommand::Delete(-1));
    km.bind_edit(KeyEvent::char('J'), EditCommand::JoinLines);
    km.bind_edit(KeyEvent::char('D'), EditCommand::DeleteToLineEnd);
    km.bind_edit(KeyEvent::char('~'), EditCommand::ToggleCase);
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib vim_normal_x vim_normal_capital_x vim_normal_capital_j vim_normal_capital_d vim_normal_tilde 2>&1 | Select-String "test result|FAILED"`
Expected: 5 passed, 0 failed

- [ ] **Step 5: Commit**

```bash
git add src/core/mode.rs
git commit -m "feat: bind x/X/J/D/~ in vim normal keymap"
```

---

### Task 12: Normal 模式 mode action — o/O/I/A/s/C/S

**Files:**
- Modify: `src/core/mode.rs` (VimMode::execute ~line 226-242, vim_normal_keymap ~line 306-328)

**Interfaces:**
- Consumes: `InsertNewLineBelow`/`InsertNewLineAbove`/`MoveToFirstNonBlank`/`MoveAfterLineEnd`/`Delete(1)`/`DeleteToLineEnd`/`DeleteLineContent` (Task 1)
- Produces: 7 个新 mode action + 键绑定

- [ ] **Step 1: 写失败测试——mode action 返回正确 EditCommand 并切换 Insert**

在 `src/core/mode.rs` 测试模块中添加：

```rust
    #[test]
    fn vim_open_below_enters_insert_and_returns_new_line_below() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeId::new("vim"),
                ModeActionId::new("open-below"),
            ),
            Some(EditCommand::InsertNewLineBelow),
        );
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('x')),
            Some(EditCommand::InsertText("x".to_string()).into()),
        );
    }

    #[test]
    fn vim_open_above_enters_insert_and_returns_new_line_above() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeId::new("vim"),
                ModeActionId::new("open-above"),
            ),
            Some(EditCommand::InsertNewLineAbove),
        );
    }

    #[test]
    fn vim_insert_at_first_non_blank_enters_insert_and_returns_move() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeId::new("vim"),
                ModeActionId::new("insert-at-first-non-blank"),
            ),
            Some(EditCommand::MoveToFirstNonBlank),
        );
    }

    #[test]
    fn vim_append_at_line_end_enters_insert_and_returns_move_after_line_end() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeId::new("vim"),
                ModeActionId::new("append-at-line-end"),
            ),
            Some(EditCommand::MoveAfterLineEnd),
        );
    }

    #[test]
    fn vim_substitute_char_enters_insert_and_returns_delete_forward() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeId::new("vim"),
                ModeActionId::new("substitute-char"),
            ),
            Some(EditCommand::Delete(1)),
        );
    }

    #[test]
    fn vim_change_to_line_end_enters_insert_and_returns_delete_to_line_end() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeId::new("vim"),
                ModeActionId::new("change-to-line-end"),
            ),
            Some(EditCommand::DeleteToLineEnd),
        );
    }

    #[test]
    fn vim_substitute_line_enters_insert_and_returns_delete_line_content() {
        let modes = ModeSet::vim();
        let mut runtime = modes.create_runtime();
        assert_eq!(
            modes.execute(
                &mut runtime,
                ModeId::new("vim"),
                ModeActionId::new("substitute-line"),
            ),
            Some(EditCommand::DeleteLineContent),
        );
    }
```

- [ ] **Step 2: 写失败测试——Normal 模式键解析到 mode action**

```rust
    #[test]
    fn vim_normal_o_resolves_to_open_below_mode_command() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('o')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("open-below"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_o_resolves_to_open_above_mode_command() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('O')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("open-above"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_i_resolves_to_insert_at_first_non_blank() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('I')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("insert-at-first-non-blank"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_a_resolves_to_append_at_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('A')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("append-at-line-end"),
            })),
        );
    }

    #[test]
    fn vim_normal_s_resolves_to_substitute_char() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('s')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("substitute-char"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_c_resolves_to_change_to_line_end() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('C')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("change-to-line-end"),
            })),
        );
    }

    #[test]
    fn vim_normal_capital_s_resolves_to_substitute_line() {
        let modes = ModeSet::vim();
        let runtime = modes.create_runtime();
        assert_eq!(
            modes.resolve_key(&runtime, KeyEvent::char('S')),
            Some(Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("substitute-line"),
            })),
        );
    }
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib vim_open_below vim_open_above vim_insert_at_first vim_append_at_line vim_substitute_char vim_change_to_line vim_substitute_line vim_normal_o vim_normal_capital_o vim_normal_capital_i vim_normal_capital_a vim_normal_s vim_normal_capital_c vim_normal_capital_s 2>&1 | Select-String "FAILED|assertion"`
Expected: 14 tests FAILED

- [ ] **Step 4: 在 `VimMode::execute` 中添加 7 个 mode action**

在 `src/core/mode.rs` 的 `VimMode::execute` 方法中，在 `"append"` 分支之后、`_ => None` 之前添加：

```rust
            "open-below" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::InsertNewLineBelow)
            }
            "open-above" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::InsertNewLineAbove)
            }
            "insert-at-first-non-blank" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::MoveToFirstNonBlank)
            }
            "append-at-line-end" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::MoveAfterLineEnd)
            }
            "substitute-char" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::Delete(1))
            }
            "change-to-line-end" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::DeleteToLineEnd)
            }
            "substitute-line" => {
                self.state_mut(state).state = VimState::Insert;
                Some(EditCommand::DeleteLineContent)
            }
```

- [ ] **Step 5: 在 `vim_normal_keymap` 中添加 7 个 mode action 键绑定**

在 `src/core/mode.rs` 的 `vim_normal_keymap` 函数中，在 `~` 绑定之后、`i` 绑定之前添加：

```rust
    km.bind(
        KeyEvent::char('o'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("open-below"),
        }),
    );
    km.bind(
        KeyEvent::char('O'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("open-above"),
        }),
    );
    km.bind(
        KeyEvent::char('I'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("insert-at-first-non-blank"),
        }),
    );
    km.bind(
        KeyEvent::char('A'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("append-at-line-end"),
        }),
    );
    km.bind(
        KeyEvent::char('s'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("substitute-char"),
        }),
    );
    km.bind(
        KeyEvent::char('C'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("change-to-line-end"),
        }),
    );
    km.bind(
        KeyEvent::char('S'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("substitute-line"),
        }),
    );
```

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test --lib vim_open_below vim_open_above vim_insert_at_first vim_append_at_line vim_substitute_char vim_change_to_line vim_substitute_line vim_normal_o vim_normal_capital_o vim_normal_capital_i vim_normal_capital_a vim_normal_s vim_normal_capital_c vim_normal_capital_s 2>&1 | Select-String "test result|FAILED"`
Expected: 14 passed, 0 failed

- [ ] **Step 7: Commit**

```bash
git add src/core/mode.rs
git commit -m "feat: add 7 mode actions (o/O/I/A/s/C/S) and normal keymap bindings"
```

---

### Task 13: App 集成测试 — 端到端验证

**Files:**
- Modify: `src/app/mod.rs` (测试模块)

**Interfaces:**
- Consumes: 所有前序 Task 的键映射和命令

- [ ] **Step 1: 写端到端测试**

在 `src/app/mod.rs` 测试模块中添加（在 `default_vim_ctrl_w_deletes_previous_word` 测试之后）：

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_w_moves_to_next_word() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('f')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char(' ')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('r')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('h')),
                FrontendEvent::Key(KeyEvent::char('h')),
                FrontendEvent::Key(KeyEvent::char('h')),
                FrontendEvent::Key(KeyEvent::char('w')),
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["fooXbar"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_dollar_moves_to_line_end() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('$')),
                FrontendEvent::Key(KeyEvent::char('x')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_x_deletes_char() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('x')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["bc"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_o_opens_line_below_and_inserts() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('f')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('r')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["foo", "bar"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_capital_a_appends_at_line_end() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('f')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('A')),
                FrontendEvent::Key(KeyEvent::char('!')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["foo!"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_capital_d_deletes_to_line_end() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('l')),
                FrontendEvent::Key(KeyEvent::char('D')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_capital_j_joins_lines() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('f')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('r')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('k')),
                FrontendEvent::Key(KeyEvent::char('J')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["foo bar"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_tilde_toggles_case() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('~')),
                FrontendEvent::Key(KeyEvent::char('~')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["Ab"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_insert_ctrl_u_deletes_to_line_start() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::ctrl('u')),
                FrontendEvent::Key(KeyEvent::char('x')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["x"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_s_substitutes_char() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('s')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["Xb"]);
    }
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --lib vim_normal_w_moves vim_normal_dollar vim_normal_x_deletes vim_normal_o_opens vim_normal_capital_a vim_normal_capital_d vim_normal_capital_j vim_normal_tilde vim_insert_ctrl_u vim_normal_s 2>&1 | Select-String "test result|FAILED"`
Expected: 10 passed, 0 failed

- [ ] **Step 3: Commit**

```bash
git add src/app/mod.rs
git commit -m "test: add 10 end-to-end tests for vim basic operations"
```

---

### Task 14: 全量验证 + clippy

**Files:**
- None (verification only)

- [ ] **Step 1: 运行全量测试**

Run: `cargo test 2>&1`
Expected: all tests passed, 0 failed

- [ ] **Step 2: 运行 clippy**

Run: `cargo clippy --all-targets --all-features 2>&1`
Expected: 无新增 warning（仅 `SplitDirection` 的预存 dead_code warning）

- [ ] **Step 3: 运行 fmt 检查**

Run: `cargo fmt -- --check 2>&1`
Expected: 无输出（格式已正确）

- [ ] **Step 4: 如有格式问题，运行 fmt 修复**

Run: `cargo fmt`

- [ ] **Step 5: 最终提交（如有格式修复）**

```bash
git add -A
git commit -m "style: fmt"
```
