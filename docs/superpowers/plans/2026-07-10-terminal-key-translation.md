# Terminal Key Translation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 crossterm 原始按键到中立 `KeyEvent` 的翻译迁移到 `terminal`，使 `protocol` 不再依赖 crossterm，同时保持所有现有输入行为。

**Architecture:** `protocol::key_event` 只保留中立键盘模型。新建私有模块 `terminal::key_translate`，以 crate 内部可见函数接收 `crossterm::event::KeyEvent` 并返回协议事件；`terminal::input` 继续负责事件流、Release 过滤和 Resize 映射。

**Tech Stack:** Rust 2024，crossterm 0.29，tokio，futures，cargo test，cargo clippy。

## Global Constraints

- 保持依赖方向：`terminal -> protocol`，`protocol -> std`；`src/protocol` 不得引用 `crossterm`。
- 不改变 `KeyModifiers`、`ArrowKey`、`KeyCode`、`KeyEvent` 或 keymap、dispatcher 的公开语义。
- 翻译函数是总函数：不支持键映射为 `KeyCode::Unknown`，并保留 Ctrl、Alt、Shift 修饰键。
- 保持 ASCII、空格、Ctrl 字符小写归一、特殊键、方向键、Function 键、Press、Repeat、Release 和 Resize 的既有行为。
- 不新增 GUI、远程前端、通用翻译 trait、Unicode 或媒体键行为。
- Rust 代码修改后运行 `cargo test` 与 `cargo clippy --all-targets --all-features`。

---

## File Structure

- `src/protocol/key_event.rs`：仅定义中立按键类型、构造函数和查询函数；移除 crossterm 适配代码及其测试。
- `src/terminal/key_translate.rs`：新增的终端输入适配器，包含 crossterm 到协议按键的翻译和规则测试。
- `src/terminal/mod.rs`：私有声明 `key_translate` 模块。
- `src/terminal/input.rs`：继续处理事件流，改为调用终端适配器。

### Task 1: 建立并验证终端按键翻译器

**Files:**
- Create: `src/terminal/key_translate.rs`
- Modify: `src/terminal/mod.rs`
- Test: `src/terminal/key_translate.rs`

**Interfaces:**
- Produces: `pub(crate) fn translate_key(crossterm::event::KeyEvent) -> crate::protocol::key_event::KeyEvent`。
- Consumes: `crossterm::event::{KeyCode, KeyEvent, KeyModifiers}` 和协议层的 `ArrowKey`、`KeyCode`、`KeyEvent`、`KeyModifiers`。

- [ ] **Step 1: 写入会失败的终端翻译回归测试并声明模块**

  在 `src/terminal/mod.rs` 的现有公开模块前加入私有模块声明：

  ```rust
  mod key_translate;

  pub mod input;
  pub mod lifecycle;
  pub mod output;
  ```

  新建 `src/terminal/key_translate.rs`，先只写入导入和这个测试；此时
  `translate_key` 尚不存在：

  ```rust
  use crossterm::event::{
      KeyCode as CrosstermCode, KeyEvent as CrosstermKey, KeyModifiers as CrosstermModifiers,
  };

  use crate::protocol::key_event::{
      ArrowKey, KeyCode, KeyEvent, KeyModifiers,
  };

  #[cfg(test)]
  mod tests {
      use super::*;

      fn key(code: CrosstermCode, mods: CrosstermModifiers) -> CrosstermKey {
          CrosstermKey::new(code, mods)
      }

      #[test]
      fn ctrl_uppercase_char_is_normalized_and_keeps_modifier() {
          assert_eq!(
              super::translate_key(key(CrosstermCode::Char('S'), CrosstermModifiers::CONTROL)),
              KeyEvent::ctrl('s')
          );
      }
  }
  ```

- [ ] **Step 2: 运行新测试并确认它因缺少函数而失败**

  Run: `cargo test ctrl_uppercase_char_is_normalized_and_keeps_modifier`

  Expected: 编译失败，错误指出 `super::translate_key` 未定义。

- [ ] **Step 3: 实现等价翻译器，并迁移全部翻译规则测试**

  在 `src/terminal/key_translate.rs` 的协议导入之后加入实现：

  ```rust
  fn translate_modifiers(mods: CrosstermModifiers) -> KeyModifiers {
      KeyModifiers {
          ctrl: mods.contains(CrosstermModifiers::CONTROL),
          alt: mods.contains(CrosstermModifiers::ALT),
          shift: mods.contains(CrosstermModifiers::SHIFT),
      }
  }

  pub(crate) fn translate_key(k: CrosstermKey) -> KeyEvent {
      let modifiers = translate_modifiers(k.modifiers);
      match k.code {
          CrosstermCode::Char(c) if c.is_ascii_graphic() || c == ' ' => {
              let ch = if modifiers.ctrl {
                  c.to_ascii_lowercase()
              } else {
                  c
              };
              KeyEvent::modified(KeyCode::Char(ch), modifiers)
          }
          CrosstermCode::Backspace => KeyEvent::modified(KeyCode::Backspace, modifiers),
          CrosstermCode::Enter => KeyEvent::modified(KeyCode::Enter, modifiers),
          CrosstermCode::Esc => KeyEvent::modified(KeyCode::Escape, modifiers),
          CrosstermCode::Left => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Left), modifiers),
          CrosstermCode::Right => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Right), modifiers),
          CrosstermCode::Up => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Up), modifiers),
          CrosstermCode::Down => KeyEvent::modified(KeyCode::Arrow(ArrowKey::Down), modifiers),
          CrosstermCode::F(n) => KeyEvent::modified(KeyCode::Function(n), modifiers),
          _ => KeyEvent::modified(KeyCode::Unknown, modifiers),
      }
  }
  ```

  在已有的失败回归测试之后加入以下完整测试；它们是
  `src/protocol/key_event.rs` 当前翻译测试的等价迁移：

  ```rust
  #[test]
  fn printable_ascii_becomes_char() {
      assert_eq!(translate_key(key(CrosstermCode::Char('a'), CrosstermModifiers::empty())), KeyEvent::char('a'));
      assert_eq!(translate_key(key(CrosstermCode::Char(' '), CrosstermModifiers::empty())), KeyEvent::char(' '));
      assert_eq!(translate_key(key(CrosstermCode::Char('Z'), CrosstermModifiers::empty())), KeyEvent::char('Z'));
  }

  #[test]
  fn ctrl_ascii_chars_keep_ctrl_modifier() {
      assert_eq!(translate_key(key(CrosstermCode::Char('q'), CrosstermModifiers::CONTROL)), KeyEvent::ctrl('q'));
      assert_eq!(translate_key(key(CrosstermCode::Char('S'), CrosstermModifiers::CONTROL)), KeyEvent::ctrl('s'));
      assert_eq!(translate_key(key(CrosstermCode::Char('x'), CrosstermModifiers::CONTROL)), KeyEvent::ctrl('x'));
  }

  #[test]
  fn ctrl_arrow_and_function_keep_ctrl_modifier() {
      assert_eq!(translate_key(key(CrosstermCode::Left, CrosstermModifiers::CONTROL)), KeyEvent::modified(KeyCode::Arrow(ArrowKey::Left), KeyModifiers::ctrl()));
      assert_eq!(translate_key(key(CrosstermCode::F(1), CrosstermModifiers::CONTROL)), KeyEvent::modified(KeyCode::Function(1), KeyModifiers::ctrl()));
  }

  #[test]
  fn special_keys_map() {
      assert_eq!(translate_key(key(CrosstermCode::Backspace, CrosstermModifiers::empty())), KeyEvent::plain(KeyCode::Backspace));
      assert_eq!(translate_key(key(CrosstermCode::Enter, CrosstermModifiers::empty())), KeyEvent::plain(KeyCode::Enter));
      assert_eq!(translate_key(key(CrosstermCode::Esc, CrosstermModifiers::empty())), KeyEvent::plain(KeyCode::Escape));
  }

  #[test]
  fn arrows_map() {
      assert_eq!(translate_key(key(CrosstermCode::Up, CrosstermModifiers::empty())), KeyEvent::arrow(ArrowKey::Up));
      assert_eq!(translate_key(key(CrosstermCode::Down, CrosstermModifiers::empty())), KeyEvent::arrow(ArrowKey::Down));
      assert_eq!(translate_key(key(CrosstermCode::Left, CrosstermModifiers::empty())), KeyEvent::arrow(ArrowKey::Left));
      assert_eq!(translate_key(key(CrosstermCode::Right, CrosstermModifiers::empty())), KeyEvent::arrow(ArrowKey::Right));
  }

  #[test]
  fn function_key_keeps_function_code() {
      assert_eq!(translate_key(key(CrosstermCode::F(1), CrosstermModifiers::empty())), KeyEvent::modified(KeyCode::Function(1), KeyModifiers::none()));
  }

  #[test]
  fn shift_arrow_becomes_shift_variant() {
      assert_eq!(translate_key(key(CrosstermCode::Left, CrosstermModifiers::SHIFT)), KeyEvent::shift_arrow(ArrowKey::Left));
      assert_eq!(translate_key(key(CrosstermCode::Right, CrosstermModifiers::SHIFT)), KeyEvent::shift_arrow(ArrowKey::Right));
      assert_eq!(translate_key(key(CrosstermCode::Up, CrosstermModifiers::SHIFT)), KeyEvent::shift_arrow(ArrowKey::Up));
      assert_eq!(translate_key(key(CrosstermCode::Down, CrosstermModifiers::SHIFT)), KeyEvent::shift_arrow(ArrowKey::Down));
  }

  #[test]
  fn shift_char_and_enter_keep_shift_modifier() {
      assert_eq!(translate_key(key(CrosstermCode::Char('a'), CrosstermModifiers::SHIFT)), KeyEvent::modified(KeyCode::Char('a'), KeyModifiers::shift()));
      assert_eq!(translate_key(key(CrosstermCode::Enter, CrosstermModifiers::SHIFT)), KeyEvent::modified(KeyCode::Enter, KeyModifiers::shift()));
  }

  #[test]
  fn arrow_without_shift_unchanged() {
      assert_eq!(translate_key(key(CrosstermCode::Left, CrosstermModifiers::empty())), KeyEvent::arrow(ArrowKey::Left));
      assert_eq!(translate_key(key(CrosstermCode::Down, CrosstermModifiers::empty())), KeyEvent::arrow(ArrowKey::Down));
  }
  ```

- [ ] **Step 4: 运行终端翻译模块测试**

  Run: `cargo test terminal::key_translate`

  Expected: PASS。新模块必须覆盖 ASCII、Ctrl、Shift、方向键、Function、特殊键
  和不支持键的既有翻译规则。

- [ ] **Step 5: 提交翻译器创建检查点**

  ```powershell
  git add src/terminal/key_translate.rs src/terminal/mod.rs
  git commit -m "feat: add terminal key translator"
  ```

### Task 2: 切换输入层并清除 protocol 的终端依赖

**Files:**
- Modify: `src/terminal/input.rs`
- Modify: `src/protocol/key_event.rs`
- Test: `src/terminal/input.rs`
- Test: `src/terminal/key_translate.rs`

**Interfaces:**
- Consumes: `crate::terminal::key_translate::translate_key`。
- Produces: `protocol::key_event` 不含 crossterm 类型、导入、翻译函数或翻译测试。
- Preserves: `map_event(Event::Key)` 过滤 Release，翻译 Press/Repeat；Resize 映射为 `FrontendEvent::Resize`。

- [ ] **Step 1: 先让 Input 测试引用终端翻译器**

  在 `src/terminal/input.rs` 顶部，将：

  ```rust
  use crate::protocol::key_event::translate_key;
  ```

  替换为：

  ```rust
  use crate::terminal::key_translate::translate_key;
  ```

  不修改 `map_event` 的匹配结构或测试断言。`release_event_is_ignored`、
  `press_event_translates`、`repeat_event_translates`、`resize_event_translates`、
  `next_event_skips_filtered_events_until_mappable` 和
  `next_event_returns_none_only_on_stream_end` 都必须保留。

- [ ] **Step 2: 运行 Input 模块测试，确认事件流语义保持不变**

  Run: `cargo test terminal::input`

  Expected: PASS。Release 仍为 `None`；Press/Repeat 仍为
  `FrontendEvent::Key(KeyEvent::char('a'))`；Resize 值保持为 `80x24`。

- [ ] **Step 3: 从协议模块删除 crossterm 适配器**

  在 `src/protocol/key_event.rs`：

  1. 删除顶部 `use crossterm::event::{...};`。
  2. 删除 `fn translate_modifiers` 与 `pub fn translate_key` 的完整定义。
  3. 删除整个 `#[cfg(test)] mod tests`，因为其中的翻译测试已在
     `terminal::key_translate` 中执行。

  删除后，文件最后一个实现仍是：

  ```rust
  impl KeyEvent {
      // existing constructors and is_plain_char remain unchanged
  }
  ```

  不删除带有 `#[allow(dead_code)]` 的中立构造函数；它们是协议 API 的预留部分。

- [ ] **Step 4: 执行边界检查和完整验证**

  Run: `rg "crossterm" src/protocol`

  Expected: 无输出。

  Run: `cargo test`

  Expected: PASS。

  Run: `cargo clippy --all-targets --all-features`

  Expected: 退出码 0；记录但不在本任务中修复既有的无关 warning。

  Run: `git diff --check`

  Expected: 无输出。

- [ ] **Step 5: 提交协议边界清理检查点**

  ```powershell
  git add src/protocol/key_event.rs src/terminal/input.rs
  git commit -m "refactor: move key translation out of protocol"
  ```

## Verification Summary

- `terminal::key_translate` 独立证明 crossterm 到中立按键模型的映射保持逐值一致。
- `terminal::input` 独立证明 Release、Press、Repeat、Resize 和事件流结束的语义不变。
- `rg "crossterm" src/protocol` 证明 `protocol -> std` 边界恢复。
- 全量测试、Clippy 和空白检查验证跨层 API 与工作区质量。
