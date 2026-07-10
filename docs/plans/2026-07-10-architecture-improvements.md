# Architecture Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将当前编辑器架构中会阻碍 split、多 view、多 content、前端解耦和模式扩展的问题，拆成可逐条执行、可测试、可回滚的改进任务。

**Architecture:** 继续保留当前 `core / protocol / app / frontend / tui / terminal` 的单向依赖思路。优先修正已经影响未来扩展的身份归属和边界问题：渲染项必须携带 `SpaceId`，协议层必须保持中立，内容查询不能依赖持续增长的 `as_xxx` 类型探测，mode/keymap runtime 不应长期绑定在 `Buffer` 内部。

**Tech Stack:** Rust 2024，MSRV 1.85，ropey，crossterm，tokio，futures，taffy，tempfile。

## Global Constraints

- 保持现有模块依赖方向：`frontend -> protocol`，`app -> frontend + core + protocol`，`tui -> frontend + terminal + protocol`，`main -> app + tui + terminal`，`terminal -> protocol`，`core -> protocol/std`，`protocol -> std`。
- 不引入 GUI、LSP、tree-sitter、插件系统或新增 Cargo feature。
- 不做无关重构，不删除为后续功能预留的 `#[allow(dead_code)]` API。
- Rust 代码修改默认运行 `cargo test`；跨层或 API 改动同时运行 `cargo clippy --all-targets --all-features`。
- 文档或注释修改至少运行 `git diff --check`。

---

## 背景判断

当前项目的方向是一个 Rust 终端文本编辑器原型，核心模型接近 Helix/Kakoune 风格：buffer 与 selection 分离，selection 使用 `anchor/head`，命令经 keymap/dispatcher 解析后执行，前端通过 `ContentQuery` pull 内容并渲染。

与 Zed 相比，本项目不是完整 GUI/workspace/collaboration 平台，不应过早引入大型 project model 或 GUI runtime。与 Helix 相比，本项目已经采用了有利于多 selection 的数据结构，但行为路径还没有完全围绕多 view、多 selection 建模。与 rsvim 相比，本项目更轻量，暂时不需要 JS/TS runtime 或远程服务，但如果长期追求 Vim-like extensibility，mode/keymap/plugin runtime 不能一直塞在 `Buffer` 里。

## 执行顺序

1. Task 1：让 TUI 渲染项携带 `SpaceId`，修复同一 content 多 view 的身份问题。
2. Task 2：把 crossterm 按键翻译移出 `protocol`，恢复协议层中立性。
3. Task 3：减少 `ContentHandler` 的 `as_buffer/as_status_bar` 类型探测，建立更稳定的 content 查询边界。
4. Task 4：规划并初步拆出 mode/keymap runtime，降低 `Buffer` 职责。
5. Task 5：建立 scene mutation 与 view lifecycle 的统一入口，为 split/panel 做准备。

---

### Task 1: RenderItem 携带 SpaceId

**问题：** `TaffyEngine` 生成的 `RenderItem` 只包含 `content_id`，`SceneRenderer` 渲染时再通过 `find_space_by_content` 找第一个匹配 space。这在同一个 `ContentId` 被两个 split/view 共享时会错误地复用 viewport 和 selection。

**Files:**
- Modify: `src/tui/resolved.rs`
- Modify: `src/tui/taffy_engine.rs`
- Modify: `src/tui/scene_renderer.rs`
- Test: `src/tui/scene_renderer.rs`
- Test: `src/tui/taffy_engine.rs`

**Interfaces:**
- Produces: `RenderItem { space_id: SpaceId, content_id: ContentId, ... }`
- Consumes: `ContentQuery::selections(space_id)` and `SceneRenderer.viewports[space_id]`

**Steps:**
- [ ] 在 `RenderItem` 中新增 `space_id: SpaceId` 字段。
- [ ] 在 `TaffyEngine::collect` 推入 host item 时填入当前 `sid`。
- [ ] 删除 `SceneRenderer::find_space_by_content` 查询路径，直接使用 `item.space_id` 获取 viewport 和 selections。
- [ ] 增加回归测试：构造两个 host 指向同一个 `ContentId`，分别给不同 `SpaceId` 设置不同 selection，断言渲染时使用各自 selection。
- [ ] 运行 `cargo test tui::taffy_engine tui::scene_renderer`。
- [ ] 运行 `cargo test`。

**Acceptance Criteria:**
- 同一 `ContentId` 出现在多个 `SpaceId` 时，渲染、selection 高亮、viewport 跟随都按 `SpaceId` 隔离。
- `scene_renderer.rs` 中不再需要 `find_space_by_content`。

---

### Task 2: 移除 protocol 对 crossterm 的依赖

**问题：** `src/protocol/key_event.rs` 直接依赖 `crossterm` 并包含 `translate_key`。这破坏 `protocol -> std` 边界，也会让未来 GUI、远程前端或测试前端被迫带上终端依赖。

**Files:**
- Modify: `src/protocol/key_event.rs`
- Create: `src/terminal/key_translate.rs`
- Modify: `src/terminal/mod.rs`
- Modify: `src/terminal/input.rs`
- Test: `src/terminal/key_translate.rs`

**Interfaces:**
- Produces: `terminal::key_translate::translate_key(crossterm::event::KeyEvent) -> protocol::key_event::KeyEvent`
- Consumes: `Input::map_event` calls the terminal-level translator

**Steps:**
- [ ] 新建 `src/terminal/key_translate.rs`，移动 `translate_modifiers` 和 `translate_key`。
- [ ] `protocol::key_event` 只保留 `KeyModifiers`、`ArrowKey`、`KeyCode`、`KeyEvent` 及中立构造器。
- [ ] 更新 `src/terminal/input.rs` 引用 `crate::terminal::key_translate::translate_key`。
- [ ] 将原 `protocol::key_event` 中按键翻译测试迁移到 `terminal::key_translate`。
- [ ] 增加边界检查：用 `rg "crossterm" src/protocol` 确认无命中。
- [ ] 运行 `cargo test terminal::key_translate protocol::key_event terminal::input`。
- [ ] 运行 `cargo clippy --all-targets --all-features`。

**Acceptance Criteria:**
- `src/protocol` 不再引用 `crossterm`。
- 现有 Ctrl、Shift、Arrow、Function、普通字符翻译行为保持不变。

---

### Task 3: 收窄 ContentHandler 类型探测

**问题：** `ContentHandler` 当前通过 `buffer_mut/as_buffer/as_status_bar` 暴露具体类型。新增 tree view、diagnostics panel、command palette、terminal pane 时会不断增加 `as_xxx`，形成脆弱的手写 RTTI。

**Files:**
- Modify: `src/core/content.rs`
- Modify: `src/core/buffer.rs`
- Modify: `src/core/status_bar.rs`
- Modify: `src/app/content.rs`
- Modify: `src/app/mod.rs`
- Test: `src/app/mod.rs`
- Test: `src/core/content.rs`

**Interfaces:**
- Candidate A: `ContentHandler::text_buffer(&self) -> Option<&Buffer>` and `text_buffer_mut(&mut self) -> Option<&mut Buffer>` as an explicit temporary bridge.
- Candidate B: introduce content capabilities, for example `ContentHandler::query(&self, request: ContentRequest) -> ContentResponse`.
- Preferred first step: choose the smallest bridge that removes `as_status_bar` growth while preserving current behavior.

**Steps:**
- [ ] 先写短设计说明，明确 `Buffer` 是 editable text content，`StatusBar` 是 derived UI content。
- [ ] 给 `StatusBar` 保持通过 `ContentLookup` 派生状态的能力，但避免让 `AppQuery` 直接知道每种 content 类型。
- [ ] 将 `ContentQuery::status_bar` 的实现改为向 content 自身请求 status-bar 数据，非 status-bar content 返回默认空数据。
- [ ] 保留 `buffer_mut` 或等价 editable-text 能力用于 `execute_text_command`，不要一次性大改所有编辑路径。
- [ ] 增加测试：新增一个非 buffer、非 status-bar 的 fake content，确认 `ContentQuery` 不 panic 且返回默认数据。
- [ ] 运行 `cargo test core::content app`。
- [ ] 运行 `cargo clippy --all-targets --all-features`。

**Acceptance Criteria:**
- `AppQuery` 不再需要随着每个 UI content 类型新增 `as_xxx` 分支。
- 文本编辑路径仍能明确拿到 editable buffer，不牺牲类型安全。

---

### Task 4: 拆分 Buffer 与 Mode Runtime 职责

**问题：** `Buffer` 同时负责 rope 文本、文件路径、modified/status、selection 编辑原语、keymap/mode runtime。短期可用，长期会阻碍 Vim operator-pending、visual mode、用户配置、宏录制、多 buffer 共享 keymap。

**Files:**
- Create: `src/core/modes.rs` or extend `src/core/mode.rs`
- Modify: `src/core/buffer.rs`
- Modify: `src/core/content.rs`
- Modify: `src/app/dispatcher.rs`
- Test: `src/core/buffer.rs`
- Test: `src/app/dispatcher.rs`

**Interfaces:**
- Produces: a dedicated mode runtime type, for example `ModeRuntime`, with:
  - `fn resolve_key(&self, key: KeyEvent) -> Option<Command>`
  - `fn handle_mode_command(&mut self, mode: ModeId, action: ModeActionId)`
- Consumes: `Buffer` or editable content owns a runtime initially, but text storage methods no longer contain Vim mode implementation details.

**Steps:**
- [ ] 第一步只做移动，不改变行为：把 `BufferModes`、`VimMode`、`PlainEditMode` 和相关 keymap 构造函数移到独立模块。
- [ ] `Buffer` 保留一个 `modes: ModeRuntime` 字段，保证现有 public 行为不变。
- [ ] 更新测试引用路径，确保 vim normal/insert、Escape、Shift+Arrow 行为不变。
- [ ] 运行 `cargo test core::buffer app::dispatcher`。
- [ ] 第二步再评估是否把 mode runtime 上移到 `View` 或 `App`，不要和纯移动混在一个提交里。

**Acceptance Criteria:**
- `buffer.rs` 主要表达文本和 selection 编辑，不再包含大量 Vim mode 具体实现。
- 行为完全保持：默认 normal mode，`i` 进入 insert，Escape 返回 normal，insert 模式普通字符输入。

---

### Task 5: 建立 Scene Mutation 与 View Lifecycle

**问题：** `views` 目前只在初始化时由 `build_views` 生成。后续 split/panel/overlay 需要同时更新 `scene_builder`、`scene`、`views`、`focused`，如果没有统一入口，状态很容易不一致。

**Files:**
- Modify: `src/app/mod.rs`
- Create: `src/app/scene_state.rs` or `src/app/layout.rs`
- Modify: `src/protocol/scene.rs`
- Test: `src/app/mod.rs`

**Interfaces:**
- Produces: app-level method such as:
  - `fn rebuild_scene_snapshot(&mut self) -> io::Result<()>`
  - `fn attach_view_for_host(&mut self, space: SpaceId, content: ContentId)`
  - later: `fn split_focused(&mut self, axis: Axis) -> io::Result<()>`
- Consumes: existing `SceneBuilder::snapshot`, `View::new(content)`, and `SpaceKind::Host`.

**Steps:**
- [ ] 新增一个 app 内部 helper，集中处理 host space 到 `View` 的同步。
- [ ] 将 `App::new` 中的 `build_views` 迁移到该 helper，保持现有初始化行为。
- [ ] 增加测试：动态创建新的 host space 后调用同步 helper，断言 `views` 中出现对应 `View` 且原 view 不丢失。
- [ ] 增加测试：重复同步不会覆盖已有 view 的 selection。
- [ ] 运行 `cargo test app`。
- [ ] 后续 split 功能必须通过该 helper 更新 scene/view/focus，不允许测试里手工插 `views` 作为生产模式。

**Acceptance Criteria:**
- scene 的 host space 与 app 的 `View` 生命周期有统一维护入口。
- 为 split/panel 做好了最小但可靠的状态同步基础。

---

## 后续拆解建议

这些任务应逐条解决，不建议一次性混改：

1. 先做 Task 1，因为它是明显 bug 型架构问题，且测试边界清晰。
2. 再做 Task 2，因为它是依赖边界问题，影响面有限但能让协议层变干净。
3. Task 3 和 Task 4 都涉及 core/app 边界，建议分别做设计确认后再改。
4. Task 5 应在实现 split 前完成，否则 split 会把临时状态维护方式固化下来。

## Verification Summary

每个任务完成时至少记录：

- 实际运行的测试命令。
- 是否运行 `cargo clippy --all-targets --all-features`。
- 是否存在未覆盖风险。
- 是否修改了 public API 或跨层依赖方向。

