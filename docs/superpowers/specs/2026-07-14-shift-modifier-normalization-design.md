# Shift 修饰键规范化设计

日期：2026-07-14

## 目标

修复 normal 模式下所有需要 Shift 的按键（O/A/I/G/J/D/X/C/S/~/$/^/{/}
等）无响应的 bug。根因是终端按键翻译层保留了 ASCII 可见字符的 SHIFT
修饰键，导致与 keymap 绑定用的无修饰键 KeyEvent 不匹配。

## 背景

`terminal::key_translate::translate_key` 把 crossterm 原始按键转换为
protocol `KeyEvent`。对 `Char` 分支，当 ctrl 按下时已有小写规范化，但
shift 修饰键原样保留。

crossterm 在用户按 Shift+字母时报告 `Char('A')` + SHIFT。翻译后得到
`KeyEvent { code: Char('A'), modifiers: { shift: true } }`。但
`vim_normal_keymap` 绑定用的是 `KeyEvent::char('A')`，即
`KeyEvent { code: Char('A'), modifiers: none() }`。`Keymap` 内部是
`HashMap<KeyEvent, _>`，修饰键不同导致查找失败。

同理，US 键盘上的 shifted 符号（`~`/`$`/`^`/`{`/`}`）也带 SHIFT
修饰键，与 keymap 绑定的无修饰键 `char('~')` 等不匹配。

现有测试 `shift_char_and_enter_keep_shift_modifier` 锁定了这个（错误的）
行为，需要修改。

## 范围

包含：

- 修改 `translate_key` 的 `Char` 分支：对 ASCII 可见字符剥离 shift 修饰键。
- 修改现有测试 `shift_char_and_enter_keep_shift_modifier` 以反映新行为。
- 新增测试覆盖字母、符号和 Ctrl+Shift 组合的 shift 剥离。

不包含：

- 修改 `KeyEvent` 数据模型或 `Keymap` 实现。
- 修改 keymap 绑定或 mode 定义。
- 引入 Shift+letter 作为独立绑定的能力（vim 不区分）。
- 非 ASCII 字符的 shift 处理（当前 `translate_key` 只匹配
  `is_ascii_graphic() || ' '`）。
- 非 Char 键码（Enter、Backspace、方向键等）的 shift 处理。这些键走独立
  分支，shift 修饰键保留不变。keymap 未绑定 Shift+Enter 等，不影响使用。

## 规范化规则

在 `translate_key` 的 `CrosstermCode::Char(character)` 分支中，当
`character` 满足 `is_ascii_graphic() || character == ' '` 时：

1. 若 `ctrl` 为 true：字符转小写（已有逻辑不变）。
2. **新增**：剥离 `shift` 修饰键。

shift 的语义已编码在字符本身中——大写字母（`'A'`）和 shifted 符号
（`'~'`）的字符值已经反映了 shift 的结果。保留 shift 修饰键是冗余的，
且导致与 keymap 绑定的不匹配。

此规则与已有的 ctrl 规范化对称：ctrl 组合键的字符也做了小写规范化，
使得 `Ctrl+S` 和 `Ctrl+Shift+S` 都解析为 `KeyEvent::ctrl('s')`。

## 不改动的部分

- `protocol::key_event`：`KeyEvent`、`KeyModifiers` 类型和构造器不变。
- `core::keymap`：`Keymap` 仍是纯 `HashMap`，不做 shift-insensitive 匹配。
- `core::mode`：keymap 绑定仍用 `KeyEvent::char('O')` 等无修饰键构造。
- `KeyEvent::is_plain_char()`：已检查 `modifiers == none()`，剥离 shift 后
  大写字母能通过此检查，vim Insert 模式的 typing 能正确处理大写输入。

## 测试

### 修改现有测试

`shift_char_and_enter_keep_shift_modifier` 当前同时测试 Shift+'a'（Char
分支）和 Shift+Enter（Enter 分支）。拆分为：

- `shift_char_strips_shift_modifier`：Shift+'a' →
  `KeyEvent::char('A')`（无 shift）。只覆盖 Char 分支的新行为。
- `shift_enter_keeps_shift_modifier`：Shift+Enter →
  `KeyEvent::modified(KeyCode::Enter, KeyModifiers::shift())`。Enter 不在
  Char 分支，shift 保留不变。此行为不在本次修改范围内。

### 新增测试

- `shift_uppercase_letter_strips_shift_modifier`：Shift+'A' →
  `KeyEvent::char('A')`（无 shift）。
- `shift_symbol_strips_shift_modifier`：Shift+'1' →
  `KeyEvent::char('!')`（无 shift）；Shift+'`' →
  `KeyEvent::char('~')`（无 shift）。
- `ctrl_shift_letter_keeps_ctrl_strips_shift`：Ctrl+Shift+S →
  `KeyEvent::ctrl('s')`（有 ctrl，无 shift）。

## 验收

```text
cargo test terminal::key_translate
cargo test
cargo clippy --all-targets --all-features
```

全部必须成功。
