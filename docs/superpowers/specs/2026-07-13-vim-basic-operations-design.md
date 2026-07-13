# Vim 基础操作扩展设计

日期：2026-07-13

## 目标

在现有 Vim mode 的基础上，实现一批不涉及 operator-prefix、count、
register/undo 的基础 vim 操作，使编辑器的日常文本编辑能力达到可用水平。

## 现状

`VimMode` 已按 View 持有独立的 Normal/Insert runtime，通过 mode keymap +
typing fallback 解析按键。编辑操作由 `EditCommand` 经 `apply_edit` 作用于
Buffer 和 selections。`Mode::execute` 可返回 `Option<EditCommand>`，
使 mode action（如 `append`）能在切换模式的同时请求一次编辑。

已有基础设施：

- 3 类词模型（whitespace / word-char / other）由 `is_word_char` 和
  `backward_word_start` 建立，服务 `Ctrl+W`（`DeleteWordBackward`）。
- `MoveLeftBy`/`MoveRightBy` 等 motion 遵循非空 selection 收缩语义。
- `delete_word_backward_at_selections` 提供了内容依赖删除原语的范式。
- Buffer 的 `move_cursor_*` 方法维护 `char_index`/`row`/`col` 一致性。

## 范围

包含：

- Insert 模式：`Ctrl+U`、`Ctrl+K`、`Ctrl+J`、`Ctrl+M`。
- Normal 模式移动：`w`、`b`、`e`、`0`、`^`、`$`、`G`、`{`、`}`。
- Normal 模式编辑：`x`、`X`、`J`、`D`、`~`。
- Normal 模式模式切换：`o`、`O`、`I`、`A`、`s`、`C`、`S`。

不包含：

- undo/redo（`u`、`Ctrl+R`）——需要 undo 栈基础设施。
- `cc`、`r{char}`——`cc` 是双键；`r` 需要等待下一键输入。
- `dd`、`gg`——双键序列，需要 prefix 机制生产化。
- `yy`、`p`、`P`——需要 yank 寄存器基础设施。
- `H`、`M`、`L`——需要 viewport 可见行信息（在 tui 层）。
- `Ctrl+T`、`Ctrl+D`——需要 shiftwidth / 缩进配置。
- 修改 Dispatcher prefix 状态机、App 分发、协议按键模型或终端输入翻译。
- 改动现有 `i`、`Esc`、`a`、`h/j/k/l` 和 Arrow/Shift+Arrow 行为。

## 按键语义

### Insert 模式

| 按键 | 命令 | 语义 |
| --- | --- | --- |
| `Ctrl+U` | `DeleteToLineStart` | 删除从行首到光标（不含）的字符，光标落到行首。非空 selection 删 `[min,max)`。 |
| `Ctrl+K` | `DeleteToLineEnd` | 删除从光标到行尾（含行尾前内容，不含换行符）的字符。非空 selection 删 `[min,max)`。 |
| `Ctrl+J` | `InsertText("\n")` | 插入换行符，与 Enter 行为一致（vim 中 Ctrl+J 也是新行）。 |
| `Ctrl+M` | `InsertText("\n")` | 同 Ctrl+J，终端回车键的 Ctrl 修饰版本。 |

`Ctrl+U` 语义为删除到行首第 0 列（非"第一个非空白字符"）。行首时为空操作。
`Ctrl+K` 删除到行尾内容末尾（不含 `\n`），光标不动。行尾时为空操作。

### Normal 模式 — 移动

| 按键 | 命令 | 语义 |
| --- | --- | --- |
| `w` | `MoveWordForward` | 跳到下一个词首。跳过当前词尾和后续空白，落在下一个词/标点单元的首字符。 |
| `b` | `MoveWordBackward` | 跳到上一个词首。跳过当前词首和前导空白，落在上一个词/标点单元的首字符。 |
| `e` | `MoveWordEnd` | 跳到下一个词尾。若当前未到词尾则到当前词尾，否则跳过空白到下一个词尾。 |
| `0` | `MoveToLineStart` | 跳到当前行第 0 列。 |
| `^` | `MoveToFirstNonBlank` | 跳到当前行第一个非空白字符。全空白行停在第 0 列。 |
| `$` | `MoveToLineEnd` | 跳到当前行最后一个非换行字符上。空行（仅 `\n`）停在行首（第 0 列）。 |
| `G` | `MoveToLastLine` | 跳到文件最后一行行首。 |
| `{` | `MoveToPrevParagraph` | 跳到上一个空行（仅含 `\n` 或空字符串的行）的行首。文件首行之前无空行时停在文件首行。 |
| `}` | `MoveToNextParagraph` | 跳到下一个空行的行首。文件末行之后无空行时停在文件末行行首。 |

词移动的 3 类模型与 `backward_word_start` 一致：

- **whitespace**：`char::is_whitespace()`（含 `\n`）。
- **word char**：`is_word_char`（`char::is_alphanumeric() || '_'`）。
- **other**：非 whitespace 非 word char，每个连续同类 run 构成一个单元。

具体行为示例（`foo.bar baz`）：

- `w` 从 `f` → `.` → `b` → （文件末尾后）`baz` 尾后。
- `e` 从 `f` → `o`（`foo` 词尾）→ `.` → `z`（`baz` 词尾）。
- `b` 从 `z` → `b` → `.` → `f`。

`w`/`e` 在文件末尾后停在 `len_chars`（one-past-end）；`b` 在文件首停在 0。

移动命令对非空 selection 的处理与 `MoveLeftBy`/`MoveRightBy` 一致：
非空时收缩到 min/max（不额外移动），空时执行移动，始终 collapse。

### Normal 模式 — 编辑

| 按键 | 命令 | 语义 |
| --- | --- | --- |
| `x` | `Delete(1)` | 删光标处字符（向前删 1 个）。行尾时删最后一个字符。Normal 下 selection 总是 collapsed，等同删当前字符。 |
| `X` | `Delete(-1)` | 删光标前字符（向后删 1 个），等同 Backspace。复用现有 `Delete`。 |
| `J` | `JoinLines` | 当前行与下一行合并：删行尾 `\n`，若两行连接处需要则插入一个空格。末行时无操作。 |
| `D` | `DeleteToLineEnd` | 删光标到行尾（不含 `\n`）。与 Insert 的 `Ctrl+K` 删除逻辑相同，但不进入 Insert。 |
| `~` | `ToggleCase` | 翻转光标字符大小写，光标右移一位。行尾时不右移。 |

`x`/`X` 直接绑定现有 `Delete(1)`/`Delete(-1)`，不新增枚举变体。

`J`（JoinLines）的空格规则：如果下一行以空白开头，则删除换行符和下一行前导
空白后插入一个空格；如果下一行非空白开头，则删除换行符后插入一个空格。两行
都是空行时仅删除换行符。

`~`（ToggleCase）：对 `head.char_index` 处的字符调用 `to_uppercase`/
`to_lowercase` 翻转，在 rope 中替换该字符，然后 `head` 右移一位（行尾不移）。

### Normal 模式 — 模式切换

| 按键 | mode action | 返回 EditCommand | 语义 |
| --- | --- | --- | --- |
| `o` | `"open-below"` | `InsertNewLineBelow` | 当前行下方插入新行，光标移到新行行首，进入 Insert。 |
| `O` | `"open-above"` | `InsertNewLineAbove` | 当前行上方插入新行，光标移到新行行首，进入 Insert。 |
| `I` | `"insert-at-first-non-blank"` | `MoveToFirstNonBlank` | 光标移到当前行第一个非空白，进入 Insert。 |
| `A` | `"append-at-line-end"` | `MoveToLineEnd` | 光标移到行尾（最后一个字符之后，即 `\n` 前），进入 Insert。 |
| `s` | `"substitute-char"` | `DeleteCharForward` | 删当前字符，进入 Insert。 |
| `C` | `"change-to-line-end"` | `DeleteToLineEnd` | 删光标到行尾，进入 Insert。 |
| `S` | `"substitute-line"` | `DeleteLineContent` | 删当前行所有内容（保留换行符），光标到行首，进入 Insert。 |

这些操作通过 `Mode::execute` 返回 `Option<EditCommand>` 实现：mode action
先将 runtime 切换到 Insert，再返回对应的编辑命令，由 `Content::execute`
复用 `apply_edit` 执行。

新增 `EditCommand` 变体：

- `InsertNewLineBelow`：在当前行末（`\n` 前）插入 `\n`，光标到新行行首。
- `InsertNewLineAbove`：在当前行首插入 `\n`，光标不动（此时在新行行首）。
- `DeleteLineContent`：删除当前行 `[line_start, line_end_before_newline)` 的
  内容，光标到行首。若当前行为最后一行（无 `\n`），删除后行为空字符串，
  光标停在 `line_start`。

`MoveToFirstNonBlank`、`MoveToLineEnd` 既用于独立移动命令（`^`、`$`），
也用于 mode action 返回值。

`A` 的 `MoveToLineEnd` 语义与 Normal `$` 略有不同：`$` 停在最后一个字符
**上**，`A` 停在最后一个字符**之后**（插入点）。因此需要区分。方案：
`A` 使用新命令 `MoveToLineEndForInsert`，定位到 `\n` 前（即最后一个字符之后）。
或者更简单：`A` 返回 `MoveToLineEnd` 后再返回 `MoveRightBy(1)`——但
`execute` 只能返回一个 `EditCommand`。

**决策**：`A` 的 mode action `"append-at-line-end"` 返回新的
`EditCommand::MoveAfterLineEnd`，语义为移到行尾最后一个字符之后（`\n` 前）。
Normal `$` 仍用 `MoveToLineEnd`，停在最后一个字符上。

## 架构

### 新增 EditCommand 变体

```rust
pub enum EditCommand {
    // ... 现有变体 ...
    DeleteToLineStart,       // Ctrl+U
    DeleteToLineEnd,         // Ctrl+K, D
    MoveWordForward,         // w
    MoveWordBackward,        // b
    MoveWordEnd,             // e
    MoveToLineStart,         // 0
    MoveToFirstNonBlank,     // ^
    MoveToLineEnd,           // $ (停在最后一个字符上)
    MoveToLastLine,          // G
    MoveToPrevParagraph,     // {
    MoveToNextParagraph,     // }
    JoinLines,               // J
    ToggleCase,              // ~
    InsertNewLineBelow,      // o
    InsertNewLineAbove,      // O
    MoveAfterLineEnd,        // A 的移动部分
    DeleteLineContent,       // S
}
```

`x` 绑定为 `Delete(1)`，`X` 绑定为 `Delete(-1)`，`Ctrl+J`/`Ctrl+M` 绑定为
`InsertText("\n")`——均不新增变体。

`s` 绑定为 mode action `"substitute-char"` 返回 `Delete(1)`——也不新增变体。
但 `Delete(1)` 对非空 selection 删 `[min,max)`，对空 selection 向前删 1 个字符。
`s` 在 Normal 模式下 selection 总是 collapsed，所以语义正确。

`C` 绑定为 mode action `"change-to-line-end"` 返回 `DeleteToLineEnd`。

### apply_edit 分支

移动类命令（`MoveWordForward`/`MoveWordBackward`/`MoveWordEnd`/
`MoveToLineStart`/`MoveToFirstNonBlank`/`MoveToLineEnd`/`MoveToLastLine`/
`MoveToPrevParagraph`/`MoveToNextParagraph`/`MoveAfterLineEnd`）遵循
`MoveLeftBy`/`MoveRightBy` 的非空收缩语义：

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
```

删除/编辑类命令（`DeleteToLineStart`/`DeleteToLineEnd`/`JoinLines`/
`ToggleCase`/`InsertNewLineBelow`/`InsertNewLineAbove`/`DeleteLineContent`）
直接调用对应 Buffer 原语，与 `DeleteWordBackward` 同形。

### Buffer 原语

新增辅助函数（`buffer.rs` 模块级）：

- `forward_word_start(rope, char_index) -> usize`：`w` 的目标位置。
- `forward_word_end(rope, char_index) -> usize`：`e` 的目标位置。
- `first_non_blank_in_line(rope, row) -> usize`：行内第一个非空白字符的
  `char_index`。
- `line_end_char(rope, row) -> usize`：行内最后一个非换行字符的 `char_index`。
- `line_end_insert(rope, row) -> usize`：行尾插入点（最后一个字符之后，`\n` 前）。
- `prev_paragraph(rope, char_index) -> usize`：上一个空行的 `char_index`。
- `next_paragraph(rope, char_index) -> usize`：下一个空行的 `char_index`。

新增 Buffer 方法（selection 层，`pub`）：

- `move_head_word_forward(&self, sel)` / `move_head_word_backward(&self, sel)` /
  `move_head_word_end(&self, sel)`
- `move_head_to_line_start(&self, sel)` / `move_head_to_first_non_blank(&self, sel)` /
  `move_head_to_line_end(&self, sel)` / `move_head_after_line_end(&self, sel)` /
  `move_head_to_last_line(&self, sel)` / `move_head_to_prev_paragraph(&self, sel)` /
  `move_head_to_next_paragraph(&self, sel)`
- `delete_to_line_start_at_selections(&mut self, selections)`
- `delete_to_line_end_at_selections(&mut self, selections)`
- `join_lines_at_selections(&mut self, selections)`
- `toggle_case_at_selections(&mut self, selections)`
- `insert_new_line_below_at_selections(&mut self, selections)`
- `insert_new_line_above_at_selections(&mut self, selections)`
- `delete_line_content_at_selections(&mut self, selections)`

### Mode action 扩展

`VimMode::execute` 新增 action：

| action | 状态变更 | 返回 EditCommand |
| --- | --- | --- |
| `"open-below"` | →Insert | `InsertNewLineBelow` |
| `"open-above"` | →Insert | `InsertNewLineAbove` |
| `"insert-at-first-non-blank"` | →Insert | `MoveToFirstNonBlank` |
| `"append-at-line-end"` | →Insert | `MoveAfterLineEnd` |
| `"substitute-char"` | →Insert | `Delete(1)` |
| `"change-to-line-end"` | →Insert | `DeleteToLineEnd` |
| `"substitute-line"` | →Insert | `DeleteLineContent` |

### 键映射

`vim_insert_keymap` 新增：

- `Ctrl+U` → `DeleteToLineStart`
- `Ctrl+K` → `DeleteToLineEnd`
- `Ctrl+J` → `InsertText("\n")`
- `Ctrl+M` → `InsertText("\n")`

`vim_normal_keymap` 新增：

- `w` → `MoveWordForward`
- `b` → `MoveWordBackward`
- `e` → `MoveWordEnd`
- `0` → `MoveToLineStart`
- `^` → `MoveToFirstNonBlank`
- `$` → `MoveToLineEnd`
- `G` → `MoveToLastLine`
- `{` → `MoveToPrevParagraph`
- `}` → `MoveToNextParagraph`
- `x` → `Delete(1)`
- `X` → `Delete(-1)`
- `J` → `JoinLines`
- `D` → `DeleteToLineEnd`
- `~` → `ToggleCase`
- `o` → mode action `"open-below"`
- `O` → mode action `"open-above"`
- `I` → mode action `"insert-at-first-non-blank"`
- `A` → mode action `"append-at-line-end"`
- `s` → mode action `"substitute-char"`
- `C` → mode action `"change-to-line-end"`
- `S` → mode action `"substitute-line"`

## 修改范围

- `src/core/command.rs`：新增 `EditCommand` 变体。
- `src/core/buffer.rs`：新增辅助函数和 Buffer 原语方法。
- `src/core/edit.rs`：新增 `apply_edit` 分支。
- `src/core/mode.rs`：新增键映射绑定和 mode action。
- 上述模块的单元测试和集成测试。

## 测试与验收

### buffer.rs 单测

- `forward_word_start`：`foo.bar baz` 从各位置的预期目标。
- `forward_word_end`：`foo.bar baz` 从各位置的预期目标。
- `backward_word_start`：已有测试，确认仍通过。
- `delete_to_line_start`：多行文本中删除行首到光标。
- `delete_to_line_end`：多行文本中删除光标到行尾。
- `join_lines`：两行合并、含前导空白的行、末行无操作。
- `toggle_case`：大小写翻转 + 光标右移、行尾不移。
- `insert_new_line_below`/`above`：新行插入位置和光标位置。
- `delete_line_content`：清空行内容保留换行。
- `first_non_blank`/`line_end`/`prev_paragraph`/`next_paragraph`：边界情况。

### edit.rs 集成测试

- 每个新移动命令在 collapsed selection 上的行为。
- 每个新移动命令在非空 selection 上的收缩行为。
- 每个新删除/编辑命令的基本行为。

### mode.rs 测试

- Insert keymap 解析 `Ctrl+U`/`Ctrl+K`/`Ctrl+J`/`Ctrl+M` 到预期命令。
- Normal keymap 解析 `w`/`b`/`e`/`0`/`^`/`$`/`G`/`{`/`}` 到预期命令。
- Normal keymap 解析 `x`/`X`/`J`/`D`/`~` 到预期命令。
- 各 mode action 返回预期 `EditCommand` 并切换到 Insert。

### App 集成测试

- `o`/`O` 在行间插入新行并进入 Insert 后输入文本的端到端验证。
- `w`/`b`/`e` 在多词文本中的连续跳转。
- `0`/`$`/`^` 在含前导空白的行中的跳转。
- `x`/`~`/`J`/`D` 的端到端编辑验证。
- `I`/`A`/`s`/`C`/`S` 模式切换 + 后续输入的端到端验证。

实现完成时运行：

```text
cargo test
cargo clippy --all-targets --all-features
```
