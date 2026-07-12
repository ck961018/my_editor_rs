# Vim Keymap Expansion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Emacs-style Insert bindings and Vim Normal `a` without expanding Vim operators or leaking Vim-specific behavior outside the mode layer.

**Architecture:** `Mode::execute` returns an optional generic `EditCommand`, allowing the Vim `append` action to switch runtime to Insert and request existing selection-aware right movement. `DeleteWordBackward` is a new generic edit command whose word-boundary calculation and deletion remain in Buffer; Content composes a mode effect with `apply_edit` using the target View's selections.

**Tech Stack:** Rust 2024, `ropey`, existing static `Content`/`ContentStore`, `ModeSet`, `Keymap`, and `tokio` app tests.

## Global Constraints

- Keep `App<F: Frontend>` statically dispatched; do not add trait objects for frontends.
- Keep `Content` a static closed enum and preserve `Content::execute(ContentInput)` as its only execution entry point.
- Keep Dispatcher prefix handling, App target resolution, `ContentRuntime`, `protocol::KeyEvent`, and terminal key translation unchanged.
- `a` is the only new Normal binding; leave `c` unbound and do not introduce operator or mode-prefix state.
- Word characters are `char::is_alphanumeric()` or `_`; other non-whitespace characters are one-character units; whitespace is skipped before deleting one unit.
- Retain the existing selection model: edit commands operate on every selection, then collapse each affected selection.
- Run `cargo test` and `cargo clippy --all-targets --all-features` before completion.

## File Structure

- `src/core/mode.rs`: Vim state transitions, mode-local keymaps, and optional mode edit effect.
- `src/core/command.rs`: closed set of generic editing commands.
- `src/core/buffer.rs`: text-aware backward-word deletion over selections.
- `src/core/edit.rs`: dispatches generic edit commands to Buffer primitives.
- `src/core/content.rs`: composes an optional mode effect with the focused View selections.
- `src/app/mod.rs`: existing `ScriptedFrontend` integration tests demonstrate dispatched default-Vim behavior.

---

### Task 1: Return an optional edit effect from Vim mode actions

**Files:**
- Modify: `src/core/mode.rs:39-77, 141-220, tests`

**Interfaces:**
- Consumes: `EditCommand`, `ModeState`, `ModeActionId`, `KeyEvent`, and `Keymap`.
- Produces: `Mode::execute(&mut dyn ModeState, ModeActionId) -> Option<EditCommand>` and `ModeSet::execute(...) -> Option<EditCommand>`.
- Produces: Normal `a` resolves to Vim action `append`; Insert Ctrl bindings resolve to generic editing commands.

- [ ] **Step 1: Write failing mode tests for the new bindings and append effect**

  Add the following tests to `src/core/mode.rs`'s existing `tests` module. Keep the first
  runtime in Insert after the `enter-insert` action, and create a fresh runtime for the
  Normal-only assertion.

  ```rust
  #[test]
  fn vim_insert_resolves_emacs_motion_and_delete_keys() {
      let modes = ModeSet::vim();
      let mut runtime = modes.create_runtime();
      assert_eq!(
          modes.execute(
              &mut runtime,
              ModeId::new("vim"),
              ModeActionId::new("enter-insert"),
          ),
          None,
      );

      assert_eq!(
          modes.resolve_key(&runtime, KeyEvent::ctrl('b')),
          Some(EditCommand::MoveLeftBy(1).into()),
      );
      assert_eq!(
          modes.resolve_key(&runtime, KeyEvent::ctrl('f')),
          Some(EditCommand::MoveRightBy(1).into()),
      );
      assert_eq!(
          modes.resolve_key(&runtime, KeyEvent::ctrl('h')),
          Some(EditCommand::Delete(-1).into()),
      );
  }

  #[test]
  fn vim_append_enters_insert_and_returns_right_move() {
      let modes = ModeSet::vim();
      let mut runtime = modes.create_runtime();

      assert_eq!(
          modes.execute(
              &mut runtime,
              ModeId::new("vim"),
              ModeActionId::new("append"),
          ),
          Some(EditCommand::MoveRightBy(1)),
      );
      assert_eq!(
          modes.resolve_key(&runtime, KeyEvent::char('x')),
          Some(EditCommand::InsertText("x".to_string()).into()),
      );
  }

  #[test]
  fn vim_normal_c_remains_unbound() {
      let modes = ModeSet::vim();
      let runtime = modes.create_runtime();

      assert_eq!(modes.resolve_key(&runtime, KeyEvent::char('c')), None);
  }
  ```

- [ ] **Step 2: Run the new tests to verify they fail**

  Run: `cargo test core::mode::tests::vim_insert_resolves_emacs_motion_and_delete_keys`

  Expected: compilation failure because `ModeSet::execute` returns `()`.

- [ ] **Step 3: Change the Mode execution contract and add key bindings**

  In `src/core/mode.rs`, change the trait and forwarding method to return an optional
  generic edit command. A nonmatching mode must return `None`.

  ```rust
  pub trait Mode {
      fn id(&self) -> ModeId;
      fn new_state(&self) -> Box<dyn ModeState>;
      fn keymap(&self, state: &dyn ModeState) -> &Keymap;
      fn typing(&self, state: &dyn ModeState, key: KeyEvent) -> Option<Command>;
      fn execute(
          &self,
          state: &mut dyn ModeState,
          action: ModeActionId,
      ) -> Option<EditCommand>;
  }

  pub(crate) fn execute(
      &self,
      runtime: &mut ModeRuntime,
      mode: ModeId,
      action: ModeActionId,
  ) -> Option<EditCommand> {
      (self.base.id() == mode)
          .then(|| self.base.execute(runtime.base.as_mut(), action))
          .flatten()
  }
  ```

  Make `PlainEditMode::execute` return `None`. Make `VimMode::execute` use this exact
  action dispatch:

  ```rust
  match action.as_str() {
      "enter-insert" => {
          self.state_mut(state).state = VimState::Insert;
          None
      }
      "enter-normal" => {
          self.state_mut(state).state = VimState::Normal;
          None
      }
      "append" => {
          self.state_mut(state).state = VimState::Insert;
          Some(EditCommand::MoveRightBy(1))
      }
      _ => None,
  }
  ```

  In `vim_insert_keymap`, after the existing Backspace binding, add:

  ```rust
  km.bind_edit(KeyEvent::ctrl('b'), EditCommand::MoveLeftBy(1));
  km.bind_edit(KeyEvent::ctrl('f'), EditCommand::MoveRightBy(1));
  km.bind_edit(KeyEvent::ctrl('h'), EditCommand::Delete(-1));
  ```

  In `vim_normal_keymap`, bind `a` to the existing mode-command shape:

  ```rust
  km.bind(
      KeyEvent::char('a'),
      Command::Content(ContentCommand::Mode {
          mode: ModeId::new("vim"),
          action: ModeActionId::new("append"),
      }),
  );
  ```

  Update existing tests that call `modes.execute(...)` so they assert or discard its
  `Option<EditCommand>` explicitly. Do not bind `c`.

- [ ] **Step 4: Run the focused mode tests**

  Run: `cargo test core::mode::tests`

  Expected: PASS.

- [ ] **Step 5: Commit the mode behavior**

  ```text
  git add src/core/mode.rs
  git commit -m "feat: add vim append and insert control keys"
  ```

### Task 2: Add selection-aware backward-word deletion and bind Ctrl+W

**Files:**
- Modify: `src/core/mode.rs:141-169, tests`
- Modify: `src/core/command.rs:24-48, tests`
- Modify: `src/core/edit.rs:7-70, tests`
- Modify: `src/core/buffer.rs:298-357, tests`

**Interfaces:**
- Consumes: `EditCommand`, `Buffer`, and `Selections`.
- Produces: `EditCommand::DeleteWordBackward` and
  `Buffer::delete_word_backward_at_selections(&mut Selections)`.
- Produces: `apply_edit(EditCommand::DeleteWordBackward, buffer, selections)`.
- Produces: Insert `Ctrl+W` resolves to `EditCommand::DeleteWordBackward`.

- [ ] **Step 1: Write failing Buffer and edit tests**

  Add a test-only helper beside `single_sel` in `src/core/buffer.rs` tests to place a
  collapsed cursor at a character index:

  ```rust
  fn selection_at(buffer: &Buffer, char_index: usize) -> Selections {
      let mut cursor = CursorPos::origin();
      cursor.char_index = char_index;
      buffer.recompute_cursor(&mut cursor);
      Selections::single(Selection::collapsed(cursor))
  }
  ```

  Add these Buffer tests. They lock down Unicode word characters, punctuation, whitespace
  and newline traversal, and selection replacement semantics.

  ```rust
  #[test]
  fn delete_word_backward_removes_unicode_word() {
      let mut buffer = Buffer::new();
      buffer.insert_char(0, 'c');
      buffer.insert_char(1, 'a');
      buffer.insert_char(2, 'f');
      buffer.insert_char(3, 'é');
      buffer.insert_char(4, '_');
      buffer.insert_char(5, '4');
      buffer.insert_char(6, '2');
      let mut selections = selection_at(&buffer, 7);

      buffer.delete_word_backward_at_selections(&mut selections);

      assert_eq!(buffer.slice().to_string(), "");
      assert_eq!(selections.primary().head().char_index, 0);
  }

  #[test]
  fn delete_word_backward_removes_one_punctuation_unit() {
      let mut buffer = Buffer::new();
      for (index, ch) in "alpha!!".chars().enumerate() {
          buffer.insert_char(index, ch);
      }
      let mut selections = selection_at(&buffer, 7);

      buffer.delete_word_backward_at_selections(&mut selections);

      assert_eq!(buffer.slice().to_string(), "alpha!");
      assert_eq!(selections.primary().head().char_index, 6);
  }

  #[test]
  fn delete_word_backward_skips_whitespace_and_crosses_newline() {
      let mut buffer = Buffer::new();
      for (index, ch) in "alpha \n beta".chars().enumerate() {
          buffer.insert_char(index, ch);
      }
      let mut selections = selection_at(&buffer, 8);

      buffer.delete_word_backward_at_selections(&mut selections);

      assert_eq!(buffer.slice().to_string(), "beta");
      assert_eq!(selections.primary().head().char_index, 0);
  }

  #[test]
  fn delete_word_backward_deletes_non_empty_selection() {
      let mut buffer = Buffer::new();
      for (index, ch) in "alpha beta".chars().enumerate() {
          buffer.insert_char(index, ch);
      }
      let mut selections = selection_at(&buffer, 6);
      selections.primary_mut().head = selection_at(&buffer, 10).primary().head;

      buffer.delete_word_backward_at_selections(&mut selections);

      assert_eq!(buffer.slice().to_string(), "alpha ");
      assert_eq!(selections.primary().head().char_index, 6);
      assert_eq!(selections.primary().anchor, selections.primary().head());
  }

  #[test]
  fn delete_word_backward_deletes_backward_selection() {
      let mut buffer = Buffer::new();
      for (index, ch) in "alpha beta".chars().enumerate() {
          buffer.insert_char(index, ch);
      }
      let mut selections = selection_at(&buffer, 10);
      selections.primary_mut().head = selection_at(&buffer, 6).primary().head;

      buffer.delete_word_backward_at_selections(&mut selections);

      assert_eq!(buffer.slice().to_string(), "alpha ");
      assert_eq!(selections.primary().head().char_index, 6);
      assert_eq!(selections.primary().anchor, selections.primary().head());
  }
  ```

  The whitespace/newline test intentionally places the cursor immediately after the
  leading `"alpha \n "` run (character index 8), so `Ctrl+W` skips that run, removes
  `alpha`, and leaves `beta` at index zero.

  Add this `src/core/edit.rs` test to verify the command reaches the Buffer primitive:

  ```rust
  #[test]
  fn delete_word_backward_dispatches_to_buffer() {
      let mut buffer = Buffer::new();
      buffer.insert_at_selections(
          &mut single_sel(CursorPos::origin()),
          "alpha beta",
      );
      let mut selections = single_sel({
          let mut cursor = CursorPos::origin();
          cursor.char_index = 10;
          buffer.recompute_cursor(&mut cursor);
          cursor
      });

      apply_edit(EditCommand::DeleteWordBackward, &mut buffer, &mut selections);

      assert_eq!(buffer.slice().to_string(), "alpha ");
      assert_eq!(selections.primary().head().char_index, 6);
  }
  ```

  Add this mode test to `src/core/mode.rs`'s existing test module:

  ```rust
  #[test]
  fn vim_insert_ctrl_w_resolves_to_delete_word_backward() {
      let modes = ModeSet::vim();
      let mut runtime = modes.create_runtime();
      assert_eq!(
          modes.execute(
              &mut runtime,
              ModeId::new("vim"),
              ModeActionId::new("enter-insert"),
          ),
          None,
      );

      assert_eq!(
          modes.resolve_key(&runtime, KeyEvent::ctrl('w')),
          Some(EditCommand::DeleteWordBackward.into()),
      );
  }
  ```

- [ ] **Step 2: Run the focused tests to verify they fail**

  Run: `cargo test delete_word_backward`

  Expected: compilation failure because `DeleteWordBackward` and
  `delete_word_backward_at_selections` do not exist.

- [ ] **Step 3: Define the command and Buffer primitive**

  Add `DeleteWordBackward` immediately after `Delete(isize)` in `EditCommand`.

  In `src/core/edit.rs`, add one match arm:

  ```rust
  EditCommand::DeleteWordBackward => buffer.delete_word_backward_at_selections(selections),
  ```

  In `src/core/mode.rs`, add the static Insert binding after the existing Ctrl+H binding:

  ```rust
  km.bind_edit(KeyEvent::ctrl('w'), EditCommand::DeleteWordBackward);
  ```

  In `src/core/buffer.rs`, add the public selection primitive beside
  `delete_at_selections`. Use the existing deletion ordering and collapse behavior; do
  not refactor unrelated movement or insertion code.

  ```rust
  pub fn delete_word_backward_at_selections(&mut self, selections: &mut Selections) {
      let starts: Vec<usize> = selections
          .all()
          .map(|selection| {
              if selection.anchor != selection.head {
                  selection.anchor.char_index.min(selection.head.char_index)
              } else {
                  backward_word_start(&self.rope, selection.head.char_index)
              }
          })
          .collect();
      let mut ranges: Vec<(usize, usize)> = selections
          .all()
          .zip(starts.iter().copied())
          .map(|(selection, start)| {
              let end = selection.anchor.char_index.max(selection.head.char_index);
              (start, end)
          })
          .collect();

      ranges.sort_unstable_by_key(|range| std::cmp::Reverse(range.0));
      ranges.dedup();
      for (start, end) in ranges {
          if end > start {
              self.rope.remove(start..end);
          }
      }
      self.modified = true;
      for (selection, start) in selections.all_mut().zip(starts) {
          selection.head.char_index = start;
          self.recompute_cursor(&mut selection.head);
          Self::collapse_to_head(selection);
      }
  }

  fn backward_word_start(rope: &Rope, char_index: usize) -> usize {
      let mut start = char_index.min(rope.len_chars());
      while start > 0 && rope.char(start - 1).is_whitespace() {
          start -= 1;
      }
      if start == 0 {
          return 0;
      }
      if is_word_char(rope.char(start - 1)) {
          while start > 0 && is_word_char(rope.char(start - 1)) {
              start -= 1;
          }
      } else {
          start -= 1;
      }
      start
  }

  fn is_word_char(ch: char) -> bool {
      ch.is_alphanumeric() || ch == '_'
  }
  ```

  Keep both helpers private module functions near `line_content_len`; only the Buffer
  selection primitive is public. The range mapper must take the maximum of anchor and
  head, so backward selections delete the same interval as forward selections.

- [ ] **Step 4: Run focused tests and the core suite**

  Run: `cargo test delete_word_backward`

  Expected: PASS.

  Run: `cargo test core::`

  Expected: PASS.

- [ ] **Step 5: Commit the generic backward-word command**

  ```text
  git add src/core/mode.rs src/core/command.rs src/core/edit.rs src/core/buffer.rs
  git commit -m "feat: add backward word deletion"
  ```

### Task 3: Apply mode edit effects through Content and prove default-Vim flows

**Files:**
- Modify: `src/core/buffer.rs:54-63`
- Modify: `src/core/content.rs:80-94, tests`
- Modify: `src/app/mod.rs:680-723, tests`

**Interfaces:**
- Consumes: `Buffer::execute_mode(...) -> Option<EditCommand>`, `apply_edit`, and
  `ContentInput::View { selections, runtime }`.
- Produces: a `ContentCommand::Mode` can update runtime and apply an optional generic edit
  command to the exact target View.
- Produces: end-to-end assertions for default Vim `a` and `Ctrl+W` behavior.

- [ ] **Step 1: Write failing Content and App integration tests**

  Add this test to the `src/core/content.rs` tests module. It explicitly verifies the
  nonempty-selection side of `a`: the selection contracts to its right boundary before
  later Insert typing.

  ```rust
  #[test]
  fn vim_append_collapses_selection_to_right_then_enters_insert() {
      let mut content = Content::Buffer(Buffer::new());
      let mut selections = Selections::single(Selection::collapsed(CursorPos::origin()));
      let mut runtime = content.create_runtime();
      content.execute(ContentInput::View {
          command: ContentCommand::Mode {
              mode: ModeId::new("vim"),
              action: ModeActionId::new("enter-insert"),
          },
          selections: &mut selections,
          runtime: &mut runtime,
      });
      content.execute(ContentInput::View {
          command: ContentCommand::Edit(EditCommand::InsertText("abc".to_string())),
          selections: &mut selections,
          runtime: &mut runtime,
      });
      content.execute(ContentInput::View {
          command: ContentCommand::Mode {
              mode: ModeId::new("vim"),
              action: ModeActionId::new("enter-normal"),
          },
          selections: &mut selections,
          runtime: &mut runtime,
      });
      selections.primary_mut().anchor.char_index = 1;
      selections.primary_mut().head.char_index = 3;

      content.execute(ContentInput::View {
          command: ContentCommand::Mode {
              mode: ModeId::new("vim"),
              action: ModeActionId::new("append"),
          },
          selections: &mut selections,
          runtime: &mut runtime,
      });

      assert_eq!(selections.primary().head().char_index, 3);
      assert_eq!(selections.primary().anchor, selections.primary().head());
      assert_eq!(
          content.resolve_key(&runtime, KeyEvent::char('x')),
          Some(EditCommand::InsertText("x".to_string()).into()),
      );
  }
  ```

  Add two `#[tokio::test(flavor = "multi_thread")]` tests to `src/app/mod.rs` near
  `default_vim_requires_insert_before_text_input`. Reuse `make_app`, `text_rows`, and
  `editor_cid` from the surrounding test module.

  ```rust
  #[tokio::test(flavor = "multi_thread")]
  async fn default_vim_a_appends_after_cursor_and_enters_insert() {
      let mut app = make_app(
          vec![
              FrontendEvent::Key(KeyEvent::char('i')),
              FrontendEvent::Key(KeyEvent::char('a')),
              FrontendEvent::Key(KeyEvent::char('b')),
              FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
              FrontendEvent::Key(KeyEvent::char('h')),
              FrontendEvent::Key(KeyEvent::char('a')),
              FrontendEvent::Key(KeyEvent::char('x')),
              FrontendEvent::Key(KeyEvent::ctrl('q')),
          ],
          None,
      );

      app.run().await.unwrap();

      assert_eq!(text_rows(&app, editor_cid()), vec!["abx"]);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn default_vim_ctrl_w_deletes_previous_word() {
      let mut app = make_app(
          vec![
              FrontendEvent::Key(KeyEvent::char('i')),
              FrontendEvent::Key(KeyEvent::char('a')),
              FrontendEvent::Key(KeyEvent::char('b')),
              FrontendEvent::Key(KeyEvent::char(' ')),
              FrontendEvent::Key(KeyEvent::char('c')),
              FrontendEvent::Key(KeyEvent::char('d')),
              FrontendEvent::Key(KeyEvent::ctrl('w')),
              FrontendEvent::Key(KeyEvent::ctrl('q')),
          ],
          None,
      );

      app.run().await.unwrap();

      assert_eq!(text_rows(&app, editor_cid()), vec!["ab "]);
  }
  ```

- [ ] **Step 2: Run the new tests to verify they fail**

  Run: `cargo test vim_append_collapses_selection_to_right_then_enters_insert`

  Expected: FAIL because `Content::execute` ignores the `Some(MoveRightBy(1))` result
  from the mode action.

  Run: `cargo test default_vim_a_appends_after_cursor_and_enters_insert`

  Expected: FAIL because appending does not yet move the collapsed cursor right.

- [ ] **Step 3: Compose the optional effect inside Content execution**

  Change the Buffer command import to `use crate::core::command::{Command, EditCommand};`
  and change `Buffer::execute_mode` to return the result from `self.modes.execute`:

  ```rust
  pub(crate) fn execute_mode(
      &self,
      runtime: &mut BufferRuntime,
      mode: ModeId,
      action: ModeActionId,
  ) -> Option<EditCommand> {
      self.modes.execute(runtime.modes_mut(), mode, action)
  }
  ```

  Import `EditCommand` remains unnecessary in `src/core/content.rs`; infer the local
  `edit` value. Replace the Buffer/Mode match arm with this version, which preserves the
  content/runtime mismatch invariants and uses the same borrowed View selections:

  ```rust
  (
      Self::Buffer(buffer),
      ContentInput::View {
          command: ContentCommand::Mode { mode, action },
          selections,
          runtime: ContentRuntime::Buffer(runtime),
      },
  ) => {
      if let Some(edit) = buffer.execute_mode(runtime, mode, action) {
          apply_edit(edit, buffer, selections);
      }
      ContentEffect::None
  }
  ```

  Do not change `ContentInput`, `DispatchCommand`, Dispatcher, App execution routing, or
  any terminal translator code.

- [ ] **Step 4: Run the focused integration tests and full validation**

  Run: `cargo test vim_append_collapses_selection_to_right_then_enters_insert`

  Expected: PASS.

  Run: `cargo test default_vim_a_appends_after_cursor_and_enters_insert`

  Expected: PASS.

  Run: `cargo test default_vim_ctrl_w_deletes_previous_word`

  Expected: PASS.

  Run: `cargo test`

  Expected: PASS.

  Run: `cargo clippy --all-targets --all-features`

  Expected: PASS with only pre-existing allowed dead-code warnings, if any.

- [ ] **Step 5: Commit Content integration and regression tests**

  ```text
  git add src/core/buffer.rs src/core/content.rs src/app/mod.rs
  git commit -m "feat: apply vim mode edit effects"
  ```
