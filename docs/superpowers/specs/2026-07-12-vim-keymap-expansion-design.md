# Vim 按键扩展设计

日期：2026-07-12

## 目标

完善默认 Vim mode 的一组基础按键：

- Insert 模式支持 Emacs 风格的 `Ctrl+B`、`Ctrl+F`、`Ctrl+H`、`Ctrl+W`。
- Normal 模式支持 `a`（append 后进入 Insert）。

本次不实现 Normal 模式的 `c`，也不引入 Vim operator、count、Visual、
命令行或 mode prefix 机制。

## 现状与问题

`VimMode` 已按 View 持有独立的 Normal/Insert runtime，并且通过 mode keymap
与 typing fallback 解析按键。普通编辑操作由 `EditCommand` 经由
`Content::execute(ContentInput::View)` 作用于 Buffer 和 View 的 selections。

现有 `Mode::execute` 只能修改 mode runtime。因此 `a` 无法在同一个 mode
action 中既进入 Insert，又按 selection 语义移动到追加位置。另一方面，
`Ctrl+W` 的删除距离依赖文本内容，不能表示成现有的 `Delete(isize)`。

## 范围

包含：

- Insert 模式的 `Ctrl+B`、`Ctrl+F`、`Ctrl+H`、`Ctrl+W`。
- Normal 模式的 `a`。
- 支撑 `Ctrl+W` 的通用编辑命令与 Buffer 删除原语。
- 对 mode action 返回编辑效果的最小扩展。

不包含：

- Normal 模式的 `c` 及其任何组合。
- 修改 Dispatcher 的 prefix 状态机、App 分发、`ContentRuntime`、协议按键模型或
  终端输入翻译。
- 改动现有 `i`、`Esc`、`h/j/k/l` 和 Arrow/Shift+Arrow 行为。
- 更复杂的词法分类、可配置 word boundary，或 Vim 兼容的 keyword 选项。

## 按键语义

### Insert

| 按键 | 命令 | 语义 |
| --- | --- | --- |
| `Ctrl+B` | `MoveLeftBy(1)` | 与 Left 相同；非空 selection 收缩到左端。 |
| `Ctrl+F` | `MoveRightBy(1)` | 与 Right 相同；非空 selection 收缩到右端。 |
| `Ctrl+H` | `Delete(-1)` | 与 Backspace 相同。 |
| `Ctrl+W` | `DeleteWordBackward` | 按下述规则向左删除一个词。 |

部分终端会将 `Ctrl+H` 传为普通 Backspace。现有 Backspace 绑定已经覆盖该
情形；Insert keymap 仍应显式绑定 `KeyEvent::ctrl('h')`，以兼容能够保留 Ctrl
修饰符的终端。

### DeleteWordBackward

对每个 selection 独立计算删除区间：

1. 非空 selection：删除 `[min(anchor, head), max(anchor, head))`，与普通
   `Delete` 一致。
2. 空 selection：从 `head` 向左先跳过连续 whitespace，再删除一个连续单元。
3. 单元是连续的 `char::is_alphanumeric()` 字符或 `_`；任何其他非 whitespace
   字符单独构成一个单元。
4. 换行符属于 whitespace。位于行首时，操作可删除换行符并继续处理前一行，
   因而能够连接两行。

删除沿用现有多 selection 约束：先生成区间，按起点降序实际删除，再将每个
selection 收缩到删除起点。分类以 Rust 标准库的 Unicode 字符分类为准，不新增
依赖或独立词法器。

### Normal 的 `a`

`a` 绑定为 `ContentCommand::Mode { mode: "vim", action: "append" }`。

- 对 collapsed selection：向右移动一个字符，若已在文本末尾则保持不动。
- 对非空 selection：复用 `MoveRightBy(1)` 的既有规则，收缩至右端而不额外右移。
- 然后进入 Insert 模式。

`c` 保持未绑定；按下后不进入 pending 状态、不修改文本、也不改变 mode。

## 架构

### Mode action 效果

采用 mode action 返回轻量编辑效果的路线：

```rust
trait Mode {
    fn execute(
        &self,
        state: &mut dyn ModeState,
        action: ModeActionId,
    ) -> Option<EditCommand>;
}
```

`ModeSet::execute` 与 `Buffer::execute_mode` 透传该返回值。`Content::execute`
处理 `ContentCommand::Mode` 时，先让 mode 更新 runtime；若返回
`Some(edit)`，则复用 `apply_edit(edit, buffer, selections)`。

`VimMode` 的 `enter-insert` 与 `enter-normal` 返回 `None`；`append` 将状态改为
Insert 并返回 `Some(EditCommand::MoveRightBy(1))`。

这样 mode 只接触自己的不透明 runtime 与通用 `EditCommand`，不接触 `Buffer` 或
`Selections`；Content 也不需要识别 Vim 的具体 action 名称。相比在
`Content::execute` 特判 `append`，该边界可让其他 content 的 mode 复用同一机制；
相比将 Buffer 和 selections 交给 Mode，又避免 mode 依赖具体内容模型。

### 编辑原语

`EditCommand` 新增 `DeleteWordBackward`。`apply_edit` 将该命令转交给 Buffer 的
专用删除原语。Buffer 持有文本，因此是计算 word boundary 和执行多区间删除的
唯一位置；keymap 只负责静态绑定，Dispatcher 和 App 不感知该语义。

## 修改范围

- `src/core/mode.rs`：Mode action 的返回值、Vim Insert 绑定和 Normal `a`。
- `src/core/command.rs`：新增 `EditCommand::DeleteWordBackward`。
- `src/core/edit.rs`：分发该编辑命令。
- `src/core/buffer.rs`：计算并删除每个 selection 的向后一个词。
- `src/core/content.rs`：执行 mode action 的可选编辑效果。
- 上述模块及现有 App 集成测试中的回归测试。

## 测试与验收

至少覆盖：

- Insert mode 将四个 Ctrl 按键解析到预期 `EditCommand`。
- `DeleteWordBackward` 删除 Unicode 字母/数字/`_` 组成的词，标点单独删除，跳过
  空白并跨换行；非空 selection 直接删除选区。
- `a` 在 collapsed selection 上右移后进入 Insert，在非空 selection 上收缩到右端
  后进入 Insert。
- 默认 Vim 的端到端按键序列验证 `a` 后输入和 `Ctrl+W` 的结果。
- `c` 在 Normal 中仍无法解析为命令。

实现完成时运行：

```text
cargo test
cargo clippy --all-targets --all-features
```

