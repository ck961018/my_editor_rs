# Event Loop Task Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade `App<F: Frontend>` from a save-specific background channel
to a cancellable master loop with internal messages and tracked task classes.

**Architecture:** Keep the existing static frontend dispatch and independent
`frontend` layer. Add focused `app::message` and `app::tasks` modules, replace
`BgResult`/`should_quit` with `AppMessage` plus `AppTasks`, and run frontend
events, internal messages, and cancellation through one `tokio::select!` loop.

**Tech Stack:** Rust 2024, Tokio, `tokio-util` `CancellationToken`,
`tokio_util::task::TaskTracker`, existing local `ScriptedFrontend` tests.

---

## File Structure

- Modify `Cargo.toml`
  Add `tokio-util = { version = "0.7", features = ["full"] }`.

- Modify `Cargo.lock`
  Let Cargo update it when running tests after the dependency change.

- Create `src/app/message.rs`
  Own the internal message enum:
  `AppMessage::SaveCompleted { content, result }`.

- Create `src/app/tasks.rs`
  Own `AppTasks`, wrapping `CancellationToken`, `TaskTracker` for detached
  tasks, and `TaskTracker` for critical tasks.

- Modify `src/app/mod.rs`
  Import the new modules, replace `BgResult`/`bg_tx`/`bg_rx`/`should_quit`/
  `pending_save`, route saves through critical tasks, handle messages, and
  update tests.

No changes are planned for `src/frontend/mod.rs`, `src/tui/**`,
`src/terminal/**`, `src/protocol/key_event.rs`, `src/app/dispatcher.rs`, or
`src/app/executor.rs`.

---

### Task 1: Add Task Infrastructure

**Files:**
- Modify: `Cargo.toml`
- Create: `src/app/tasks.rs`
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Add the dependency**

Edit `Cargo.toml` dependencies to include `tokio-util`:

```toml
[dependencies]
ropey = "1"
crossterm = { version = "0.29", features = ["event-stream"] }
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["full"] }
futures = "0.3"
taffy = "0.11"
```

- [ ] **Step 2: Create `src/app/tasks.rs`**

Add this file:

```rust
use std::future::Future;

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[derive(Debug)]
pub(crate) struct AppTasks {
    cancel: CancellationToken,
    detached_tasks: TaskTracker,
    critical_tasks: TaskTracker,
}

impl AppTasks {
    pub(crate) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            detached_tasks: TaskTracker::new(),
            critical_tasks: TaskTracker::new(),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    pub(crate) fn spawn_detached<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.detached_tasks.spawn(task);
    }

    pub(crate) fn spawn_critical<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.critical_tasks.spawn(task);
    }

    pub(crate) fn close_detached(&self) {
        self.detached_tasks.close();
    }

    pub(crate) fn close_critical(&self) {
        self.critical_tasks.close();
    }

    pub(crate) async fn wait_critical(&self) {
        self.critical_tasks.wait().await;
    }
}

impl Default for AppTasks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_critical_waits_for_critical_task() {
        let tasks = AppTasks::new();
        let (tx, rx) = oneshot::channel();
        tasks.spawn_critical(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            tx.send(()).unwrap();
        });

        tasks.close_critical();
        tasks.wait_critical().await;

        assert!(rx.await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn detached_tasks_do_not_block_waiting_for_critical_tasks() {
        let tasks = AppTasks::new();
        tasks.spawn_detached(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        tasks.close_detached();
        tasks.close_critical();

        let result =
            tokio::time::timeout(Duration::from_millis(50), tasks.wait_critical()).await;
        assert!(result.is_ok());
    }
}
```

- [ ] **Step 3: Register the module**

At the top of `src/app/mod.rs`, add:

```rust
mod tasks;
```

Do not wire it into `App` yet.

- [ ] **Step 4: Run the focused tests**

Run:

```powershell
cargo test app::tasks
```

Expected: PASS. `Cargo.lock` is updated because `tokio-util` is added.

- [ ] **Step 5: Commit**

Run:

```powershell
git add Cargo.toml Cargo.lock src\app\mod.rs src\app\tasks.rs
git commit -m "feat(app): add task tracking infrastructure"
```

---

### Task 2: Add Internal App Messages

**Files:**
- Create: `src/app/message.rs`
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Create `src/app/message.rs`**

Add:

```rust
use std::io;

use crate::protocol::ids::ContentId;

#[derive(Debug)]
pub(crate) enum AppMessage {
    SaveCompleted {
        content: ContentId,
        result: io::Result<()>,
    },
}
```

- [ ] **Step 2: Register and import the message module**

At the top of `src/app/mod.rs`, add:

```rust
mod message;
```

Near the existing app imports, add:

```rust
use crate::app::message::AppMessage;
```

Do not remove `BgResult` yet.

- [ ] **Step 3: Add failing message handler tests**

Inside `#[cfg(test)] mod tests` in `src/app/mod.rs`, add these tests near the
save tests:

```rust
    #[test]
    fn save_completed_ok_marks_buffer_saved() {
        let mut app = make_app(vec![], None);
        {
            let buf = app
                .contents
                .get_mut(&editor_cid())
                .and_then(|c| c.buffer_mut())
                .unwrap();
            buf.insert_char(0, 'x');
            assert!(buf.modified());
        }
        app.pending_saves.insert(editor_cid());

        app.handle_app_message(AppMessage::SaveCompleted {
            content: editor_cid(),
            result: Ok(()),
        })
        .unwrap();

        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert!(!app.pending_saves.contains(&editor_cid()));
        assert!(!buf.modified());
        assert_eq!(buf.status(), StatusMessage::Saved);
    }

    #[test]
    fn save_completed_err_marks_buffer_save_failed() {
        let mut app = make_app(vec![], None);
        app.pending_saves.insert(editor_cid());

        app.handle_app_message(AppMessage::SaveCompleted {
            content: editor_cid(),
            result: Err(io::Error::new(io::ErrorKind::Other, "boom")),
        })
        .unwrap();

        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert!(!app.pending_saves.contains(&editor_cid()));
        assert_eq!(buf.status(), StatusMessage::SaveFailed);
    }
```

- [ ] **Step 4: Run tests to verify they fail**

Run:

```powershell
cargo test save_completed -- --nocapture
```

Expected: FAIL to compile because `pending_saves` and
`handle_app_message` do not exist yet.

- [ ] **Step 5: Commit only if no code was changed beyond the intentional
failing tests**

Do not commit this task yet. The tests are intentionally red and will be made
green in Task 3.

---

### Task 3: Migrate App State And Message Handling

**Files:**
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Replace imports and fields**

In `src/app/mod.rs`, change:

```rust
use std::collections::HashMap;
```

to:

```rust
use std::collections::{HashMap, HashSet};
```

Add:

```rust
use tokio::sync::mpsc;

use crate::app::message::AppMessage;
use crate::app::tasks::AppTasks;
```

Remove the old `BgResult` enum:

```rust
#[derive(Debug)]
enum BgResult {
    SaveResult(ContentId, io::Result<()>),
}
```

Replace these `App` fields:

```rust
should_quit: bool,
frontend: F,
bg_tx: mpsc::Sender<BgResult>,
bg_rx: mpsc::Receiver<BgResult>,
pending_save: Option<ContentId>,
```

with:

```rust
frontend: F,
message_tx: mpsc::UnboundedSender<AppMessage>,
message_rx: mpsc::UnboundedReceiver<AppMessage>,
tasks: AppTasks,
pending_saves: HashSet<ContentId>,
```

- [ ] **Step 2: Update `App::new` initialization**

Replace:

```rust
let (bg_tx, bg_rx) = mpsc::channel::<BgResult>(8);
```

with:

```rust
let (message_tx, message_rx) = mpsc::unbounded_channel::<AppMessage>();
```

Replace these struct fields:

```rust
should_quit: false,
frontend,
bg_tx,
bg_rx,
pending_save: None,
```

with:

```rust
frontend,
message_tx,
message_rx,
tasks: AppTasks::new(),
pending_saves: HashSet::new(),
```

- [ ] **Step 3: Replace `handle_bg_result` with `handle_app_message`**

Delete `handle_bg_result`.

Add:

```rust
    fn handle_app_message(&mut self, message: AppMessage) -> io::Result<()> {
        match message {
            AppMessage::SaveCompleted { content, result } => {
                self.pending_saves.remove(&content);
                let buf = self
                    .contents
                    .get_mut(&content)
                    .and_then(|c| c.buffer_mut())
                    .expect("saved buffer exists");
                match result {
                    Ok(()) => {
                        buf.mark_saved();
                        buf.set_status(StatusMessage::Saved);
                    }
                    Err(_) => buf.set_status(StatusMessage::SaveFailed),
                }
            }
        }
        Ok(())
    }
```

- [ ] **Step 4: Run the message tests**

Run:

```powershell
cargo test save_completed -- --nocapture
```

Expected: PASS for the two `save_completed_*` tests. Other tests may still
fail to compile because `run`, `spawn_save`, and `execute_operation` still use
old fields.

- [ ] **Step 5: Commit is still deferred**

Do not commit yet because the crate may not compile until Task 4.

---

### Task 4: Migrate Save Spawning And Run Loop

**Files:**
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Update quit handling**

In `handle_event`, keep resize and key handling unchanged. `QuitRequest`
becomes:

```rust
FrontendEvent::QuitRequest => self.tasks.cancel(),
```

In `execute_operation`, replace:

```rust
Operation::Quit => self.should_quit = true,
```

with:

```rust
Operation::Quit => self.tasks.cancel(),
```

- [ ] **Step 2: Replace `spawn_save`**

Replace the full old `spawn_save` with:

```rust
    /// 发起异步保存。返回是否真正发起（同一 content 已在保存时忽略）。
    fn spawn_save(&mut self, id: ContentId) -> bool {
        if self.pending_saves.contains(&id) {
            return false;
        }
        let (path, bytes) = {
            let buf = match self.contents.get_mut(&id).and_then(|c| c.buffer_mut()) {
                Some(b) => b,
                None => return false,
            };
            let path = match buf.path().map(|p| p.to_path_buf()) {
                Some(p) => p,
                None => {
                    buf.set_status(StatusMessage::SaveFailed);
                    return false;
                }
            };
            (path, buf.slice().to_string())
        };
        let tx = self.message_tx.clone();
        self.pending_saves.insert(id);
        self.tasks.spawn_critical(async move {
            let result = tokio::fs::write(path, bytes).await;
            let _ = tx.send(AppMessage::SaveCompleted {
                content: id,
                result,
            });
        });
        true
    }
```

- [ ] **Step 3: Add `shutdown_tasks`**

Add this method near `run`:

```rust
    async fn shutdown_tasks(&mut self) -> io::Result<()> {
        self.tasks.cancel();
        self.tasks.close_detached();
        self.tasks.close_critical();
        self.tasks.wait_critical().await;
        while let Ok(message) = self.message_rx.try_recv() {
            self.handle_app_message(message)?;
        }
        Ok(())
    }
```

- [ ] **Step 4: Replace `run`**

Replace the full `run` implementation with:

```rust
    pub async fn run(&mut self) -> io::Result<()> {
        self.render()?;
        loop {
            tokio::select! {
                ev = self.frontend.next_event() => {
                    match ev? {
                        Some(event) => self.handle_event(event).await?,
                        None => self.tasks.cancel(),
                    }
                }
                message = self.message_rx.recv() => {
                    if let Some(message) = message {
                        self.handle_app_message(message)?;
                    } else {
                        self.tasks.cancel();
                    }
                }
                _ = self.tasks.cancelled() => {
                    self.shutdown_tasks().await?;
                    break;
                }
            }
            if !self.tasks.is_cancelled() {
                self.render()?;
            }
        }
        Ok(())
    }
```

- [ ] **Step 5: Update the old quit assertion**

In `run_inserts_char_then_quits`, replace:

```rust
assert!(app.should_quit);
```

with:

```rust
assert!(app.tasks.is_cancelled());
```

- [ ] **Step 6: Run App tests**

Run:

```powershell
cargo test app:: -- --nocapture
```

Expected: PASS or only failures in tests that still assert old duplicate-save
semantics. There should be no compile errors for `BgResult`, `bg_tx`, `bg_rx`,
`pending_save`, or `should_quit`.

- [ ] **Step 7: Commit**

Run:

```powershell
git add Cargo.toml Cargo.lock src\app\mod.rs src\app\message.rs src\app\tasks.rs
git commit -m "refactor(app): route background work through tracked messages"
```

---

### Task 5: Add Save De-Dupe And Shutdown Coverage

**Files:**
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Add a duplicate-save test**

Inside `#[cfg(test)] mod tests`, add:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_save_ignores_duplicate_pending_save_for_same_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedupe.txt");
        std::fs::write(&path, "hello").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(vec![], Some(&path_str));

        assert!(app.spawn_save(editor_cid()));
        assert!(!app.spawn_save(editor_cid()));
        assert!(app.pending_saves.contains(&editor_cid()));

        app.shutdown_tasks().await.unwrap();

        assert!(!app.pending_saves.contains(&editor_cid()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.status(), StatusMessage::Saved);
    }
```

- [ ] **Step 2: Add an exit-waits-for-save test**

Add:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn run_waits_for_pending_save_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wait-save.txt");
        std::fs::write(&path, "hi").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::ctrl('s')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            Some(&path_str),
        );

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), app.run()).await;
        assert!(result.is_ok());
        result.unwrap().unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Xhi");
        assert!(!app.pending_saves.contains(&editor_cid()));
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.status(), StatusMessage::Saved);
    }
```

- [ ] **Step 3: Run targeted save tests**

Run:

```powershell
cargo test save -- --nocapture
```

Expected: PASS, including existing `ctrl_s_saves_file_and_marks_saved`,
`prefix_key_sequence_saves`, and the new tests.

- [ ] **Step 4: Commit**

Run:

```powershell
git add src\app\mod.rs
git commit -m "test(app): cover tracked save shutdown semantics"
```

---

### Task 6: Final Cleanup And Verification

**Files:**
- Modify if needed: `src/app/mod.rs`, `src/app/message.rs`,
  `src/app/tasks.rs`, `Cargo.toml`, `Cargo.lock`

- [ ] **Step 1: Search for stale event-loop names**

Run:

```powershell
rg "BgResult|bg_tx|bg_rx|pending_save|should_quit" src\app
```

Expected: no matches.

- [ ] **Step 2: Search for forbidden frontend regressions**

Run:

```powershell
rg "FrontendImpl|HeadlessFrontend|Box<dyn Frontend>" src
```

Expected: no matches.

- [ ] **Step 3: Format**

Run:

```powershell
cargo fmt
```

Expected: command exits successfully.

- [ ] **Step 4: Run the full test suite**

Run:

```powershell
cargo test
```

Expected: all tests pass.

- [ ] **Step 5: Check diff hygiene**

Run:

```powershell
git diff --check
```

Expected: no output.

- [ ] **Step 6: Inspect status**

Run:

```powershell
git status --short
```

Expected: only intended files are modified. `AGENTS.md` may already be
modified from the earlier documentation update; do not include it in commits
for this implementation unless the user explicitly asks.

- [ ] **Step 7: Commit final cleanup if any files changed after Task 5**

If `cargo fmt` or cleanup changed implementation files, run:

```powershell
git add Cargo.toml Cargo.lock src\app\mod.rs src\app\message.rs src\app\tasks.rs
git commit -m "chore(app): verify event loop task management cleanup"
```

If no files changed after Task 5, skip this commit.

---

## Self-Review

**Spec coverage:** Covered all design requirements: `AppMessage`,
unbounded internal message channel, `CancellationToken`, two `TaskTracker`
classes, `pending_saves: HashSet<ContentId>`, save completion handling,
critical-task shutdown waiting, detached-task non-blocking behavior, no
frontend trait/tui changes, and final stale-name checks.

**Placeholder scan:** No `TBD`, `TODO`, “implement later”, or vague test steps
are used. Each code-changing step includes concrete code.

**Type consistency:** The plan consistently uses `AppMessage`,
`AppTasks`, `message_tx`, `message_rx`, `pending_saves`,
`spawn_critical`, `spawn_detached`, and `shutdown_tasks`.
