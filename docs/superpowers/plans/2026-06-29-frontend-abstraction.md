# 前端抽象层 + 双输入 bug 修复 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 引入 `Frontend` trait 让 `App` 不再感知 Tui（泛型 `App<F: Frontend>`，编译期选择前端），并修复 Windows 上每次按键被识别两次的 bug。

**Architecture:** 新增 `src/frontend.rs` 定义 `Frontend` trait（`async fn next_event` + `apply_patch`/`render`/`resize`，无 crossterm/tokio 依赖）。`TuiFrontend` 拥有自己的 `Input`+`Output` 并实现该 trait；`App` 变为泛型 `App<F: Frontend>`，`main.rs` 构造 `TuiFrontend` 注入。双输入 bug 在 `terminal/input.rs` 提取纯函数 `map_event` 过滤 `KeyEventKind::Release`。

**Tech Stack:** Rust edition 2021（需 Rust ≥1.75 以支持 trait 内 `async fn`）、crossterm 0.28（event-stream）、tokio 1（full）、futures 0.3。

**Spec:** `docs/superpowers/specs/2026-06-29-frontend-abstraction-design.md`

---

## 文件结构

- **Create** `src/frontend.rs` — `Frontend` trait，前端层接缝；纯数据依赖，无 crossterm/tokio。
- **Modify** `src/terminal/input.rs` — 提取 `map_event` 纯函数过滤 `Release`，修复双输入。
- **Modify** `src/tui/tui_frontend.rs` — 拥有 `Input`+`Output`，`impl Frontend`，提取 `render_to<W>` 供测试注入。
- **Modify** `src/app.rs` — 泛型 `App<F: Frontend>`，依赖中无 `TuiFrontend`/`Output`/`Input`。
- **Modify** `src/main.rs` — `mod frontend;`，构造 `TuiFrontend` 注入 `App`，`term_size` 迁入 main。

每个任务结束后工作树可编译、`cargo test` 全绿，并提交。

---

## Task 1: 修复双输入 bug — `map_event` 过滤 Release

**Files:**
- Modify: `src/terminal/input.rs`

**背景：** crossterm 在 Windows 对每个物理按键发出 `KeyEventKind::Press` 与 `KeyEventKind::Release` 两个事件；当前 `next_event` 对两者都调 `translate_key`，导致每键处理两次（字符打两个、回车换两行）。Unix 只发 `Press`，不复现。修复方式：提取纯函数 `map_event`，仅接受 `Press`/`Repeat`，忽略 `Release`。

- [ ] **Step 1: 写失败测试（Release 被忽略）**

在 `src/terminal/input.rs` 末尾追加测试模块（当前文件无 `#[cfg(test)]` 块）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        Event, KeyCode, KeyEvent as CrosstermKey, KeyEventKind, KeyModifiers,
    };
    use crate::protocol::frontend_event::{FrontendEvent, ResizeEvent};
    use crate::protocol::key_event::KeyEvent;

    fn key_event(code: KeyCode, kind: KeyEventKind) -> CrosstermKey {
        CrosstermKey::new_with_kind(code, KeyModifiers::empty(), kind)
    }

    #[test]
    fn release_event_is_ignored() {
        let ev = Event::Key(key_event(KeyCode::Char('a'), KeyEventKind::Release));
        assert_eq!(map_event(ev), None);
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test release_event_is_ignored`
Expected: 编译失败，`cannot find function map_event`（函数尚未定义）。

- [ ] **Step 3: 实现 `map_event` 并改造 `next_event`**

把 `src/terminal/input.rs` 整体替换为：

```rust
use std::io;

use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;

use crate::protocol::frontend_event::{FrontendEvent, ResizeEvent};
use crate::protocol::key_event::translate_key;

pub struct Input {
    events: EventStream,
}

impl Input {
    pub fn new() -> Self {
        Self {
            events: EventStream::new(),
        }
    }

    pub async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        match self.events.next().await {
            Some(Ok(ev)) => Ok(map_event(ev)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }
}

/// 纯函数：crossterm Event → FrontendEvent。
/// Windows 上每个物理键有 Press + Release；只接受 Press / Repeat，忽略 Release，
/// 否则每个字符输入两次、回车换两行。Unix 只发 Press，过滤为 no-op。
fn map_event(ev: Event) -> Option<FrontendEvent> {
    match ev {
        Event::Key(k) => {
            if k.kind == KeyEventKind::Release {
                None
            } else {
                Some(FrontendEvent::Key(translate_key(k)))
            }
        }
        Event::Resize(w, h) => Some(FrontendEvent::Resize(ResizeEvent { width: w, height: h })),
        _ => None, // mouse / focus 等忽略
    }
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        Event, KeyCode, KeyEvent as CrosstermKey, KeyEventKind, KeyModifiers,
    };
    use crate::protocol::frontend_event::{FrontendEvent, ResizeEvent};
    use crate::protocol::key_event::KeyEvent;

    fn key_event(code: KeyCode, kind: KeyEventKind) -> CrosstermKey {
        CrosstermKey::new_with_kind(code, KeyModifiers::empty(), kind)
    }

    #[test]
    fn release_event_is_ignored() {
        let ev = Event::Key(key_event(KeyCode::Char('a'), KeyEventKind::Release));
        assert_eq!(map_event(ev), None);
    }
}
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test release_event_is_ignored`
Expected: PASS。

- [ ] **Step 5: 补充 Press / Repeat / Resize 测试**

在 `mod tests` 内追加（`release_event_is_ignored` 之后）：

```rust
    #[test]
    fn press_event_translates() {
        let ev = Event::Key(key_event(KeyCode::Char('a'), KeyEventKind::Press));
        assert_eq!(
            map_event(ev),
            Some(FrontendEvent::Key(KeyEvent::Char(b'a')))
        );
    }

    #[test]
    fn repeat_event_translates() {
        // 按住键时的 Repeat 仍应触发输入，不能被过滤
        let ev = Event::Key(key_event(KeyCode::Char('a'), KeyEventKind::Repeat));
        assert_eq!(
            map_event(ev),
            Some(FrontendEvent::Key(KeyEvent::Char(b'a')))
        );
    }

    #[test]
    fn resize_event_translates() {
        let ev = Event::Resize(80, 24);
        assert_eq!(
            map_event(ev),
            Some(FrontendEvent::Resize(ResizeEvent { width: 80, height: 24 }))
        );
    }
```

- [ ] **Step 6: 运行全部新增测试，确认通过**

Run: `cargo test map_event` 无效（函数私有，用模块名过滤）；改为：
Run: `cargo test terminal::input`
Expected: 4 个测试全 PASS。

- [ ] **Step 7: 全量回归 + 提交**

Run: `cargo test`
Expected: 全部通过（原 59 + 新增 4 = 63）。

```bash
git add src/terminal/input.rs
git commit -m "fix: 过滤 crossterm Release 事件修复 Windows 双输入"
```

---

## Task 2: 新增 `Frontend` trait

**Files:**
- Create: `src/frontend.rs`
- Modify: `src/main.rs`（加 `mod frontend;`）

**背景：** 前端层接缝。App 将只依赖此 trait。trait 只依赖纯数据类型，不含 crossterm/tokio。本任务仅定义 trait 并注册模块，尚无实现者——会有未使用警告，下一任务消除。

- [ ] **Step 1: 创建 `src/frontend.rs`**

```rust
use std::io;

use crate::core::editor::Editor;
use crate::protocol::core_patch::CorePatch;
use crate::protocol::frontend_event::FrontendEvent;

/// 前端层抽象：App 只依赖此 trait，不感知 Tui/终端/GUI 细节。
/// 具体实现（TuiFrontend，以及未来的 GuiFrontend）在编译期由 main 选择并注入。
pub trait Frontend {
    /// 等待下一个前端输入事件；None 表示该事件被前端吞掉（如 key Release）。
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;

    /// 消费核心产生的 patch，更新前端内部脏标记/视口。
    fn apply_patch(&mut self, patch: &CorePatch, editor: &Editor);

    /// 把当前 editor 状态绘制到前端各自的输出介质。
    fn render(&mut self, editor: &Editor) -> io::Result<()>;

    /// 通知前端输出介质尺寸变化（终端 resize 等）。
    fn resize(&mut self, width: usize, height: usize);
}
```

- [ ] **Step 2: 在 `src/main.rs` 注册模块**

在 `src/main.rs` 顶部 `mod` 列表加入 `frontend`（保持字母序无所谓，放在 `app` 后即可）。把现有的：

```rust
mod app;
mod core;
mod protocol;
mod terminal;
mod tui;
```

改为：

```rust
mod app;
mod core;
mod frontend;
mod protocol;
mod terminal;
mod tui;
```

- [ ] **Step 3: 确认编译（允许 unused 警告）**

Run: `cargo build`
Expected: 编译通过；可能有 `trait Frontend is never used` 之类的 dead_code 警告——正常，Task 3 消除。

- [ ] **Step 4: 提交**

```bash
git add src/frontend.rs src/main.rs
git commit -m "feat: 新增 Frontend trait 前端层抽象"
```

---

## Task 3: `TuiFrontend` 拥有 I/O 并实现 `Frontend`

**Files:**
- Modify: `src/tui/tui_frontend.rs`
- Modify: `src/app.rs`

**背景：** 把 `Input` 与 `Output<Stdout>` 从 `App` 迁入 `TuiFrontend`，使其成为自包含的 `Frontend` 实现。提取 `render_to<W>` 承载绘制逻辑供测试注入 `Vec<u8>`。`App` 仍为具体类型 `App { editor, tui }`（暂不泛型化），但改为通过 trait 方法驱动 `tui`，并去掉自身的 `input`/`output` 字段。此任务结束后工作树可编译、原有测试全绿。

- [ ] **Step 1: 重写 `src/tui/tui_frontend.rs` 的非测试部分**

把 `src/tui/tui_frontend.rs` 第 1–76 行（从 `use std::io;` 到 `impl TuiFrontend { ... }` 的 `render` 方法结束、`}` 闭合 impl，即第 76 行）整体替换为：

```rust
use std::io::{self, Stdout};

use crate::core::editor::Editor;
use crate::frontend::Frontend;
use crate::protocol::core_patch::CorePatch;
use crate::protocol::frontend_event::FrontendEvent;
use crate::terminal::input::Input;
use crate::terminal::output::Output;
use crate::tui::renderer::Renderer;
use crate::tui::viewport::Viewport;

pub struct TuiFrontend {
    viewport: Viewport,
    needs_full_redraw: bool,
    text_dirty: bool,
    status_dirty: bool,
    input: Input,
    output: Output<Stdout>,
}

impl TuiFrontend {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            viewport: Viewport::new(width, height),
            needs_full_redraw: true,
            text_dirty: true,
            status_dirty: true,
            input: Input::new(),
            output: Output::new(io::stdout()),
        }
    }

    /// 内部绘制到可注入 output，供单测用 Vec<u8> 断言 VT 输出。
    fn render_to<W: io::Write>(
        &mut self,
        editor: &Editor,
        output: &mut Output<W>,
    ) -> io::Result<()> {
        if self.needs_full_redraw {
            output.clear_screen()?;
            Renderer::draw(output, editor, &self.viewport)?;
            self.needs_full_redraw = false;
            self.text_dirty = false;
            self.status_dirty = false;
        } else if self.text_dirty || self.status_dirty {
            Renderer::draw(output, editor, &self.viewport)?;
            self.text_dirty = false;
            self.status_dirty = false;
        }
        Ok(())
    }
}

impl Frontend for TuiFrontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        self.input.next_event().await
    }

    fn apply_patch(&mut self, patch: &CorePatch, editor: &Editor) {
        match patch {
            CorePatch::BufferChanged => {
                self.text_dirty = true;
            }
            CorePatch::CursorMoved => {
                let old_top = self.viewport.top_row;
                self.viewport.ensure_cursor_visible(editor.cursor().row);
                // 光标移动总要重绘以更新光标位置；若 top_row 变化则需全屏重绘。
                self.text_dirty = true;
                if self.viewport.top_row != old_top {
                    self.needs_full_redraw = true;
                }
            }
            CorePatch::StatusChanged => {
                self.status_dirty = true;
            }
            CorePatch::FullRedrawRequired => {
                self.needs_full_redraw = true;
                self.text_dirty = true;
                self.status_dirty = true;
            }
        }
    }

    fn render(&mut self, editor: &Editor) -> io::Result<()> {
        self.render_to(editor, &mut self.output)
    }

    fn resize(&mut self, width: usize, height: usize) {
        self.viewport = Viewport::new(width, height);
        self.needs_full_redraw = true;
        self.text_dirty = true;
        self.status_dirty = true;
    }
}
```

- [ ] **Step 2: 更新 `tui_frontend.rs` 内 2 个 render 测试改调 `render_to`**

在 `src/tui/tui_frontend.rs` 的 `mod tests` 中：

把 `render_outputs_when_dirty` 里的：
```rust
        tf.render(&ed, &mut out).unwrap();
```
改为：
```rust
        tf.render_to(&ed, &mut out).unwrap();
```

把 `render_noop_when_clean` 里的：
```rust
        tf.render(&ed, &mut out).unwrap();
```
改为：
```rust
        tf.render_to(&ed, &mut out).unwrap();
```

（其余 4 个测试 `new_starts_dirty` / `apply_buffer_changed_sets_text_dirty` / `apply_full_redraw_sets_all` / `cursor_moved_below_viewport_triggers_full_redraw` 不调用 render，无需改动。注意：测试构造 `TuiFrontend::new(...)` 现会顺带创建 `Input`/`Output`，但不调用 `next_event`，无 I/O 副作用，安全。）

- [ ] **Step 3: 改造 `src/app.rs` 去掉 input/output，改用 trait 方法驱动 tui**

把 `src/app.rs` 整体替换为：

```rust
use std::io;

use crossterm::terminal::size as term_size;

use crate::core::editor::Editor;
use crate::protocol::core_patch::PatchList;
use crate::protocol::frontend_event::FrontendEvent;
use crate::tui::tui_frontend::TuiFrontend;

pub struct App {
    editor: Editor,
    tui: TuiFrontend,
}

impl App {
    pub fn new(path: Option<&str>) -> io::Result<Self> {
        let mut editor = Editor::new();
        if let Some(p) = path {
            // open_path 内部处理 NotFound/InvalidData/其他 IO，返回 Ok/Err
            editor.open_path(p)?;
        }

        let (width, height) = term_size().unwrap_or((80, 24));

        Ok(Self {
            editor,
            tui: TuiFrontend::new(width as usize, height as usize),
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        self.tui.render(&self.editor)?;

        while !self.editor.should_quit {
            let event = match self.tui.next_event().await? {
                Some(e) => e,
                None => continue,
            };

            // 终端尺寸变化时，先更新 viewport 尺寸（v0.1 仍只做全屏重绘，不重算滚动）。
            if let FrontendEvent::Resize(r) = &event {
                self.tui.resize(r.width as usize, r.height as usize);
            }

            let mut patches = PatchList::new();
            self.editor.handle_event(event, &mut patches)?;

            for patch in patches.items() {
                self.tui.apply_patch(patch, &self.editor);
            }

            self.tui.render(&self.editor)?;
        }

        Ok(())
    }
}
```

说明：`App` 仍具体（`tui: TuiFrontend`），但已不持有 `input`/`output`，全部经 trait 方法驱动。`term_size` 暂留在此，Task 4 迁入 main。

- [ ] **Step 4: 编译 + 全量测试**

Run: `cargo build`
Expected: 编译通过，无 dead_code 警告（`render_to` 被测试使用，`Frontend` 被 `TuiFrontend` 实现）。

Run: `cargo test`
Expected: 全部通过（63 个；tui_frontend 的 6 个测试仍绿，2 个已改调 `render_to`）。

- [ ] **Step 5: 提交**

```bash
git add src/tui/tui_frontend.rs src/app.rs
git commit -m "refactor: TuiFrontend 拥有 Input/Output 并实现 Frontend trait"
```

---

## Task 4: `App` 泛型化 + main 接线 + 伪造前端测试

**Files:**
- Modify: `src/app.rs`
- Modify: `src/main.rs`

**背景：** 把 `App` 改为泛型 `App<F: Frontend>`，`main.rs` 构造 `TuiFrontend` 注入（`term_size` 迁入 main）。这是本次重构的核心收益点：`App` 依赖中彻底去掉 `TuiFrontend`/`Output`/`Input`，且 `App` 首次可被伪造前端单元测试。TDD：先写伪造前端测试（失败——`App` 还不接受任意 `F`），再泛型化使其通过。

- [ ] **Step 1: 在 `src/app.rs` 写失败测试（伪造前端驱动 App::run）**

把 `src/app.rs` 整体替换为（含测试模块）：

```rust
use std::io;

use crate::core::editor::Editor;
use crate::frontend::Frontend;
use crate::protocol::core_patch::PatchList;
use crate::protocol::frontend_event::FrontendEvent;

pub struct App<F: Frontend> {
    editor: Editor,
    frontend: F,
}

impl<F: Frontend> App<F> {
    pub fn new(path: Option<&str>, frontend: F) -> io::Result<Self> {
        let mut editor = Editor::new();
        if let Some(p) = path {
            // open_path 内部处理 NotFound/InvalidData/其他 IO，返回 Ok/Err
            editor.open_path(p)?;
        }
        Ok(Self { editor, frontend })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        self.frontend.render(&self.editor)?;

        while !self.editor.should_quit {
            let event = match self.frontend.next_event().await? {
                Some(e) => e,
                None => continue,
            };

            // App 只调 trait 方法，不感知 Tui；resize 维度交给前端自己消化
            if let FrontendEvent::Resize(r) = &event {
                self.frontend.resize(r.width as usize, r.height as usize);
            }

            let mut patches = PatchList::new();
            self.editor.handle_event(event, &mut patches)?;

            for patch in patches.items() {
                self.frontend.apply_patch(patch, &self.editor);
            }

            self.frontend.render(&self.editor)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use crate::protocol::core_patch::CorePatch;
    use crate::protocol::frontend_event::ResizeEvent;
    use crate::protocol::key_event::{CtrlKey, KeyEvent};

    /// 伪造前端：按脚本顺序弹出事件，记录 render/resize 调用。
    struct ScriptedFrontend {
        events: VecDeque<FrontendEvent>,
        resize_calls: Vec<(usize, usize)>,
        render_calls: usize,
    }

    impl ScriptedFrontend {
        fn new(events: Vec<FrontendEvent>) -> Self {
            Self {
                events: events.into(),
                resize_calls: Vec::new(),
                render_calls: 0,
            }
        }
    }

    impl Frontend for ScriptedFrontend {
        async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
            Ok(self.events.pop_front())
        }
        fn apply_patch(&mut self, _patch: &CorePatch, _editor: &Editor) {}
        fn render(&mut self, _editor: &Editor) -> io::Result<()> {
            self.render_calls += 1;
            Ok(())
        }
        fn resize(&mut self, width: usize, height: usize) {
            self.resize_calls.push((width, height));
        }
    }

    #[tokio::test]
    async fn run_inserts_char_then_quits() {
        let fe = ScriptedFrontend::new(vec![
            FrontendEvent::Key(KeyEvent::Char(b'a')),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ]);
        let mut app = App::new(None, fe).unwrap();
        app.run().await.unwrap();
        assert_eq!(app.editor.buffer().slice().to_string(), "a");
        assert!(app.editor.should_quit());
        assert!(app.frontend.render_calls >= 1, "render 应被调用");
    }

    #[tokio::test]
    async fn run_forwards_resize_to_frontend() {
        let fe = ScriptedFrontend::new(vec![
            FrontendEvent::Resize(ResizeEvent { width: 100, height: 40 }),
            FrontendEvent::Key(KeyEvent::Ctrl(CtrlKey::Q)),
        ]);
        let mut app = App::new(None, fe).unwrap();
        app.run().await.unwrap();
        assert_eq!(app.frontend.resize_calls, vec![(100, 40)]);
    }
}
```

- [ ] **Step 2: 运行测试，确认状态**

Run: `cargo test app::tests`
Expected: 此时 `src/main.rs` 仍在用旧的具体 `App::new(path)`（单参数），编译会失败（`App::new` 现需两个参数）。先做 Step 3 修复 main，再回来跑测试。

- [ ] **Step 3: 更新 `src/main.rs` 接线**

把 `src/main.rs` 整体替换为：

```rust
mod app;
mod core;
mod frontend;
mod protocol;
mod terminal;
mod tui;

use std::io;

use app::App;
use crossterm::terminal::size as term_size;
use terminal::lifecycle::TerminalGuard;
use tui::tui_frontend::TuiFrontend;

#[tokio::main]
async fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str());

    let _guard = TerminalGuard::enter()?;

    let (width, height) = term_size().unwrap_or((80, 24));
    let frontend = TuiFrontend::new(width as usize, height as usize);
    let mut app = App::new(path, frontend)?;
    app.run().await?;
    // _guard drop 时自动恢复终端
    Ok(())
}
```

- [ ] **Step 4: 运行 App 测试，确认通过**

Run: `cargo test app::tests`
Expected: 2 个测试 PASS（`run_inserts_char_then_quits`、`run_forwards_resize_to_frontend`）。

- [ ] **Step 5: 全量回归 + release 构建**

Run: `cargo test`
Expected: 全部通过（63 + 2 = 65）。

Run: `cargo build --release`
Expected: 编译通过。

- [ ] **Step 6: 提交**

```bash
git add src/app.rs src/main.rs
git commit -m "refactor: App 泛型化为 App<F: Frontend>，main 注入 TuiFrontend"
```

---

## 完成检查清单（对应 spec §9）

- [ ] `src/terminal/input.rs`：`map_event` 过滤 `Release`，4 个新测试通过。
- [ ] `src/frontend.rs`：`Frontend` trait 定义，无 crossterm/tokio 依赖。
- [ ] `src/tui/tui_frontend.rs`：实现 `Frontend`，持有 `Input`+`Output`，`render_to` 供测试。
- [ ] `src/app.rs`：泛型 `App<F: Frontend>`，依赖中无 `TuiFrontend`/`Output`/`Input`/`term_size`。
- [ ] `src/main.rs`：构造 `TuiFrontend` 注入 `App`；`term_size` 在 main。
- [ ] 新增 `App` 伪造前端测试（输入、退出、resize）通过。
- [ ] 现有 6 个 `tui_frontend` 测试改调 `render_to` 后仍绿。
- [ ] `cargo test` 全绿（65）；`cargo build --release` 通过。
- [ ] 基线 §2 不变式"App 不感知 Tui"经审查确认（`app.rs` import 中无 `TuiFrontend`/`Output`/`Input`）。

**交互冒烟（需在真实终端验证，头less 无法覆盖）：** `cargo run -- smoke.txt`——输入单字符不重复、回车单换行、方向键移动、Ctrl-S 保存、Ctrl-Q 退出后终端恢复正常。
