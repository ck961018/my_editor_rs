# Content Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the content mode command model and make `Buffer` default to a
minimal Vim interaction mode.

**Architecture:** Keymaps bind `Command` instead of the current mixed
`Operation`. `Buffer` owns a `BufferModes` container with a boxed generic
`Mode` trait object; built-in `PlainEditMode` and `VimMode` both use that
same trait, and Vim keeps Normal/Insert state privately. Dispatcher resolves
relative `Command` values into `DispatchCommand` targets; App executes app,
content, and text commands through narrow paths.

**Tech Stack:** Rust 2024, existing `ropey` buffer model, existing
`crossterm` key protocol, existing tokio app loop, existing in-module Rust
unit tests.

## Global Constraints

- Preserve architecture direction from `AGENTS.md`: `core` must not depend on
  terminal, layout, async, frontend, or app modules.
- Keep `ContentHandler` object-safe and compatible with
  `HashMap<ContentId, Box<dyn ContentHandler>>`.
- `Mode` must be a trait object / instance model; do not implement concrete
  modes as a mode enum.
- `ModeLayer` is content-specific. First implementation only gives `Buffer`
  a `Base` layer; `StatusBar` gets no mode runtime.
- Default `Buffer::new()` uses `vim` in Normal state.
- Do not implement Vim count, operator, Visual, Command-line, `a`, `o`, `dd`,
  or `x`.
- Do not implement script loading, dynamic registry, language mode, readonly
  policy, auto-pair, or typing hooks.
- If Rust code changes, run `cargo test`; because this changes API/type
  boundaries, also run `cargo clippy --all-targets --all-features`.

---

## File Structure

- Create `src/core/command.rs`: owns `Command`, `AppCommand`,
  `ContentCommand`, and `TextCommand`.
- Create `src/core/mode.rs`: owns generic `Mode`, `ModeId`, and
  `ModeActionId`.
- Modify `src/core/mod.rs`: export `command` and `mode`; remove `operation`
  after all references are migrated.
- Modify `src/core/keymap.rs`: bind and return `Command` values.
- Modify `src/core/content.rs`: add object-safe `resolve_key` and
  `handle_mode_command` hooks.
- Modify `src/core/buffer.rs`: add `BufferModes`, `PlainEditMode`, `VimMode`,
  and route key resolution through modes.
- Modify `src/core/status_bar.rs`: adapt to `ContentHandler` additions with
  default behavior.
- Modify `src/app/dispatcher.rs`: replace `ResolvedOperation` with
  `DispatchCommand`, resolve `Command` targets, and keep prefix source only as
  dispatcher-internal state.
- Modify `src/app/executor.rs`: replace `execute(Operation, ...)` with
  `execute_text_command(TextCommand, Buffer, Selections)`.
- Modify `src/app/mod.rs`: execute `DispatchCommand` and route save, mode,
  app, and text commands separately.
- Modify tests in the touched modules. Keep test-only helper access narrow and
  local to existing `#[cfg(test)]` modules.

---

### Task 1: Add Command Model and Migrate Keymap

**Files:**
- Create: `src/core/command.rs`
- Modify: `src/core/mod.rs`
- Modify: `src/core/keymap.rs`

**Interfaces:**
- Produces:
  - `pub enum Command`
  - `pub enum AppCommand`
  - `pub enum ContentCommand`
  - `pub enum TextCommand`
  - `KeyBinding::Command(Command)`
  - `Keymap::bind(&mut self, key: KeyEvent, command: Command)`
- Consumes: existing `KeyEvent` and `Keymap` prefix tree structure.

- [ ] **Step 1: Add command model tests first**

Create `src/core/command.rs` with the command definitions and unit tests:

```rust
use crate::core::mode::{ModeActionId, ModeId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    App(AppCommand),
    Content(ContentCommand),
    Noop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppCommand {
    Quit,
    FocusNext,
    FocusPrev,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentCommand {
    Text(TextCommand),
    Save,
    Mode { mode: ModeId, action: ModeActionId },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextCommand {
    MoveBy { chars: isize, lines: isize },
    MoveLeftBy(usize),
    MoveRightBy(usize),
    MoveUpBy(usize),
    MoveDownBy(usize),
    MoveTo { char_idx: usize, line_idx: usize },
    ExtendLeftBy(usize),
    ExtendRightBy(usize),
    ExtendUpBy(usize),
    ExtendDownBy(usize),
    InsertText(String),
    Delete(isize),
    CollapseSelections,
}

impl From<TextCommand> for Command {
    fn from(command: TextCommand) -> Self {
        Command::Content(ContentCommand::Text(command))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_command_wraps_as_content_command() {
        let command: Command = TextCommand::MoveLeftBy(1).into();
        assert_eq!(
            command,
            Command::Content(ContentCommand::Text(TextCommand::MoveLeftBy(1)))
        );
    }

    #[test]
    fn mode_command_carries_mode_action_ids() {
        let command = Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("enter-insert"),
        });
        assert_eq!(
            command,
            Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("enter-insert"),
            })
        );
    }
}
```

Also create a temporary minimal `src/core/mode.rs` so this compiles:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeId(&'static str);

impl ModeId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeActionId(&'static str);

impl ModeActionId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}
```

- [ ] **Step 2: Export modules**

Modify `src/core/mod.rs`:

```rust
pub mod buffer;
pub mod command;
pub mod content;
pub mod keymap;
pub mod mode;
pub mod operation;
pub mod status_bar;
```

Keep `operation` exported for this task only; later tasks remove it.

- [ ] **Step 3: Verify command model tests**

Run: `cargo test core::command core::mode`

Expected: PASS for the two new `core::command` tests. Other modules should
still compile because existing `operation` remains exported.

- [ ] **Step 4: Migrate Keymap to Command**

Modify `src/core/keymap.rs` to use `Command`:

```rust
use std::collections::HashMap;

use crate::core::command::{Command, ContentCommand, TextCommand};
use crate::protocol::key_event::KeyEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyBinding {
    Command(Command),
    #[allow(dead_code)]
    Prefix(Keymap),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    bindings: HashMap<KeyEvent, KeyBinding>,
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, key: KeyEvent) -> Option<&KeyBinding> {
        self.bindings.get(&key)
    }

    pub fn bind(&mut self, key: KeyEvent, command: Command) {
        self.bindings.insert(key, KeyBinding::Command(command));
    }

    pub fn bind_text(&mut self, key: KeyEvent, command: TextCommand) {
        self.bind(
            key,
            Command::Content(ContentCommand::Text(command)),
        );
    }

    #[allow(dead_code)]
    pub fn bind_prefix(&mut self, key: KeyEvent, sub: Keymap) {
        self.bindings.insert(key, KeyBinding::Prefix(sub));
    }

    #[allow(dead_code)]
    pub fn unbind(&mut self, key: KeyEvent) {
        self.bindings.remove(&key);
    }
}
```

Update `src/core/keymap.rs` tests to assert `Command` values:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{Command, ContentCommand, TextCommand};
    use crate::protocol::key_event::{ArrowKey, KeyCode};

    #[test]
    fn bind_and_lookup_command() {
        let mut km = Keymap::new();
        km.bind_text(
            KeyEvent::plain(KeyCode::Enter),
            TextCommand::InsertText("\n".to_string()),
        );
        let binding = km.lookup(KeyEvent::plain(KeyCode::Enter)).unwrap();
        assert_eq!(
            binding,
            &KeyBinding::Command(Command::Content(ContentCommand::Text(
                TextCommand::InsertText("\n".to_string())
            )))
        );
    }

    #[test]
    fn lookup_missing_is_none() {
        let km = Keymap::new();
        assert!(km.lookup(KeyEvent::plain(KeyCode::Enter)).is_none());
    }

    #[test]
    fn unbind_removes() {
        let mut km = Keymap::new();
        km.bind_text(KeyEvent::plain(KeyCode::Backspace), TextCommand::Delete(-1));
        km.unbind(KeyEvent::plain(KeyCode::Backspace));
        assert!(km.lookup(KeyEvent::plain(KeyCode::Backspace)).is_none());
    }

    #[test]
    fn bind_prefix_nested() {
        let mut sub = Keymap::new();
        sub.bind(KeyEvent::char('s'), Command::Content(ContentCommand::Save));
        let mut km = Keymap::new();
        km.bind_prefix(KeyEvent::char('x'), sub);
        match km.lookup(KeyEvent::char('x')).unwrap() {
            KeyBinding::Prefix(sub_km) => {
                assert!(matches!(
                    sub_km.lookup(KeyEvent::char('s')),
                    Some(KeyBinding::Command(Command::Content(ContentCommand::Save)))
                ));
            }
            _ => panic!("expected Prefix"),
        }
    }

    #[test]
    fn keymap_clone_eq() {
        let mut km = Keymap::new();
        km.bind_text(KeyEvent::arrow(ArrowKey::Left), TextCommand::MoveLeftBy(1));
        let cloned = km.clone();
        assert_eq!(km, cloned);
    }
}
```

- [ ] **Step 5: Run focused tests and commit**

Run: `cargo test core::keymap core::command`

Expected: PASS for command and keymap tests. If other modules fail to compile
because `KeyBinding::Operation` no longer exists, immediately continue to Task
2 before committing. If the crate compiles at this checkpoint, commit:

```bash
git add src/core/command.rs src/core/mode.rs src/core/mod.rs src/core/keymap.rs
git commit -m "refactor(core): bind keymaps to commands"
```

---

### Task 2: Replace Operation with TextCommand in Buffer and Executor

**Files:**
- Modify: `src/core/buffer.rs`
- Modify: `src/app/executor.rs`
- Modify: `src/core/operation.rs`
- Modify: `src/core/mod.rs`

**Interfaces:**
- Consumes: `TextCommand` from Task 1.
- Produces:
  - `execute_text_command(command: TextCommand, buffer: &mut Buffer, selections: &mut Selections)`
  - no production dependency on `core::operation`.

- [ ] **Step 1: Write executor tests for TextCommand**

In `src/app/executor.rs`, change the tests to import `TextCommand` and call
`execute_text_command`. Use these exact replacements for the first tests:

```rust
use crate::core::command::TextCommand;

#[test]
fn insert_text_changes_buffer_and_selection() {
    let mut buf = Buffer::new();
    let mut selections = single_sel(CursorPos::origin());
    execute_text_command(
        TextCommand::InsertText("hi".to_string()),
        &mut buf,
        &mut selections,
    );
    assert_eq!(buf.slice().to_string(), "hi");
    assert_eq!(selections.primary().head().char_index, 2);
    assert_eq!(selections.primary().anchor, selections.primary().head());
}

#[test]
fn collapse_selections_collapses_and_retains_primary() {
    let mut buf = Buffer::new();
    buf.insert_char(0, 'a');
    let mut selections = non_empty_sel(0, 1, &buf);
    execute_text_command(TextCommand::CollapseSelections, &mut buf, &mut selections);
    assert_eq!(selections.primary().anchor, selections.primary().head());
    assert_eq!(selections.primary().head().char_index, 1);
    assert_eq!(selections.all().count(), 1);
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test app::executor`

Expected: FAIL because `execute_text_command` does not exist and executor
still imports `Operation`.

- [ ] **Step 3: Implement TextCommand executor**

Replace `src/app/executor.rs` top-level implementation with:

```rust
use crate::core::buffer::Buffer;
use crate::core::command::TextCommand;
use crate::protocol::selection::Selections;

pub fn execute_text_command(
    command: TextCommand,
    buffer: &mut Buffer,
    selections: &mut Selections,
) {
    match command {
        TextCommand::MoveLeftBy(n) => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_left(sel, n);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        TextCommand::MoveRightBy(n) => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_right(sel, n);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        TextCommand::MoveUpBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_up(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        TextCommand::MoveDownBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_down(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        TextCommand::MoveBy { chars, lines } => {
            for sel in selections.all_mut() {
                buffer.move_head_by(sel, chars, lines);
                Buffer::collapse_to_head(sel);
            }
        }
        TextCommand::ExtendLeftBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_left(sel, n);
            }
        }
        TextCommand::ExtendRightBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_right(sel, n);
            }
        }
        TextCommand::ExtendUpBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_up(sel, n);
            }
        }
        TextCommand::ExtendDownBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_down(sel, n);
            }
        }
        TextCommand::MoveTo { char_idx, line_idx } => {
            buffer.set_head(selections.primary_mut(), char_idx, line_idx);
            Buffer::collapse_to_head(selections.primary_mut());
            selections.retain_primary();
        }
        TextCommand::InsertText(text) => buffer.insert_at_selections(selections, &text),
        TextCommand::Delete(n) => buffer.delete_at_selections(selections, n),
        TextCommand::CollapseSelections => {
            for sel in selections.all_mut() {
                Buffer::collapse_to_head(sel);
            }
            selections.retain_primary();
        }
    }
}
```

Update every executor test call from `execute(Operation::...)` to
`execute_text_command(TextCommand::...)`.

- [ ] **Step 4: Replace buffer keymap commands**

In `src/core/buffer.rs`, replace `Operation` imports with command imports:

```rust
use crate::core::command::{Command, ContentCommand, TextCommand};
```

Replace `default_buffer_keymap()` body with `TextCommand` bindings:

```rust
fn default_buffer_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind_text(
        KeyEvent::plain(KeyCode::Enter),
        TextCommand::InsertText("\n".to_string()),
    );
    km.bind_text(KeyEvent::plain(KeyCode::Backspace), TextCommand::Delete(-1));
    km.bind_text(KeyEvent::arrow(ArrowKey::Left), TextCommand::MoveLeftBy(1));
    km.bind_text(KeyEvent::arrow(ArrowKey::Right), TextCommand::MoveRightBy(1));
    km.bind_text(KeyEvent::arrow(ArrowKey::Up), TextCommand::MoveUpBy(1));
    km.bind_text(KeyEvent::arrow(ArrowKey::Down), TextCommand::MoveDownBy(1));
    km.bind_text(
        KeyEvent::shift_arrow(ArrowKey::Left),
        TextCommand::ExtendLeftBy(1),
    );
    km.bind_text(
        KeyEvent::shift_arrow(ArrowKey::Right),
        TextCommand::ExtendRightBy(1),
    );
    km.bind_text(
        KeyEvent::shift_arrow(ArrowKey::Up),
        TextCommand::ExtendUpBy(1),
    );
    km.bind_text(
        KeyEvent::shift_arrow(ArrowKey::Down),
        TextCommand::ExtendDownBy(1),
    );
    km.bind_text(
        KeyEvent::plain(KeyCode::Escape),
        TextCommand::CollapseSelections,
    );
    km
}
```

Temporarily change `Buffer::default_binding` to return `Option<Command>` if
Task 3 has not yet introduced `resolve_key`; otherwise remove it in Task 3.

- [ ] **Step 5: Remove production operation module**

After all imports are migrated, delete `src/core/operation.rs` and remove
`pub mod operation;` from `src/core/mod.rs`. If deletion is too large for this
task because dispatcher still references it, leave the file until Task 4 and
write a note in the commit message: `operation retained only for dispatcher migration`.

- [ ] **Step 6: Run focused tests**

Run: `cargo test app::executor core::buffer core::keymap`

Expected: PASS, or compile failures only in dispatcher/app modules that are
explicitly migrated in Task 4.

- [ ] **Step 7: Commit**

If the crate compiles after this task, commit:

```bash
git add src/core src/app/executor.rs
git commit -m "refactor(core): move text editing to TextCommand"
```

If dispatcher/app compile failures remain because this task intentionally
removed `Operation`, do not commit yet; proceed directly to Task 4, then commit
Tasks 2-4 together.

---

### Task 3: Add Generic Mode Trait and Buffer Modes

**Files:**
- Modify: `src/core/mode.rs`
- Modify: `src/core/buffer.rs`
- Modify: `src/core/content.rs`

**Interfaces:**
- Consumes: `Command`, `ContentCommand`, `TextCommand`, `Keymap`.
- Produces:
  - `pub trait Mode`
  - `BufferModes`
  - `PlainEditMode`
  - `VimMode`
  - `ContentHandler::resolve_key`
  - `ContentHandler::handle_mode_command`

- [ ] **Step 1: Extend mode tests**

Add tests in `src/core/mode.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_ids_are_copyable_values() {
        let id = ModeId::new("vim");
        assert_eq!(id.as_str(), "vim");
        assert_eq!(id, ModeId::new("vim"));
    }

    #[test]
    fn mode_action_ids_are_copyable_values() {
        let action = ModeActionId::new("enter-insert");
        assert_eq!(action.as_str(), "enter-insert");
        assert_eq!(action, ModeActionId::new("enter-insert"));
    }
}
```

- [ ] **Step 2: Define generic Mode trait**

Extend `src/core/mode.rs`:

```rust
use crate::core::command::Command;
use crate::core::keymap::Keymap;
use crate::protocol::key_event::KeyEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeId(&'static str);

impl ModeId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModeActionId(&'static str);

impl ModeActionId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

pub trait Mode {
    fn id(&self) -> ModeId;
    fn label(&self) -> &str;
    fn keymap(&self) -> &Keymap;
    fn typing(&self, key: KeyEvent) -> Option<Command>;
    fn handle_mode_command(&mut self, action: ModeActionId);
}
```

- [ ] **Step 3: Add ContentHandler hooks**

Modify `src/core/content.rs` imports:

```rust
use crate::core::command::Command;
use crate::core::mode::{ModeActionId, ModeId};
```

Add object-safe default methods to `ContentHandler`:

```rust
fn resolve_key(&self, key: KeyEvent) -> Option<Command> {
    match self.keymap().lookup(key) {
        Some(crate::core::keymap::KeyBinding::Command(command)) => Some(command.clone()),
        Some(crate::core::keymap::KeyBinding::Prefix(_)) | None => None,
    }
}

fn handle_mode_command(&mut self, _mode: ModeId, _action: ModeActionId) {}
```

Keep `keymap()` and `keymap_mut()` for prefix tests and non-mode content.

- [ ] **Step 4: Implement BufferModes, PlainEditMode, VimMode**

In `src/core/buffer.rs`, add the following types near the `Buffer` struct:

```rust
use crate::core::mode::{Mode, ModeActionId, ModeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BufferModeLayer {
    Base,
}

struct BufferModes {
    base: Box<dyn Mode>,
}

impl BufferModes {
    fn vim() -> Self {
        Self {
            base: Box::new(VimMode::new()),
        }
    }

    #[cfg(test)]
    fn plain_edit() -> Self {
        Self {
            base: Box::new(PlainEditMode::new()),
        }
    }

    fn resolve_key(&self, key: KeyEvent) -> Option<Command> {
        match self.base.keymap().lookup(key) {
            Some(crate::core::keymap::KeyBinding::Command(command)) => Some(command.clone()),
            Some(crate::core::keymap::KeyBinding::Prefix(_)) | None => self.base.typing(key),
        }
    }

    fn handle_mode_command(&mut self, mode: ModeId, action: ModeActionId) {
        if self.base.id() == mode {
            self.base.handle_mode_command(action);
        }
    }
}
```

Add `PlainEditMode`:

```rust
struct PlainEditMode {
    keymap: Keymap,
}

impl PlainEditMode {
    fn new() -> Self {
        Self {
            keymap: plain_edit_keymap(),
        }
    }
}

impl Mode for PlainEditMode {
    fn id(&self) -> ModeId {
        ModeId::new("plain-edit")
    }

    fn label(&self) -> &str {
        "PLAIN"
    }

    fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    fn typing(&self, key: KeyEvent) -> Option<Command> {
        key.is_plain_char()
            .map(|ch| TextCommand::InsertText(ch.to_string()).into())
    }

    fn handle_mode_command(&mut self, _action: ModeActionId) {}
}
```

Add `VimMode`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VimState {
    Normal,
    Insert,
}

struct VimMode {
    state: VimState,
    normal_keymap: Keymap,
    insert_keymap: Keymap,
}

impl VimMode {
    fn new() -> Self {
        Self {
            state: VimState::Normal,
            normal_keymap: vim_normal_keymap(),
            insert_keymap: vim_insert_keymap(),
        }
    }
}

impl Mode for VimMode {
    fn id(&self) -> ModeId {
        ModeId::new("vim")
    }

    fn label(&self) -> &str {
        match self.state {
            VimState::Normal => "NORMAL",
            VimState::Insert => "INSERT",
        }
    }

    fn keymap(&self) -> &Keymap {
        match self.state {
            VimState::Normal => &self.normal_keymap,
            VimState::Insert => &self.insert_keymap,
        }
    }

    fn typing(&self, key: KeyEvent) -> Option<Command> {
        match self.state {
            VimState::Normal => None,
            VimState::Insert => key
                .is_plain_char()
                .map(|ch| TextCommand::InsertText(ch.to_string()).into()),
        }
    }

    fn handle_mode_command(&mut self, action: ModeActionId) {
        match action.as_str() {
            "enter-insert" => self.state = VimState::Insert,
            "enter-normal" => self.state = VimState::Normal,
            _ => {}
        }
    }
}
```

Add keymap builders:

```rust
fn plain_edit_keymap() -> Keymap {
    default_text_keymap(true)
}

fn vim_insert_keymap() -> Keymap {
    default_text_keymap(false)
}

fn default_text_keymap(bind_escape_to_collapse: bool) -> Keymap {
    let mut km = Keymap::new();
    km.bind_text(
        KeyEvent::plain(KeyCode::Enter),
        TextCommand::InsertText("\n".to_string()),
    );
    km.bind_text(KeyEvent::plain(KeyCode::Backspace), TextCommand::Delete(-1));
    km.bind_text(KeyEvent::arrow(ArrowKey::Left), TextCommand::MoveLeftBy(1));
    km.bind_text(KeyEvent::arrow(ArrowKey::Right), TextCommand::MoveRightBy(1));
    km.bind_text(KeyEvent::arrow(ArrowKey::Up), TextCommand::MoveUpBy(1));
    km.bind_text(KeyEvent::arrow(ArrowKey::Down), TextCommand::MoveDownBy(1));
    km.bind_text(KeyEvent::shift_arrow(ArrowKey::Left), TextCommand::ExtendLeftBy(1));
    km.bind_text(KeyEvent::shift_arrow(ArrowKey::Right), TextCommand::ExtendRightBy(1));
    km.bind_text(KeyEvent::shift_arrow(ArrowKey::Up), TextCommand::ExtendUpBy(1));
    km.bind_text(KeyEvent::shift_arrow(ArrowKey::Down), TextCommand::ExtendDownBy(1));
    if bind_escape_to_collapse {
        km.bind_text(
            KeyEvent::plain(KeyCode::Escape),
            TextCommand::CollapseSelections,
        );
    } else {
        km.bind(
            KeyEvent::plain(KeyCode::Escape),
            Command::Content(ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("enter-normal"),
            }),
        );
    }
    km
}

fn vim_normal_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind_text(KeyEvent::char('h'), TextCommand::MoveLeftBy(1));
    km.bind_text(KeyEvent::char('j'), TextCommand::MoveDownBy(1));
    km.bind_text(KeyEvent::char('k'), TextCommand::MoveUpBy(1));
    km.bind_text(KeyEvent::char('l'), TextCommand::MoveRightBy(1));
    km.bind(
        KeyEvent::char('i'),
        Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("enter-insert"),
        }),
    );
    km.bind(KeyEvent::plain(KeyCode::Escape), Command::Noop);
    km
}
```

Add field to `Buffer`:

```rust
modes: BufferModes,
```

Initialize in `Buffer::new()`:

```rust
modes: BufferModes::vim(),
keymap: Keymap::new(),
```

Keep `keymap` empty for compatibility with `ContentHandler::keymap`.

Override `ContentHandler` methods:

```rust
fn resolve_key(&self, key: KeyEvent) -> Option<Command> {
    self.modes.resolve_key(key)
}

fn handle_mode_command(&mut self, mode: ModeId, action: ModeActionId) {
    self.modes.handle_mode_command(mode, action);
}
```

- [ ] **Step 5: Add Buffer mode tests**

In `src/core/buffer.rs` tests, add:

```rust
#[test]
fn default_buffer_uses_vim_normal_and_plain_char_is_not_insert() {
    let b = Buffer::new();
    assert!(b.resolve_key(KeyEvent::char('a')).is_none());
}

#[test]
fn vim_i_enters_insert_and_plain_char_inserts() {
    let mut b = Buffer::new();
    assert_eq!(
        b.resolve_key(KeyEvent::char('i')),
        Some(Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("enter-insert"),
        }))
    );
    b.handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-insert"));
    assert_eq!(
        b.resolve_key(KeyEvent::char('a')),
        Some(Command::Content(ContentCommand::Text(TextCommand::InsertText(
            "a".to_string()
        ))))
    );
}

#[test]
fn vim_escape_returns_to_normal() {
    let mut b = Buffer::new();
    b.handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-insert"));
    assert_eq!(
        b.resolve_key(KeyEvent::plain(KeyCode::Escape)),
        Some(Command::Content(ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("enter-normal"),
        }))
    );
    b.handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-normal"));
    assert!(b.resolve_key(KeyEvent::char('a')).is_none());
}
```

- [ ] **Step 6: Run focused tests and commit**

Run: `cargo test core::mode core::buffer`

Expected: PASS for mode and buffer tests, or dispatcher/app compile failures
that are resolved in Task 4.

Commit if compiling:

```bash
git add src/core/mode.rs src/core/content.rs src/core/buffer.rs
git commit -m "feat(core): add buffer mode runtime"
```

---

### Task 4: Migrate Dispatcher to DispatchCommand

**Files:**
- Modify: `src/app/dispatcher.rs`

**Interfaces:**
- Consumes: `Command`, `AppCommand`, `ContentCommand`, `TextCommand`.
- Produces:
  - `pub(crate) enum DispatchCommand`
  - `Dispatcher::dispatch(...) -> Option<DispatchCommand>`

- [ ] **Step 1: Replace dispatcher result tests**

Update dispatcher tests to assert dispatch commands. Example replacements:

```rust
#[test]
fn global_quit_resolves_to_app_command() {
    let (mut dispatcher, scene, focused, contents) = fixture();

    let command = dispatcher
        .dispatch(KeyEvent::ctrl('q'), focused, &scene, &contents)
        .unwrap();

    assert_eq!(command, DispatchCommand::App(AppCommand::Quit));
}

#[test]
fn vim_normal_char_without_binding_returns_none() {
    let (mut dispatcher, scene, focused, contents) = fixture();

    assert!(
        dispatcher
            .dispatch(KeyEvent::char('a'), focused, &scene, &contents)
            .is_none()
    );
}

#[test]
fn vim_i_resolves_to_content_mode_command() {
    let (mut dispatcher, scene, focused, contents) = fixture();

    let command = dispatcher
        .dispatch(KeyEvent::char('i'), focused, &scene, &contents)
        .unwrap();

    assert_eq!(
        command,
        DispatchCommand::Content {
            command: ContentCommand::Mode {
                mode: ModeId::new("vim"),
                action: ModeActionId::new("enter-insert"),
            },
            content: ContentId(0),
        }
    );
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test app::dispatcher`

Expected: FAIL because dispatcher still returns `ResolvedOperation`.

- [ ] **Step 3: Implement DispatchCommand**

Rewrite dispatcher imports:

```rust
use crate::core::command::{AppCommand, Command, ContentCommand};
use crate::core::content::ContentLookup;
use crate::core::keymap::{KeyBinding, Keymap};
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::key_event::KeyEvent;
use crate::protocol::scene::Scene;
use crate::protocol::space::SpaceKind;
```

Define result types:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DispatchCommand {
    App(AppCommand),
    Content {
        command: ContentCommand,
        content: ContentId,
    },
    ViewContent {
        command: ContentCommand,
        space: SpaceId,
        content: ContentId,
    },
    Noop,
}
```

Keep `OperationSource` renamed to `CommandSource`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommandSource {
    sid: Option<SpaceId>,
    cid: Option<ContentId>,
}
```

Change lookup result:

```rust
enum LookupResult<'a> {
    Hit(Command),
    Prefix(&'a Keymap),
    Miss,
}

fn lookup_in<'a>(keymap: &'a Keymap, key: &KeyEvent) -> LookupResult<'a> {
    match keymap.lookup(*key) {
        Some(KeyBinding::Command(command)) => LookupResult::Hit(command.clone()),
        Some(KeyBinding::Prefix(sub)) => LookupResult::Prefix(sub),
        None => LookupResult::Miss,
    }
}
```

Replace fallback with content resolve:

```rust
let cid = focused_content_id(scene, focused)?;
let command = contents.get(cid)?.resolve_key(key)?;
resolve_command(
    command,
    CommandSource {
        sid: Some(focused),
        cid: Some(cid),
    },
    focused,
    scene,
    contents,
)
```

Implement target resolution:

```rust
fn resolve_command(
    command: Command,
    source: CommandSource,
    focused: SpaceId,
    scene: &Scene,
    contents: &dyn ContentLookup,
) -> Option<DispatchCommand> {
    match command {
        Command::App(command) => Some(DispatchCommand::App(command)),
        Command::Noop => Some(DispatchCommand::Noop),
        Command::Content(ContentCommand::Text(command)) => {
            let (space, content) = view_content_target(source, focused, scene, contents)?;
            Some(DispatchCommand::ViewContent {
                command: ContentCommand::Text(command),
                space,
                content,
            })
        }
        Command::Content(command @ ContentCommand::Save)
        | Command::Content(command @ ContentCommand::Mode { .. }) => {
            let content = source
                .cid
                .or_else(|| focused_content_id(scene, focused))?;
            contents.get(content)?;
            Some(DispatchCommand::Content { command, content })
        }
    }
}

fn view_content_target(
    source: CommandSource,
    focused: SpaceId,
    scene: &Scene,
    contents: &dyn ContentLookup,
) -> Option<(SpaceId, ContentId)> {
    let (space, content) = match (source.sid, source.cid) {
        (Some(space), Some(content)) => (space, content),
        _ => {
            let content = focused_content_id(scene, focused)?;
            (focused, content)
        }
    };
    contents.get(content)?;
    Some((space, content))
}
```

Global keymap:

```rust
pub fn default_global_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::ctrl('q'), Command::App(AppCommand::Quit));
    km.bind(KeyEvent::ctrl('s'), Command::Content(ContentCommand::Save));
    km
}
```

- [ ] **Step 4: Update prefix tests**

Use `Command::Content(ContentCommand::Save)` for prefix save tests:

```rust
let mut sub = Keymap::new();
sub.bind(KeyEvent::char('s'), Command::Content(ContentCommand::Save));
```

Expected prefix completion:

```rust
assert_eq!(
    command,
    DispatchCommand::Content {
        command: ContentCommand::Save,
        content: ContentId(0),
    }
);
```

- [ ] **Step 5: Run dispatcher tests**

Run: `cargo test app::dispatcher`

Expected: PASS for dispatcher tests.

- [ ] **Step 6: Commit**

```bash
git add src/app/dispatcher.rs
git commit -m "refactor(app): dispatch commands with resolved targets"
```

---

### Task 5: Execute DispatchCommand in App

**Files:**
- Modify: `src/app/mod.rs`

**Interfaces:**
- Consumes:
  - `DispatchCommand`
  - `ContentCommand`
  - `TextCommand`
  - `execute_text_command`
- Produces:
  - `App::execute_command(&mut self, DispatchCommand) -> io::Result<()>`

- [ ] **Step 1: Update app tests for default Vim**

Replace `run_inserts_char_then_quits` with:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn default_vim_requires_insert_before_text_input() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    let buf = app
        .contents
        .get_mut(&editor_cid())
        .and_then(|c| c.buffer_mut())
        .unwrap();
    assert_eq!(buf.slice().to_string(), "a");
    assert!(app.tasks.is_cancelled());
}
```

Update existing integration tests that type characters to enter insert mode
first and leave with Escape before Ctrl-Q. For example:

```rust
FrontendEvent::Key(KeyEvent::char('i')),
FrontendEvent::Key(KeyEvent::char('a')),
FrontendEvent::Key(KeyEvent::char('b')),
FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
FrontendEvent::Key(KeyEvent::ctrl('q')),
```

- [ ] **Step 2: Run app tests to verify failure**

Run: `cargo test app::`

Expected: FAIL because `App` still expects `ResolvedOperation`.

- [ ] **Step 3: Update App imports**

In `src/app/mod.rs`, replace dispatcher imports:

```rust
use crate::app::dispatcher::{DispatchCommand, Dispatcher, default_global_keymap};
```

Replace operation import with:

```rust
use crate::core::command::{AppCommand, ContentCommand};
```

- [ ] **Step 4: Replace execute_operation**

Replace `execute_operation` with:

```rust
fn execute_command(&mut self, command: DispatchCommand) -> io::Result<()> {
    match command {
        DispatchCommand::App(command) => match command {
            AppCommand::Quit => self.tasks.cancel(),
            AppCommand::FocusNext | AppCommand::FocusPrev => {}
        },
        DispatchCommand::Content { command, content } => match command {
            ContentCommand::Save => {
                self.spawn_save(content);
            }
            ContentCommand::Mode { mode, action } => {
                if let Some(content) = self.contents.get_mut(&content) {
                    content.handle_mode_command(mode, action);
                }
            }
            ContentCommand::Text(_) => {}
        },
        DispatchCommand::ViewContent {
            command,
            space,
            content,
        } => {
            if let ContentCommand::Text(command) = command {
                let content = self
                    .contents
                    .get_mut(&content)
                    .and_then(|c| c.buffer_mut())
                    .expect("text command target is a buffer");
                let view = self.views.get_mut(&space).expect("target view exists");
                executor::execute_text_command(command, content, view.selections_mut());
            }
        }
        DispatchCommand::Noop => {}
    }
    Ok(())
}
```

Change event handling call:

```rust
if let Some(command) = self
    .dispatcher
    .dispatch(k, self.focused, &self.scene, &self.contents)
{
    self.execute_command(command)?;
}
```

- [ ] **Step 5: Update direct execution tests**

Tests that construct `ResolvedOperation` must construct `DispatchCommand`.
For text:

```rust
app.execute_command(DispatchCommand::ViewContent {
    command: ContentCommand::Text(TextCommand::InsertText("Z".to_string())),
    space: other_sid,
    content: other_cid,
})
.unwrap();
```

For save:

```rust
app.execute_command(DispatchCommand::Content {
    command: ContentCommand::Save,
    content: other_cid,
})
.unwrap();
```

- [ ] **Step 6: Run app tests**

Run: `cargo test app::`

Expected: PASS for app tests after updating event scripts for default Vim.

- [ ] **Step 7: Commit**

```bash
git add src/app/mod.rs
git commit -m "refactor(app): execute resolved commands"
```

---

### Task 6: Clean Up Old Operation References and Complete Vim Coverage

**Files:**
- Delete: `src/core/operation.rs`
- Modify: `src/core/mod.rs`
- Modify: all files returned by `rg "Operation|ResolvedOperation|OperationTarget|OperationSource"`

**Interfaces:**
- Consumes: final `Command` / `DispatchCommand` model.
- Produces: no remaining production or test references to old operation names.

- [ ] **Step 1: Search old names**

Run:

```powershell
rg -n "Operation|ResolvedOperation|OperationTarget|OperationSource|default_binding" src
```

Expected before cleanup: remaining references only in files not yet migrated.

- [ ] **Step 2: Remove operation module**

Delete `src/core/operation.rs`.

Modify `src/core/mod.rs` to:

```rust
pub mod buffer;
pub mod command;
pub mod content;
pub mod keymap;
pub mod mode;
pub mod status_bar;
```

- [ ] **Step 3: Remove Buffer default_binding**

In `src/core/content.rs`, remove `default_binding` from `ContentHandler`.

In `src/core/buffer.rs`, remove the `default_binding` implementation. Plain
text input now lives only in `Mode::typing`.

- [ ] **Step 4: Add integration coverage for minimal Vim movement**

Add in `src/app/mod.rs` tests:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_h_moves_left_after_insert() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('h')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    let buf = app
        .contents
        .get_mut(&editor_cid())
        .and_then(|c| c.buffer_mut())
        .unwrap();
    assert_eq!(buf.slice().to_string(), "ab");
    let head = app
        .views
        .get(&app.focused)
        .unwrap()
        .selections()
        .primary()
        .head();
    assert_eq!(head.char_index, 1);
}
```

- [ ] **Step 5: Run full tests**

Run: `cargo test`

Expected: PASS. If tests fail because scripts type without `i`, update the
test script to enter Vim insert mode explicitly unless the test constructs a
plain-edit buffer.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets --all-features`

Expected: PASS with no warnings introduced by command/mode changes.

- [ ] **Step 7: Commit**

```bash
git add src
git commit -m "feat(core): default buffers to minimal vim mode"
```

---

### Task 7: Final Hygiene and Documentation Alignment

**Files:**
- Modify: `docs/superpowers/specs/2026-07-09-content-mode-design.md` only if
  implementation reveals a required wording correction.

**Interfaces:**
- Consumes: completed implementation.
- Produces: clean final verification state.

- [ ] **Step 1: Check diff hygiene**

Run:

```powershell
git status --short
git diff --check
```

Expected: `git diff --check` prints no errors. `git status --short` shows only
intentional implementation files if final commits were not created during
tasks.

- [ ] **Step 2: Check final source search**

Run:

```powershell
rg -n "Operation|ResolvedOperation|OperationTarget|OperationSource|default_binding" src
```

Expected: no matches, unless a test comment intentionally mentions the old
term. Remove stale comments instead of keeping compatibility wording.

- [ ] **Step 3: Run final verification**

Run:

```powershell
cargo test
cargo clippy --all-targets --all-features
```

Expected: both commands PASS.

- [ ] **Step 4: Final commit if needed**

If Task 6 left any uncommitted cleanup, commit:

```bash
git add src docs/superpowers/specs/2026-07-09-content-mode-design.md
git commit -m "chore: align content mode cleanup"
```

If there are no changes, do not create an empty commit.

---

## Self-Review

- Spec coverage: The plan covers `Mode` trait, content-specific layer
  ownership, default Vim, command model, keymap migration, dispatcher target
  resolution, App execution, old `Operation` cleanup, and verification.
- Scope: The plan does not add script loading, full registry, status bar mode
  display, language/policy modes, or full Vim behaviors excluded by the spec.
- Type consistency: `Command`, `AppCommand`, `ContentCommand`, `TextCommand`,
  `DispatchCommand`, `ModeId`, and `ModeActionId` names are stable across all
  tasks. `Operation` is only a migration source and is removed by Task 6.
- Test strategy: Each behavioral change has focused tests before
  implementation and finishes with `cargo test`; API boundary work finishes
  with clippy.
