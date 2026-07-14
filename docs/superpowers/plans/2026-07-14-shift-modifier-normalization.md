# Shift 修饰键规范化实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 normal 模式下所有需要 Shift 的按键无响应的 bug，在 `translate_key` 的 Char 分支剥离 ASCII 可见字符的 shift 修饰键。

**Architecture:** 只改 `terminal::key_translate.rs` 一个文件。在 `CrosstermCode::Char` 分支中，构造 `KeyEvent` 前剥离 `modifiers.shift`，与已有的 ctrl 小写规范化对称。测试侧拆分现有测试 + 新增 3 个测试。

**Tech Stack:** Rust 2024, crossterm

## Global Constraints

- MSRV 1.85，Rust 2024 edition
- 修改后运行 `cargo test` 和 `cargo clippy --all-targets --all-features` 必须全部成功
- 遵循现有代码风格：无注释除非解释非明显约束
- TDD：先写失败测试，再改实现

---

### Task 1: 拆分现有测试并新增 shift 剥离测试

**Files:**
- Modify: `src/terminal/key_translate.rs:182-192`（现有测试 `shift_char_and_enter_keep_shift_modifier`）
- Test: `src/terminal/key_translate.rs`（同文件的 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: `translate_key` 现有签名 `pub(crate) fn translate_key(key: CrosstermKey) -> KeyEvent`
- Produces: 无新接口，仅测试变更

**说明：** 先写好所有目标测试（期望新行为），此时它们会失败。Task 2 再改实现使它们通过。现有测试 `shift_char_and_enter_keep_shift_modifier` 同时覆盖 Char 分支和 Enter 分支，按 spec 拆分为两个独立测试，并新增 3 个测试。

- [ ] **Step 1: 替换现有测试为拆分后的版本**

将 `src/terminal/key_translate.rs` 中的：

```rust
    #[test]
    fn shift_char_and_enter_keep_shift_modifier() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('a'), CrosstermModifiers::SHIFT)),
            KeyEvent::modified(KeyCode::Char('a'), KeyModifiers::shift())
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Enter, CrosstermModifiers::SHIFT)),
            KeyEvent::modified(KeyCode::Enter, KeyModifiers::shift())
        );
    }
```

替换为：

```rust
    #[test]
    fn shift_char_strips_shift_modifier() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('a'), CrosstermModifiers::SHIFT)),
            KeyEvent::char('a')
        );
    }

    #[test]
    fn shift_enter_keeps_shift_modifier() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Enter, CrosstermModifiers::SHIFT)),
            KeyEvent::modified(KeyCode::Enter, KeyModifiers::shift())
        );
    }

    #[test]
    fn shift_uppercase_letter_strips_shift_modifier() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('A'), CrosstermModifiers::SHIFT)),
            KeyEvent::char('A')
        );
    }

    #[test]
    fn shift_symbol_strips_shift_modifier() {
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('1'), CrosstermModifiers::SHIFT)),
            KeyEvent::char('!')
        );
        assert_eq!(
            super::translate_key(key(CrosstermCode::Char('`'), CrosstermModifiers::SHIFT)),
            KeyEvent::char('~')
        );
    }

    #[test]
    fn ctrl_shift_letter_keeps_ctrl_strips_shift() {
        assert_eq!(
            super::translate_key(key(
                CrosstermCode::Char('S'),
                CrosstermModifiers::CONTROL | CrosstermModifiers::SHIFT,
            )),
            KeyEvent::ctrl('s')
        );
    }
```

- [ ] **Step 2: 运行测试验证它们失败**

Run: `cargo test terminal::key_translate`
Expected: 5 个测试中 4 个 FAIL（`shift_char_strips_shift_modifier`、`shift_uppercase_letter_strips_shift_modifier`、`shift_symbol_strips_shift_modifier`、`ctrl_shift_letter_keeps_ctrl_strips_shift`）；`shift_enter_keeps_shift_modifier` 通过（Enter 不走 Char 分支）。失败原因是 `translate_key` 仍保留 shift 修饰键。

- [ ] **Step 3: Commit**

```bash
git add src/terminal/key_translate.rs
git commit -m "test: split shift modifier tests and add new shift-stripping cases"
```

---

### Task 2: 实现 shift 修饰键剥离

**Files:**
- Modify: `src/terminal/key_translate.rs:15-38`（`translate_key` 函数）

**Interfaces:**
- Consumes: 无
- Produces: `translate_key` 对 ASCII 可见字符剥离 shift 修饰键的新行为

- [ ] **Step 1: 修改 `translate_key` 的 Char 分支**

将 `src/terminal/key_translate.rs` 中的：

```rust
pub(crate) fn translate_key(key: CrosstermKey) -> KeyEvent {
    let modifiers = translate_modifiers(key.modifiers);
    let code = match key.code {
        CrosstermCode::Char(character) if character.is_ascii_graphic() || character == ' ' => {
            let character = if modifiers.ctrl {
                character.to_ascii_lowercase()
            } else {
                character
            };
            KeyCode::Char(character)
        }
```

替换为：

```rust
pub(crate) fn translate_key(key: CrosstermKey) -> KeyEvent {
    let mut modifiers = translate_modifiers(key.modifiers);
    let code = match key.code {
        CrosstermCode::Char(character) if character.is_ascii_graphic() || character == ' ' => {
            let character = if modifiers.ctrl {
                character.to_ascii_lowercase()
            } else {
                character
            };
            // shift 的语义已编码在字符本身（大写字母或 shifted 符号），
            // 剥离 shift 使其与 keymap 中无修饰键的 KeyEvent 绑定匹配。
            modifiers.shift = false;
            KeyCode::Char(character)
        }
```

关键变更点：
1. `let modifiers` → `let mut modifiers`（需要可变借用）
2. 在 `KeyCode::Char(character)` 之前加 `modifiers.shift = false;`

- [ ] **Step 2: 运行 key_translate 测试验证全部通过**

Run: `cargo test terminal::key_translate`
Expected: 所有测试 PASS

- [ ] **Step 3: 运行全量测试套件**

Run: `cargo test`
Expected: 所有测试 PASS，0 failures

- [ ] **Step 4: 运行 clippy**

Run: `cargo clippy --all-targets --all-features`
Expected: 无 error，无新增 warning

- [ ] **Step 5: Commit**

```bash
git add src/terminal/key_translate.rs
git commit -m "fix: strip shift modifier for ASCII graphic chars in translate_key

crossterm reports Shift+letter as Char('A') + SHIFT, but keymap binds
KeyEvent::char('A') with no modifiers. The mismatch caused all
Shift-required keys (O/A/I/G/J/D/X/C/S/~/$/^/{/} etc.) to be
unresponsive in normal mode. Strip shift for ASCII graphic chars,
symmetric with existing ctrl lowercase normalization."
```
