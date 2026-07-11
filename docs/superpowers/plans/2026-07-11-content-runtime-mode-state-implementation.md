# Content Runtime 与 Mode 状态 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 mode 的可变状态从共享 Buffer 移入按 `SpaceId` 归属的静态 `ContentRuntime`，使同一 Buffer 的多个 View 保持独立 mode 会话。

**Architecture:** `ContentRuntime` 是与 `Content` 一一对应的闭合枚举，View 持有当前 Content 的 runtime。Buffer 暂时仍持有 `ModeSet` 行为和普通 keymap；`ModeSet` 创建并操作不透明的 `ModeRuntime`，Vim 的 `Normal/Insert` 状态从 `VimMode` 移入其中。Mode 命令改为 View 目标，按键前缀机制与渲染查询保持不变。

**Tech Stack:** Rust 2024、标准库、ropey、tokio、现有单 crate 测试框架。

## Global Constraints

- 保持 Rust 2024 与 MSRV 1.85；不得新增依赖。
- `Content` 和 `ContentRuntime` 都是静态闭合集合；不得引入 `Box<dyn ContentRuntime>`、`Box<dyn Content>` 或 App 层具体 Content 类型探测。
- 类型擦除只允许位于 `core::mode` 内部，用于具体 mode 的私有 state；App、View、ContentStore 和 Buffer 不得 downcast 具体 mode state。
- `View` 继续归 App 所有、按 `SpaceId` 保存 selections；runtime 的生命周期也按一次 Space-Content 绑定计算。
- `App<F: Frontend>` 继续泛型静态分发；不得引入 app/tui 依赖。
- 保持按键 capture chain 与 `Dispatcher::pending` 的 prefix 语义；不将 prefix 状态移入 runtime。
- 不修改 `RenderQuery`、`ContentQuery`、状态栏输出或 TUI 渲染行为。
- 保存仍由 App 的 Tokio 任务执行；`ContentEffect::Save` 与 `SaveFinished` 流程不得改变。
- 每个 Rust 改动先写失败测试，再实现；最终运行 `cargo fmt --check`、`cargo test` 和 `cargo clippy --all-targets --all-features`。

---

## File Structure

- Create: `src/core/content_runtime.rs`：静态 `ContentRuntime`、`BufferRuntime` 与空的 `StatusBarRuntime`。
- Modify: `src/core/mode.rs`：不可变 mode 行为、类型擦除 state、Vim 和 plain-edit 实现。
- Modify: `src/core/buffer.rs`：持有 `ModeSet` 和普通 keymap，最终通过外部 `BufferRuntime` 解析和执行 mode。
- Modify: `src/core/content.rs`：创建 runtime、使用 `ContentInput::View`，并为 Content/runtime 配对静态分派。
- Modify: `src/core/content_store.rs`：创建 runtime，并将 runtime 传给 mode key 解析。
- Modify: `src/core/mod.rs`：导出 `content_runtime` 模块。
- Modify: `src/app/view.rs`：持有 `ContentRuntime`，同时借出 selections 与 runtime。
- Modify: `src/app/mod.rs`：创建 View runtime、传入 Dispatcher、以 `ContentInput::View` 执行 View 命令。
- Modify: `src/app/dispatcher.rs`：mode 命令解析为 `ViewContent`，mode fallback 读取 focused View runtime。

## Task 1: 将 Mode 行为与其可变 State 分离

**Files:**
- Modify: `src/core/mode.rs`
- Modify: `src/core/buffer.rs`
- Test: `src/core/mode.rs` inline tests
- Test: `src/core/buffer.rs` inline tests

**Interfaces:**
- Produces `ModeSet`（不可变行为与配置）和 `ModeRuntime`（base mode 的不透明 state）。
- 在本任务结束时，Buffer 暂时持有 `mode_runtime: ModeRuntime` 以保持现有调用方和按键行为可用；Task 3 删除该兼容字段。

- [ ] **Step 1: 写入两个 ModeRuntime 互不影响的失败测试**

在 `src/core/mode.rs` 的测试模块加入：

```rust
#[test]
fn vim_mode_runtime_is_independent() {
    let modes = ModeSet::vim();
    let mut first = modes.create_runtime();
    let second = modes.create_runtime();

    modes.execute(
        &mut first,
        ModeId::new("vim"),
        ModeActionId::new("enter-insert"),
    );

    assert_eq!(
        modes.resolve_key(&first, KeyEvent::char('a')),
        Some(Command::Content(ContentCommand::Edit(
            EditCommand::InsertText("a".to_string())
        )))
    );
    assert_eq!(modes.resolve_key(&second, KeyEvent::char('a')), None);
}
```

- [ ] **Step 2: 运行测试确认新 state API 尚不存在**

Run: `cargo test core::mode::tests::vim_mode_runtime_is_independent`

Expected: FAIL，编译器报告 `ModeSet`、`create_runtime` 或 `execute` 尚不存在。

- [ ] **Step 3: 实现不可变行为与不透明 State**

在 `src/core/mode.rs` 用下列接口替换当前 stateful `Mode`：

```rust
pub trait ModeState: std::any::Any {
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

pub trait Mode {
    fn id(&self) -> ModeId;
    fn new_state(&self) -> Box<dyn ModeState>;
    fn keymap(&self, state: &dyn ModeState) -> &Keymap;
    fn typing(&self, state: &dyn ModeState, key: KeyEvent) -> Option<Command>;
    fn execute(&self, state: &mut dyn ModeState, action: ModeActionId);
}

pub(crate) struct ModeRuntime {
    base: Box<dyn ModeState>,
}

pub(crate) struct ModeSet {
    base: Box<dyn Mode>,
}
```

为任意 `T: Any` 实现 `ModeState` 的两个 downcast 辅助方法。`ModeSet::create_runtime`
调用 `base.new_state()`；`resolve_key` 将 state 借给 base mode 选择 keymap 和 typing；
`execute` 仅在 `ModeId` 匹配 base mode 时转发 action。

将 `PlainEditMode`、`VimMode` 和默认 keymap 构造函数从 `buffer.rs` 移到 `mode.rs`。
让 `VimMode` 只保存 normal/insert keymap，新增私有
`VimModeState { state: VimState }`。仅 `VimMode` 自己 downcast 该 state；
`enter-insert` 与 `enter-normal` 只修改此字段。

将 `BufferModes` 字段替换为：

```rust
modes: ModeSet,
mode_runtime: ModeRuntime,
```

`Buffer::new` 从同一个 `ModeSet` 创建初始 runtime。保留现有
`resolve_key(KeyEvent)` 与 `handle_mode_command(ModeId, ModeActionId)` 方法，分别
委派给 `modes.resolve_key(&mode_runtime, ...)` 和
`modes.execute(&mut mode_runtime, ...)`，以保持 Task 2 前所有调用方可编译。

- [ ] **Step 4: 运行 mode 与 Buffer 测试**

Run: `cargo test core::mode::tests`

Expected: PASS，两个 ModeRuntime 的 Vim 输入状态不同。

Run: `cargo test core::buffer::tests`

Expected: PASS，现有文本编辑、默认 Vim 和 plain-edit 测试保持通过。

- [ ] **Step 5: 提交行为/state 分离**

```text
git add src/core/mode.rs src/core/buffer.rs
git commit -m "refactor: separate mode behavior from state"
```

## Task 2: 引入静态 ContentRuntime 与 View 所有权

**Files:**
- Create: `src/core/content_runtime.rs`
- Modify: `src/core/mod.rs`
- Modify: `src/core/buffer.rs`
- Modify: `src/core/content.rs`
- Modify: `src/core/content_store.rs`
- Modify: `src/app/view.rs`
- Modify: `src/app/mod.rs`
- Test: `src/core/content.rs` inline tests
- Test: `src/core/content_store.rs` inline tests
- Test: `src/app/view.rs` inline tests
- Test: `src/app/mod.rs` inline tests

**Interfaces:**
- Produces `ContentRuntime::{Buffer(BufferRuntime), StatusBar(StatusBarRuntime)}`。
- Produces `Content::create_runtime()`、`ContentStore::create_runtime(ContentId)` 和 `View::new(ContentId, ContentRuntime)`。
- 在本任务结束时，新增 runtime-aware API 与 `ContentInput::View` 已可用；旧 `WithSelections` 和 Buffer 内兼容 runtime 暂时保留，Task 3 一次性删除。

- [ ] **Step 1: 写入静态 runtime 工厂和 View 所有权的失败测试**

在 `src/core/content.rs` 加入：

```rust
#[test]
fn buffer_creates_independent_content_runtimes() {
    let content = Content::Buffer(Buffer::new());
    let mut first = content.create_runtime();
    let second = content.create_runtime();
    let mut selections = Selections::single(Selection::collapsed(CursorPos::origin()));

    let mut content = content;
    content.execute(ContentInput::View {
        command: ContentCommand::Mode {
            mode: ModeId::new("vim"),
            action: ModeActionId::new("enter-insert"),
        },
        selections: &mut selections,
        runtime: &mut first,
    });

    assert!(content.resolve_key_with_runtime(&first, KeyEvent::char('a')).is_some());
    assert!(content.resolve_key_with_runtime(&second, KeyEvent::char('a')).is_none());
}

#[test]
#[should_panic(expected = "content/runtime mismatch")]
fn mismatched_view_runtime_is_an_internal_error() {
    let mut content = Content::Buffer(Buffer::new());
    let mut runtime = ContentRuntime::StatusBar(StatusBarRuntime);
    let mut selections = Selections::single(Selection::collapsed(CursorPos::origin()));

    content.execute(ContentInput::View {
        command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
        selections: &mut selections,
        runtime: &mut runtime,
    });
}

#[test]
fn status_bar_creates_a_status_bar_runtime() {
    let content = Content::StatusBar(StatusBar::new(ContentId(0)));
    assert!(matches!(
        content.create_runtime(),
        ContentRuntime::StatusBar(_)
    ));
}
```

在 `src/app/view.rs` 加入：

```rust
#[test]
fn view_borrows_selections_and_runtime_together() {
    let mut view = View::new(
        ContentId(0),
        ContentRuntime::StatusBar(StatusBarRuntime),
    );
    let (selections, runtime) = view.selections_and_runtime_mut();

    selections.primary_mut().head.char_index = 3;
    assert!(matches!(runtime, ContentRuntime::StatusBar(_)));
}
```

更新 `content_store.rs` 的编辑测试，使其从
`store.create_runtime(id).expect("content exists")` 获得 runtime，并通过
`ContentInput::View` 编辑。

- [ ] **Step 2: 运行测试确认静态 runtime API 尚不存在**

Run: `cargo test core::content::tests::buffer_creates_independent_content_runtimes`

Expected: FAIL，编译器报告 `ContentRuntime`、`create_runtime`、`View` 或
`resolve_key_with_runtime` 不存在。

- [ ] **Step 3: 实现 ContentRuntime、runtime-aware core API 与 View 字段**

创建 `src/core/content_runtime.rs`：

```rust
pub struct BufferRuntime {
    modes: ModeRuntime,
}

pub struct StatusBarRuntime;

pub enum ContentRuntime {
    Buffer(BufferRuntime),
    StatusBar(StatusBarRuntime),
}
```

仅对 `core` 暴露 `BufferRuntime::new`、`modes`、`modes_mut`。向
`src/core/mod.rs` 加入 `pub mod content_runtime;`。

在 Buffer 增加 runtime-aware 的并行入口：

```rust
pub(crate) fn create_runtime(&self) -> BufferRuntime;
pub(crate) fn resolve_key_with_runtime(
    &self,
    runtime: &BufferRuntime,
    key: KeyEvent,
) -> Option<Command>;
pub(crate) fn execute_mode_with_runtime(
    &self,
    runtime: &mut BufferRuntime,
    mode: ModeId,
    action: ModeActionId,
);
```

在 `Content` 实现：

```rust
pub fn create_runtime(&self) -> ContentRuntime;
pub fn resolve_key_with_runtime(
    &self,
    runtime: &ContentRuntime,
    key: KeyEvent,
) -> Option<Command>;
```

并新增 `ContentInput::View { command, selections, runtime }`。匹配的
Buffer + `ContentRuntime::Buffer`：Edit 调用 `apply_edit`，Mode 调用
`execute_mode_with_runtime`；StatusBar 对匹配但不适用的 View 输入返回
`ContentEffect::None`。不匹配变体执行
`panic!("content/runtime mismatch")`。

保留旧 `ContentInput::WithSelections`、无 runtime 的 `resolve_key` 和
`Command(Mode)` 仅作为本次迁移的兼容入口，直到 Task 3 删除。

在 `ContentStore` 增加 `create_runtime` 和 `resolve_key_with_runtime` 转发。

将 View 改为：

```rust
pub struct View {
    content: ContentId,
    selections: Selections,
    runtime: ContentRuntime,
}
```

实现 `runtime()` 与：

```rust
pub fn selections_and_runtime_mut(&mut self) -> (&mut Selections, &mut ContentRuntime);
```

将 `build_views` 和 `collect_content_spaces` 改为接收 `&ContentStore`，并为每个
Content Space 调用 `contents.create_runtime(*content).expect("scene content exists in content store")`。
在 `App::new` 中传入 `&contents`；所有手工构造 View 的测试也从 Store 创建 runtime。

- [ ] **Step 4: 运行新增 core/View 测试和完整回归**

Run: `cargo test core::content::tests`

Expected: PASS，runtime 工厂静态分派，两个 Buffer runtime 状态独立。

Run: `cargo test app::view::tests`

Expected: PASS，View 可同时借出 selections 与 runtime。

Run: `cargo test`

Expected: PASS，兼容入口使现有 Dispatcher 和 App 行为保持不变。

- [ ] **Step 5: 提交静态 ContentRuntime 基础设施**

```text
git add src/core/content_runtime.rs src/core/mod.rs src/core/buffer.rs src/core/content.rs src/core/content_store.rs src/app/view.rs src/app/mod.rs
git commit -m "feat: add static content runtimes"
```

## Task 3: 切换生产路由到 View Runtime 并删除兼容入口

**Files:**
- Modify: `src/core/buffer.rs`
- Modify: `src/core/content.rs`
- Modify: `src/core/content_store.rs`
- Modify: `src/app/dispatcher.rs`
- Modify: `src/app/mod.rs`
- Test: `src/app/dispatcher.rs` inline tests
- Test: `src/app/mod.rs` inline tests

**Interfaces:**
- Consumes `View::runtime()`、`View::selections_and_runtime_mut()` 和 runtime-aware ContentStore API。
- Produces最终的 `ContentInput::View`、`ContentStore::resolve_key(id, runtime, key)` 和 runtime-free Buffer。
- Produces所有 `ContentCommand::Mode` 的 `DispatchCommand::ViewContent` 目标。

- [ ] **Step 1: 写入跨 View mode 隔离的失败集成测试**

在 `src/app/mod.rs` 的测试模块新增
`two_views_of_one_buffer_keep_independent_mode_runtime`。用 `SceneBuilder` 创建两个
指向 `editor_cid()` 的 Content Space：

```rust
let left = builder.content_grow(editor_cid(), 1);
let right = builder.content_grow(editor_cid(), 1);
let root = builder.container_grow(
    Arrangement::Flex {
        direction: Axis::Horizontal,
        gap: 0,
        align: Align::Stretch,
    },
    vec![left, right],
    1,
);
let scene = builder
    .snapshot(root, Size { width: 40, height: 5 })
    .unwrap();
app.scene = scene;
app.views = build_views(&app.scene, &app.contents);
```

测试通过现有 App 输入链路运行左 View 的 `i`、`a`，再把焦点切到右 View 并输入
`a`：

```rust
app.focused = left;
app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
    .await
    .unwrap();
app.handle_event(FrontendEvent::Key(KeyEvent::char('a')))
    .await
    .unwrap();
app.focused = right;
app.handle_event(FrontendEvent::Key(KeyEvent::char('a')))
    .await
    .unwrap();

assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
```

将 `vim_i_resolves_to_content_mode_command` 改名为
`vim_i_resolves_to_view_content_mode_command`，期望：

```rust
DispatchCommand::ViewContent {
    command: ContentCommand::Mode {
        mode: ModeId::new("vim"),
        action: ModeActionId::new("enter-insert"),
    },
    space: focused,
    content: ContentId(0),
}
```

扩展 `production_content_paths_have_no_dynamic_type_probes`，使其读取
`../core/content_runtime.rs`，并以运行时拼接的禁止片段断言 Content runtime 没有动态
Content 分发：

```rust
let content_runtime = include_str!("../core/content_runtime.rs");
let forbidden = [
    ["Box<dyn ", "ContentRuntime>"].concat(),
    ["Box<dyn ", "Content>"].concat(),
];
for fragment in forbidden {
    assert!(!content_runtime.contains(&fragment), "{fragment}");
}
```

- [ ] **Step 2: 运行测试确认旧生产路径仍共享 Buffer state**

Run: `cargo test app::tests::two_views_of_one_buffer_keep_independent_mode_runtime`

Expected: FAIL，右 View 也会把 `a` 解析为插入命令，或 mode 命令仍缺少 `SpaceId`。

- [ ] **Step 3: 删除兼容入口并接通最终执行链**

删除 Buffer 的 `mode_runtime` 字段及旧 `resolve_key(KeyEvent)`、
`handle_mode_command`。将 Task 2 的 runtime-aware Buffer 方法改为最终名称：

```rust
pub(crate) fn resolve_key(&self, runtime: &BufferRuntime, key: KeyEvent) -> Option<Command>;
pub(crate) fn execute_mode(
    &self,
    runtime: &mut BufferRuntime,
    mode: ModeId,
    action: ModeActionId,
);
```

删除 `ContentInput::WithSelections`、无 runtime 的 `Content::resolve_key` 与
`ContentCommand::Mode` 的 `Command` 兼容执行。将最终 ContentStore API 设为：

```rust
pub fn resolve_key(
    &self,
    id: ContentId,
    runtime: &ContentRuntime,
    key: KeyEvent,
) -> Option<Command>;
```

在 `Dispatcher::dispatch` 增加 `runtime: &ContentRuntime` 参数。仅在 focused
content mode fallback 中调用 `contents.resolve_key(cid, runtime, key)`；capture
chain、global keymap、`PendingKeymap` 和 prefix source 保持原样。

在 `resolve_command` 中让 `Edit(_)` 与 `Mode { .. }` 共同调用
`view_content_target` 并生成 `DispatchCommand::ViewContent`；`Save` 保持
`DispatchCommand::Content`。

在 `App::handle_event` 以字段级借用读取 focused View 的 `runtime()` 并传给
Dispatcher。在 `App::execute_command` 的 `ViewContent` 分支中：

```rust
let view = self.views.get_mut(&space).expect("target view exists");
assert_eq!(view.content(), content, "view/content target mismatch");
let (selections, runtime) = view.selections_and_runtime_mut();
let effect = self.contents.execute(
    content,
    ContentInput::View {
        command,
        selections,
        runtime,
    },
);
```

保存和 `SaveFinished` 仍使用 `ContentInput::Command` / `ContentInput::Event`，不借用
View runtime。

更新 dispatcher fixture：从 ContentStore 创建 runtime；进入 Insert 的测试以
`ContentInput::View` 修改局部 runtime 与 selections。

- [ ] **Step 4: 运行完整验证**

Run: `cargo fmt --check`

Expected: PASS。

Run: `cargo test`

Expected: PASS，新增跨 View 测试通过，既有 prefix、保存、selection、RenderQuery 和
TUI 测试保持通过。

Run: `cargo clippy --all-targets --all-features`

Expected: exit code 0；仅允许仓库已有且未由本改动引入的 warning。

Run: `git diff --check`

Expected: exit code 0，无空白错误。

- [ ] **Step 5: 提交最终生产路径**

```text
git add src/core/buffer.rs src/core/content.rs src/core/content_store.rs src/app/dispatcher.rs src/app/mod.rs
git commit -m "refactor: route mode commands through views"
```

## Plan Review Checklist

- 静态 `ContentRuntime`、mode state 类型擦除、View 所有权、View 目标 mode 命令、按键边界、渲染非目标和保存非回归均有对应步骤。
- 所有最终 API 在前置步骤中定义：`ModeSet`、`ModeRuntime`、`ContentRuntime`、`ContentInput::View`、`View::runtime`、`View::selections_and_runtime_mut`、`ContentStore::create_runtime` 和 runtime-aware `resolve_key`。
- 兼容入口只存在于 Task 2，并在 Task 3 明确删除；最终代码不保留共享 Buffer mode runtime。
- 计划不修改用户维护的 `docs/roadmap` 或工作区中已有未提交的 `AGENTS.md`；实现完成后单独报告这些文档是否需要同步。
- 最终验证覆盖格式、完整测试、Clippy 和空白检查。
