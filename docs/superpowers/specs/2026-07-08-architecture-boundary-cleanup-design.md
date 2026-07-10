# 架构边界清理设计

日期：2026-07-08

## 1. 背景

当前架构已经支持 scene 驱动前端、pull 渲染、selection 模型和
View 实体归属，但仍有三个边界问题：

1. `Frontend` trait 定义在 `app` 层，`tui` 通过
   `use crate::app::Frontend` 实现它，形成 `tui -> app` 的反向依赖。
   同时 `FrontendImpl` 枚举在 `app` 层直接枚举 TUI 类型，使抽象层和
   具体实现耦合。
2. `build_editor_scene` 内部创建局部 `SceneBuilder`。后续动态创建
   split、panel、overlay、minibuffer 等 space 时，如果继续创建局部
   builder，会重置 `SpaceId` 分配，破坏全局唯一和连续分配语义。
3. `CtrlKey` 只支持 `Q`/`S`，`Shift` 也只表达方向键。协议无法表示
   `Ctrl+x`、`Ctrl+Left`、`Ctrl+F1`、`Ctrl+Shift+Left` 等通用修饰键
   组合，导致 keymap 扩展时必须继续膨胀枚举。

本设计目标是一次性修正这些边界，而不改变编辑器的用户可见行为。

## 2. 目标

- 新增独立 `frontend` 层，`app` 和 `tui` 都依赖它，`tui` 不再依赖
  `app`。
- 使用静态分发：`App<F: Frontend>` 持有具体前端，不引入
  `Box<dyn Frontend>`。
- 删除 `FrontendImpl` 和全局 `HeadlessFrontend`。
- `App` 长期持有唯一 `SceneBuilder`，后续所有 `SpaceId` 都由它分配。
- `SceneBuilder` 能从当前节点集合生成渲染用 `Scene`，但不消耗自身。
- `KeyEvent` 改成通用 `KeyCode + KeyModifiers` 模型。
- 保持现有保存、退出、普通输入、方向键、selection 扩展和取消选区
  行为不变。

## 3. 非目标

- 不引入 GUI、远程前端或 Cargo feature。
- 不把 `tui` 完全改造成只依赖 `protocol` 的纯 painter。本次只移除
  `tui -> app` 依赖。
- 不实现新的 split/panel/overlay 用户功能，只为后续动态 space 创建
  修正 ID 分配基础。
- 不扩展 Unicode 文本输入语义。`KeyCode::Char(char)` 为协议铺路，
  但输入路径仍按当前终端支持范围迁移。
- 不保留全局 headless 前端类型。测试需要时使用测试模块内局部 fake
  frontend。

## 4. 模块结构

目标结构：

```text
src/
  frontend/
    mod.rs          Frontend trait

  app/
    mod.rs          App<F: Frontend>，持 scene_builder + scene + frontend
    dispatcher.rs   keymap 分发，使用新 KeyEvent
    executor.rs
    view.rs
    content.rs
    frontend.rs     删除

  tui/
    tui_frontend.rs impl frontend::Frontend for TuiFrontend<W>
    headless.rs     删除
    scene_renderer.rs

  protocol/
    key_event.rs    KeyModifiers + KeyCode + KeyEvent
    scene.rs        SceneBuilder 可 snapshot，不再必须 consume
```

目标依赖方向：

```text
frontend -> protocol
app      -> frontend + core + protocol
tui      -> frontend + terminal + protocol + core
main     -> app + tui + terminal
```

`main.rs` 是唯一接线层，同时依赖 `app` 和 `tui`。`app` 不引用
`crate::tui`，`tui` 不引用 `crate::app`。

## 5. Frontend 抽象

新增 `src/frontend/mod.rs`：

```rust
use std::io;

use crate::protocol::content_query::ContentQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::SpaceId;
use crate::protocol::scene::Scene;

pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;

    fn render(
        &mut self,
        scene: &Scene,
        query: &dyn ContentQuery,
        focused: SpaceId,
    ) -> io::Result<()>;
}
```

`src/app/frontend.rs` 删除。`tui::TuiFrontend<W>` 改为实现
`crate::frontend::Frontend`：

```rust
impl<W: io::Write> Frontend for TuiFrontend<W> {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        self.input.next_event().await
    }

    fn render(
        &mut self,
        scene: &Scene,
        query: &dyn ContentQuery,
        focused: SpaceId,
    ) -> io::Result<()> {
        self.renderer.render(scene, query, focused, &mut self.output as &mut dyn Canvas)
    }
}
```

`App` 使用静态分发：

```rust
pub struct App<F: Frontend> {
    frontend: F,
    scene_builder: SceneBuilder,
    scene: Scene,
    focused: SpaceId,
    // contents/views/dispatcher/bg channels...
}
```

`main.rs` 构造具体前端并注入：

```rust
let frontend = TuiFrontend::new(Output::new(io::stdout()));
let mut app = App::new(path, width as usize, height as usize, frontend)?;
```

`FrontendImpl` 删除。删除后不存在 trait + enum 双重分发，也不存在 dyn
dispatch 成本。每个具体前端对应一个 `App<F>` 单态化版本。

## 6. HeadlessFrontend 删除与测试替代

删除 `src/tui/headless.rs` 和 `FrontendImpl::Headless`。这会影响原先通过
`HeadlessFrontend` 捕获 VT 字节的 app 集成测试。

替代策略：

- `tui::scene_renderer` 的渲染细节继续用局部 `StubQuery` 和
  `Output<Vec<u8>>` 测试，不需要全局 headless 前端。
- `app` 层需要事件驱动集成测试时，在测试模块内定义局部
  `ScriptedFrontend`：

```rust
struct ScriptedFrontend {
    events: VecDeque<FrontendEvent>,
    renders: usize,
}

impl Frontend for ScriptedFrontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        Ok(self.events.pop_front())
    }

    fn render(
        &mut self,
        _scene: &Scene,
        _query: &dyn ContentQuery,
        _focused: SpaceId,
    ) -> io::Result<()> {
        self.renders += 1;
        Ok(())
    }
}
```

这样测试前端仍然可脚本驱动，但不会作为生产模块或跨层 API 保留。

## 7. SceneBuilder 生命周期

当前 `SceneBuilder::finish(self, root, size)` 消耗 builder，且
`build_editor_scene` 内部创建局部 builder。本设计改为：

- `App` 持有唯一 `SceneBuilder`。
- `SceneBuilder` 持有所有已分配 nodes 和 `next_id`。
- 生成 `Scene` 的 API 不消耗 builder。
- 所有后续 space 创建必须通过 `App.scene_builder`。

建议 API：

```rust
impl SceneBuilder {
    pub fn snapshot(&mut self, root: SpaceId, size: Size) -> Result<Scene, BuildError>;

    pub fn host_grow(&mut self, content: ContentId, weight: u32) -> SpaceId;
    pub fn host_fixed(&mut self, content: ContentId, size: i32) -> SpaceId;
    pub fn container_grow(
        &mut self,
        arrangement: Arrangement,
        children: Vec<SpaceId>,
        weight: u32,
    ) -> SpaceId;
}
```

`snapshot` 命名强调它生成当前 builder 状态的渲染快照，不重置
`next_id`，也不放弃 nodes 所有权。`snapshot` 仍负责原有构建校验：

- root 必须存在。
- children 不能悬空。
- 从 root 可达图不能有环。
- 回填 child parent。

`Scene` 继续作为渲染/查询视图：

```rust
pub struct Scene {
    pub root: SpaceId,
    pub size: Size,
    nodes: HashMap<SpaceId, SpaceNode>,
}
```

`Scene` 不承担分配新 ID 的职责。后续如果需要动态更新 scene，应通过
builder 创建/调整节点，再重新 `snapshot`。

### 7.1 标准 editor scene

无参自建 builder 的 `build_editor_scene(width, height, editor, status)` 删除
或替换为接收 builder 的 helper。推荐保留接收 builder 的 helper，减少
测试和初始化重复：

```rust
pub fn build_editor_scene(
    builder: &mut SceneBuilder,
    width: i32,
    height: i32,
    editor: ContentId,
    status: ContentId,
) -> Result<(Scene, SpaceId), BuildError> {
    let editor_space = builder.host_grow(editor, 1);
    let status_space = builder.host_fixed(status, 1);
    let root = builder.container_grow(
        Arrangement::Flex {
            direction: Axis::Vertical,
            gap: 0,
            align: Align::Stretch,
        },
        vec![editor_space, status_space],
        1,
    );
    let scene = builder.snapshot(root, Size { width, height })?;
    Ok((scene, editor_space))
}
```

该 helper 不拥有 builder，不会重置 `next_id`。

## 8. KeyEvent 通用 modifier 模型

删除 `CtrlKey`。`KeyEvent` 从枚举改为结构体：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Arrow(ArrowKey),
    Backspace,
    Enter,
    Escape,
    Function(u8),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}
```

辅助构造函数：

```rust
impl KeyModifiers {
    pub fn none() -> Self;
    pub fn ctrl() -> Self;
    pub fn shift() -> Self;
    pub fn alt() -> Self;
    pub fn ctrl_shift() -> Self;
}

impl KeyEvent {
    pub fn plain(code: KeyCode) -> Self;
    pub fn char(c: char) -> Self;
    pub fn ctrl(c: char) -> Self;
    pub fn arrow(arrow: ArrowKey) -> Self;
    pub fn shift_arrow(arrow: ArrowKey) -> Self;
    pub fn modified(code: KeyCode, modifiers: KeyModifiers) -> Self;

    pub fn is_plain_char(&self) -> Option<char>;
}
```

迁移示例：

```text
KeyEvent::Char(b'a')              -> KeyEvent::char('a')
KeyEvent::Ctrl(CtrlKey::Q)        -> KeyEvent::ctrl('q')
KeyEvent::Ctrl(CtrlKey::S)        -> KeyEvent::ctrl('s')
KeyEvent::Arrow(ArrowKey::Left)   -> KeyEvent::arrow(ArrowKey::Left)
KeyEvent::Shift(ArrowKey::Left)   -> KeyEvent::shift_arrow(ArrowKey::Left)
KeyEvent::Enter                   -> KeyEvent::plain(KeyCode::Enter)
KeyEvent::Escape                  -> KeyEvent::plain(KeyCode::Escape)
KeyEvent::Backspace               -> KeyEvent::plain(KeyCode::Backspace)
```

### 8.1 translate_key 规则

`translate_key` 从 crossterm key event 生成 `KeyEvent`：

- ASCII printable + no modifiers -> `KeyEvent::char(c)`。
- `Ctrl+任意 ASCII 字符` -> `KeyEvent::modified(KeyCode::Char(c), ctrl)`。
- Arrow / Enter / Backspace / Escape / Function 保留 `KeyCode`，并映射
  crossterm modifiers 到 `KeyModifiers`。
- 不支持或不可表达的 crossterm key -> `KeyCode::Unknown`，但仍保留
  modifiers。
- `Shift+char` 按 crossterm 实际事件表达，不额外发明大小写规则。许多
  终端已经把 `Shift+a` 表达为 `Char('A')`。

### 8.2 Keymap 和 dispatcher 影响

`KeyEvent` 仍实现 `Eq + Hash`，所以 `HashMap<KeyEvent, KeyBinding>` 继续
可用。默认全局绑定改为：

```rust
km.bind(KeyEvent::ctrl('q'), Operation::Quit);
km.bind(KeyEvent::ctrl('s'), Operation::Save);
```

selection 扩展绑定改为：

```rust
km.bind(KeyEvent::shift_arrow(ArrowKey::Left), Operation::SelectionExtendLeftBy(1));
```

默认文本输入从匹配 `KeyEvent::Char(c)` 改为：

```rust
if let Some(c) = key.is_plain_char() {
    return Some(Operation::InsertText(c.to_string()));
}
```

dispatcher 的前缀状态机不需要改变，只需要使用新的 `KeyEvent` 值查表。

## 9. 实施顺序

1. 新增 `src/frontend/mod.rs`，迁移 `Frontend` trait。
2. 泛型化 `App<F: Frontend>`，`main.rs` 注入 `TuiFrontend`。
3. 删除 `FrontendImpl` 和 `HeadlessFrontend`，用测试内局部 fake frontend
   替代。
4. 调整 `SceneBuilder` 为长期 builder，新增 `snapshot`，让
   `build_editor_scene` 接收 `&mut SceneBuilder`。
5. 将 `App` 初始化改为创建并持有唯一 `SceneBuilder`。
6. 改 `KeyEvent` 协议、`translate_key`、keymap 绑定、dispatcher 默认输入
   逻辑和相关测试。
7. 更新 `docs/design/current-architecture.md` 和 `AGENTS.md` 中的架构边界
   说明。

这个顺序先切依赖边界，再切 scene 生命周期，最后切按键协议。每一步都
可以通过 `cargo test` 保持仓库可工作。

## 10. 测试策略

必须覆盖：

- `tui` 中不再出现 `use crate::app::Frontend`。
- `app` 中不再引用 `crate::tui` 或 `FrontendImpl`。
- `App<TuiFrontend<Stdout>>` 可由 `main.rs` 正常构造。
- app 测试可用局部 `ScriptedFrontend` 驱动事件。
- 初始 scene 创建后继续 `scene_builder.host_*`，新 ID 大于已有 ID。
- 多次 `snapshot` 不重置 `next_id`。
- `snapshot` 仍拒绝未知 root、悬空 child 和环。
- `Ctrl+q` / `Ctrl+s` 仍触发 Quit / Save。
- `Ctrl+x` 能被翻译为 ctrl 字符事件，而不是 `Unknown`。
- `Ctrl+Left` 和 `Ctrl+F1` 能保留 ctrl modifier。
- `Shift+Arrow` 仍触发现有 selection 扩展绑定。
- 普通字符输入、Enter、Backspace、Escape 行为保持。

验证命令：

```text
cargo fmt
cargo test
cargo clippy --all-targets --all-features
git diff --check
```

## 11. 风险与缓解

- **App 泛型化导致测试签名变化**：测试辅助函数需要返回
  `App<ScriptedFrontend>`。这是局部迁移，边界清晰。
- **删除 HeadlessFrontend 降低 app 渲染字节断言覆盖**：渲染字节细节
  下沉到 `SceneRenderer` 单测；app 层只断言事件驱动后的状态和 render
  调用次数。
- **SceneBuilder snapshot clone 成本**：当前 scene 很小，可接受。未来大
  scene 可优化为更细粒度增量更新，但不影响本次接口语义。
- **KeyEvent 改动面大**：通过构造函数降低迁移噪声，并用全量测试覆盖
  Quit/Save/selection/普通输入。
- **Ctrl/Shift 终端兼容性差异**：协议保留 modifiers；具体终端能否上报
  某些组合由 crossterm/终端决定，不在本层伪造。

## 12. 成功标准

- `app` 和 `tui` 都依赖 `frontend`，但二者互不依赖。
- 无 `FrontendImpl`、无全局 `HeadlessFrontend`。
- `App` 静态分发持有具体前端，无 dyn dispatch。
- `App` 持有唯一 `SceneBuilder`，新 space ID 不会因 helper 局部 builder
  重置。
- keymap 可绑定通用 modifier key，包括 `Ctrl+任意字符`、`Ctrl+Arrow`、
  `Ctrl+Function` 和 `Shift+Arrow`。
- 全量测试、clippy 和空白检查通过。
