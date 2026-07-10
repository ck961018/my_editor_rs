# 前端抽象层 + 双输入 bug 修复 设计

> 关联实现计划：后续由 writing-plans 基于本文生成。
> 关联基线：`docs/design/architecture_design_rust.md`（TUI Editor v0.1 架构设计）。

## 1. 背景与动机

v0.1 已实现并提交（18 commits，59 测试通过）。本次针对两个问题：

1. **双输入 bug**：在 Windows 上每次按键被识别为两次——输入字符打印两个，回车换两行。Unix 不复现。
2. **App 感知 Tui**：`App` 直接持有 `TuiFrontend`、`Output<io::Stdout>`、`Input`（crossterm EventStream），并直接调 `self.tui.resize(...)`。App 完全感知了 Tui 与终端细节，与设计文档 §"未来"（`TuiFrontend / GuiFrontend / RemoteFrontend`，第 93 行）预期的多前端架构相悖。

## 2. 目标

- 引入 `Frontend` trait 作为前端层；`App` 只依赖该 trait，不再出现 `TuiFrontend`/`Output`/`Input`。
- 编译期选择具体前端：泛型 `App<F: Frontend>`，`main.rs` 构造 `TuiFrontend` 注入。暂不加 Cargo feature flag（Gui 真到来时再加，YAGNI）。
- 修复双输入 bug：过滤 crossterm 的 `KeyEventKind::Release`。
- 顺带收益：`App` 由"持有真实 Stdout/EventStream、不可测"变为可注入伪造前端、可单元测试。

## 3. 非目标（YAGNI）

- 不实现 Gui frontend（仅留抽象接缝）。
- 不加 Cargo feature flag（`tui`/`gui`）。泛型 `App<F>` 已满足"编译期选择"。
- 不改 `Editor`/`Buffer`/`Cursor`/`Status`/protocol 数据类型等核心逻辑。
- 不引入关联错误类型 `type Error`（v0.1 用 `io::Result` 足够）。
- 不改 async 架构（保留 tokio + crossterm event-stream，符合基线设计 §1）。

## 4. 双输入 bug 根因与修复

**根因**：`src/terminal/input.rs` 中 `next_event` 对 `Event::Key(k)` 直接调 `translate_key(k)`，未检查 `k.kind`。crossterm 在 Windows 对每个物理按键发出 `KeyEventKind::Press` 与 `KeyEventKind::Release` 两个事件；两者都被翻译 → 每键处理两次。Unix 只发 `Press`，故不复现。

**修复**：`src/terminal/input.rs` 提取纯函数 `map_event(Event) -> Option<FrontendEvent>`，对 `Event::Key(k)` 仅当 `k.kind != KeyEventKind::Release` 时翻译（即接受 `Press` 与 `Repeat`，忽略 `Release`）。

```rust
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use std::io;
use crate::protocol::frontend_event::{FrontendEvent, ResizeEvent};
use crate::protocol::key_event::translate_key;

pub struct Input { events: EventStream }

impl Input {
    pub fn new() -> Self { Self { events: EventStream::new() } }

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
```

**行为差异**：仅 Windows 改变（过滤 Release）；Unix 无 Release 事件，零变化。`Repeat` 被接受 → 按住键时连续输入/退格仍生效。`translate_key`/`key_event.rs` 不动。

## 5. 架构：`Frontend` trait

新增顶层模块 `src/frontend.rs`。它只依赖纯数据类型（`FrontendEvent`/`CorePatch`/`Editor`/`io`），不含 crossterm/tokio，与 protocol 同样纯净。

```rust
// src/frontend.rs
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

**要点**：
- `async fn next_event` 保留基线的 tokio + EventStream 架构；静态分发下 `async fn` in trait 在 Rust ≥1.75 可用，无需 boxed future。
- `render(&Editor)` 不暴露 `Output<W>`——输出介质是前端私有细节（Tui 用 `Output<Stdout>`，未来 Gui 用窗口）。App 永远看不到 `Output`。
- 错误用 `io::Result`（v0.1 足够）。
- trait 放 `src/frontend.rs` 而非 `protocol/`：protocol 是纯数据契约，`Frontend` 是行为层接缝，概念独立。

**模块依赖方向（改动后）**：
```
main.rs  →  frontend::Frontend (trait)  ←  App 依赖
                 ↑ impl
            tui::TuiFrontend (持有 Input + Output + Viewport)
                 ↓
            terminal::{Input, Output, lifecycle}
```
App 依赖收敛为：`Editor`、`PatchList`、`FrontendEvent`、`Frontend` trait——不再出现 `TuiFrontend`/`Output`/`Input`。

## 6. `TuiFrontend` 重构为自包含前端

`TuiFrontend` 拥有自己的输入与输出，成为完整的 `Frontend` 实现。`Input` 与 `Output<Stdout>` 从 `App` 迁入。

```rust
// src/tui/tui_frontend.rs
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
    input: Input,               // 从 App 迁入
    output: Output<Stdout>,     // 从 App 迁入
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
    pub(crate) fn render_to<W: io::Write>(
        &mut self, editor: &Editor, output: &mut Output<W>,
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
            CorePatch::BufferChanged => { self.text_dirty = true; }
            CorePatch::CursorMoved => {
                let old_top = self.viewport.top_row;
                self.viewport.ensure_cursor_visible(editor.cursor().row);
                self.text_dirty = true;
                if self.viewport.top_row != old_top {
                    self.needs_full_redraw = true;
                }
            }
            CorePatch::StatusChanged => { self.status_dirty = true; }
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

**要点**：
- `apply_patch` / `resize` 内部逻辑原样保留（上次已审查通过）。
- 提取 `render_to<W>` 承载绘制逻辑，trait `render` 转发到 `self.output`；现有 6 个单测改调 `render_to` 并注入 `Output::new(Vec::new())`，逻辑/断言不变。
- `TuiFrontend` 自给自足，App 不再分别构造 `Input`/`Output`。

## 7. `App` 重构为泛型 `App<F: Frontend>`

```rust
// src/app.rs
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
            // App 只调 trait 方法，不感知 Tui；resize 维度交给前端消化
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
```

**`main.rs` 改动**：
```rust
mod frontend;   // 新增
mod app;
mod core;
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
    let (w, h) = term_size().unwrap_or((80, 24));   // 从 app.rs 迁来
    let frontend = TuiFrontend::new(w as usize, h as usize);
    let mut app = App::new(path, frontend)?;
    app.run().await?;
    Ok(())
}
```

**要点**：
- `App` 依赖列表去掉 `TuiFrontend`/`Output`/`Input`/`term_size`——只剩 `Editor`/`Frontend`/`PatchList`/`FrontendEvent`。基线 §2 不变式"App 不感知 Tui"达成。
- `term_size()` 从 `App::new` 移到 `main`（终端胶水本就该在 main）。
- resize 仍在 App 循环里调 `self.frontend.resize(...)`——这是 App→trait 调用，非 App→Tui，符合抽象。

## 8. 测试策略

1. **`terminal/input.rs`（新增）** — `map_event` 纯函数：
   - `KeyEvent{kind: Release, Char('a')}` → `None`（修复核心断言）。
   - `KeyEvent{kind: Press, Char('a')}` → `Some(Key(Char(b'a')))`。
   - `KeyEvent{kind: Repeat, Char('a')}` → `Some(...)`（按住仍生效）。
   - `Event::Resize(80, 24)` → `Some(Resize{...})`。
   - 构造 Release 用 `KeyEvent::new_with_kind(code, mods, KeyEventKind::Release)`。
   - `EventStream` 本身不测（真实 I/O，无法头less 单测）。

2. **`app.rs`（新增，关键收益）** — 用伪造前端测 `App::run`：
   - `ScriptedFrontend`：`next_event` 弹出脚本化事件队列；`apply_patch`/`render`/`resize` 记录调用。
   - 喂 `Char('a')` → `Ctrl(Q)`：断言 `editor.buffer()` 含 `'a'`、`render` 被调用、循环正常退出。
   - 喂 `Resize{80,24}`：断言 `frontend.resize` 被调用且维度正确。
   - 这是当前架构做不到的（App 曾持真实 Stdout/EventStream），是本次重构主要可测性收益。

3. **`tui/tui_frontend.rs`（小改）** — 6 个现有测试改调 `render_to<W>` 注入 `Vec<u8>`，逻辑/断言不变。

4. **`frontend.rs`** — trait 无逻辑，不单测。

**回归**：`cargo test` 全绿（现有 59 + 新增约 6-8 ≈ 66）；`cargo build --release` 通过。交互冒烟（`cargo run -- smoke.txt`，单字符不重复、回车单换行、Ctrl-S/Ctrl-Q 正常）仍需在真实终端验证——头less 无法覆盖。

## 9. 完成标准

- [ ] `src/terminal/input.rs`：`map_event` 过滤 `Release`，Windows 双输入消失。
- [ ] `src/frontend.rs`：`Frontend` trait 定义，无 crossterm/tokio 依赖。
- [ ] `src/tui/tui_frontend.rs`：实现 `Frontend`，持有 `Input`+`Output`，`render_to` 供测试。
- [ ] `src/app.rs`：泛型 `App<F: Frontend>`，依赖中无 `TuiFrontend`/`Output`/`Input`。
- [ ] `src/main.rs`：构造 `TuiFrontend` 注入 `App`；`term_size` 在 main。
- [ ] 新增 `map_event` 测试（Release/Press/Repeat/Resize）。
- [ ] 新增 `App` 伪造前端测试（输入、退出、resize）。
- [ ] 现有 6 个 `tui_frontend` 测试改调 `render_to` 后仍绿。
- [ ] `cargo test` 全绿；`cargo build --release` 通过。
- [ ] 基线 §2 不变式"App 不感知 Tui"经审查确认。
