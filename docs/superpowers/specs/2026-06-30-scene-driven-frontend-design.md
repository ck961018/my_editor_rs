# Scene 驱动前端层替换设计（v0.2）

> 本 spec 把 `docs/design/layout_design.md`（远期目标架构）落地为对 v0.1 前端层的可实施重构。
> 范围：替换 `Frontend`/`TuiFrontend`/旧 `Renderer`，引入 `Scene`/`Space`/`ResolvedScene`/Taffy 驱动的渲染管线；`Editor`/`Buffer`/`Cursor`/`Status` 核心保留。

## 1. 决策摘要

| 维度 | 决策 |
|---|---|
| 实现范围 | 替换 v0.1 前端层（`Frontend` trait / `TuiFrontend` / 旧 `Renderer`） |
| 布局引擎 | 引入 Taffy + `i32↔f32` adapter（对齐 layout_design §6） |
| 重构深度 | 前端层 + Scene 驱动；`Editor`/`Buffer` 薄包装进 `ContentStore`；`App` 主循环改调新渲染；`protocol` 保留 |
| 落地策略 | Big bang 一次性替换 |
| 单位 | 核心模型全程 `i32`（对齐修订后的 layout_design） |

## 2. 架构总览

Big bang 替换。删除 `Frontend` trait / `TuiFrontend` / 旧 `Renderer`，新增 `layout/` 子系统。`Editor`/`Buffer`/`Cursor`/`Status` 核心保留，薄包装进 `ContentStore`。`protocol`（`FrontendEvent`/`KeyEvent`）保留；`CorePatch` 保留但 App 不再依赖（每帧全量重算 `ResolvedScene`，符合 layout_design §5）。

```
App
 ├─ editor_state: EditorState   (包 Editor + Viewport，作为 Text Content 状态)
 ├─ store: ContentStore         (持有 EditorState)
 ├─ scene: Scene                (root: Vertical [editor Grow(1), status Fixed(1)])
 ├─ engine: TaffyEngine         (i32↔f32 adapter)
 ├─ renderer: TuiRenderer<W>    (impl Renderer，经 store 取数据)
 ├─ input: Input                (保留 terminal/input)
 └─ guard: TerminalGuard        (保留 terminal/lifecycle)
```

数据流：

```
Scene + Store → TaffyEngine::layout → ResolvedScene → render(scene, store, renderer) → Output
```

v0.1 实际是两区域布局：文本区（`Grow`）+ 状态栏（`Fixed(1)`，最后一行）。重构后 Scene：

```
root: Vertical, align: Stretch
  editor: Host(Text)   Grow(1)
  status: Host(StatusBar) Fixed(1)
```

## 3. 模块结构

```
src/
  layout/              新增
    mod.rs
    ids.rs             SceneId / SpaceId / ContentId
    space.rs           Space / SpaceKind / Arrangement / Axis / Align / Sizing / Layer
    scene.rs           Scene / SceneBuilder / Size / Rect / Point / SpaceNode
    content.rs         Content / ContentKind / ContentState trait / ContentStore / EditorState
    resolved.rs        ResolvedScene / RenderItem / render()
    taffy_engine.rs    TaffyEngine (i32↔f32 adapter)
  tui/
    renderer.rs        重写为 TuiRenderer: impl Renderer
    viewport.rs        保留（移入 EditorState）
  core/                保留 editor/buffer/cursor/status
  protocol/            保留 frontend_event/key_event；core_patch 保留但 App 不用
  terminal/            保留 output/lifecycle/input
  app.rs               重写主循环
  frontend.rs          删
  tui/tui_frontend.rs  删
```

## 4. 核心类型

### 4.1 ids.rs

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SceneId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SpaceId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ContentId(pub u64);
```

### 4.2 space.rs

```rust
pub struct Space {
    pub id: SpaceId,
    pub name: Option<String>,
    pub kind: SpaceKind,
    pub sizing: Sizing,
    pub layer: Layer,
}

pub enum SpaceKind {
    Container { arrangement: Arrangement, children: Vec<SpaceId> },
    Host { content: ContentId },
}

pub enum Arrangement {
    Flex { direction: Axis, gap: i32, align: Align },
}

pub enum Axis { Horizontal, Vertical }

pub enum Align { Stretch, Start, Center, End }

pub enum Sizing {
    Fixed(i32),
    Grow(u32),
}

#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    Base = 0,
    Overlay = 10,
    Modal = 20,
    Debug = 100,
}

// 手动 impl Ord/PartialOrd by discriminant，避免 derive 按声明顺序的脆弱性。
impl PartialOrd for Layer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Layer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as i32).cmp(&(*other as i32))
    }
}
```

### 4.3 scene.rs

```rust
pub struct Size { pub width: i32, pub height: i32 }
pub struct Rect { pub x: i32, pub y: i32, pub width: i32, pub height: i32 }
pub struct Point { pub x: i32, pub y: i32 }

impl Rect {
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.x + self.width
            && p.y >= self.y && p.y < self.y + self.height
    }
    pub fn intersect(&self, other: &Rect) -> Option<Rect> { /* 标准矩形相交 */ }
}

pub struct SpaceNode {
    pub id: SpaceId,
    pub parent: Option<SpaceId>,
    pub children: Vec<SpaceId>,
    pub space: Space,
}

pub struct Scene {
    pub root: SpaceId,
    pub size: Size,
    pub focused: Option<ContentId>,
    nodes: HashMap<SpaceId, SpaceNode>,
}
```

`Scene` 对外暴露 `SceneBuilder` 构造，`finish` 时校验不变量（§8）。`Scene::resize(width, height)` 更新 `size`。

### 4.4 content.rs

```rust
pub struct Content { pub id: ContentId, pub kind: ContentKind }

pub enum ContentKind {
    Text,
    StatusBar,
    Terminal,
    Tree,
    Inspector,
    Panel,
    Custom(String),
}

pub trait ContentState {
    fn kind(&self) -> ContentKind;
}

/// v0.1 务实处理：单个 Editor 状态服务两个 ContentId（Text + StatusBar）。
/// ContentId 作为渲染分支标签决定画 buffer 还是 status_line。
pub struct ContentStore {
    editor: EditorState,
    editor_content: ContentId,   // Text
    status_content: ContentId,   // StatusBar
}

pub struct EditorState {
    editor: Editor,
    viewport: Viewport,
}

impl ContentState for EditorState {
    fn kind(&self) -> ContentKind { ContentKind::Text }
}

impl EditorState {
    pub fn editor(&self) -> &Editor { &self.editor }
    pub fn editor_mut(&mut self) -> &mut Editor { &mut self.editor }
    pub fn viewport(&self) -> &Viewport { &self.viewport }
    pub fn viewport_mut(&mut self) -> &mut Viewport { &mut self.viewport }
}

impl ContentStore {
    pub fn new(editor: Editor, viewport: Viewport, editor_content: ContentId, status_content: ContentId) -> Self { ... }
    pub fn editor_state(&self) -> &EditorState { &self.editor }
    pub fn editor_state_mut(&mut self) -> &mut EditorState { &mut self.editor }
    pub fn content_kind(&self, id: ContentId) -> ContentKind {
        if id == self.editor_content { ContentKind::Text }
        else if id == self.status_content { ContentKind::StatusBar }
        else { ContentKind::Text } // 不应发生
    }
    pub fn handle_event(&mut self, ev: FrontendEvent, patches: &mut PatchList) -> io::Result<()> {
        // 委托 Editor::handle_event；光标移动后 recompute viewport
    }
}
```

`Viewport` 滚动跟随光标：`handle_event` 后用 `editor.cursor()` 与当前 `viewport`/`Scene.size` 重算 `top_row`/`left_col`，确保光标可见。逻辑移自旧 `TuiFrontend`。

### 4.5 resolved.rs

```rust
pub struct RenderItem {
    pub space: SpaceId,
    pub parent: Option<SpaceId>,
    pub content: Option<ContentId>,
    pub rect: Rect,
    pub clip: Option<Rect>,
    pub layer: Layer,
    pub z_index: i32,
    pub order: u64,
}

pub struct ResolvedScene {
    pub items: Vec<RenderItem>,
}

pub trait Renderer {
    fn draw_content(
        &mut self,
        content: ContentId,
        store: &ContentStore,
        rect: Rect,
        clip: Option<Rect>,
    );
    fn flush(&mut self) -> io::Result<()>;
}

pub fn render(scene: &ResolvedScene, store: &ContentStore, renderer: &mut dyn Renderer) {
    let mut items = scene.items.clone();
    items.sort_by_key(|i| (i.layer, i.z_index, i.order));
    for item in items {
        if let Some(content) = item.content {
            renderer.draw_content(content, store, item.rect, item.clip);
        }
    }
}
```

## 5. Taffy adapter 与布局管线

`Cargo.toml` 新增依赖 `taffy`（版本在实现计划中固定）。

```rust
pub struct TaffyEngine {
    tree: taffy::TaffyTree,
}

impl TaffyEngine {
    pub fn layout(&mut self, scene: &Scene, store: &ContentStore) -> ResolvedScene {
        // 1. DFS 遍历 Space 树，建 Taffy 节点
        //    Container -> flex node（direction/gap/align_items/sizing 映射）
        //    Host      -> Taffy leaf node（v0.1 不挂 measure：editor Grow 填满，status Fixed(1)）
        // 2. root 用 Scene.size（i32 as f32），忽略 root 自身 Sizing（layout_design §12.7）
        // 3. tree.compute_layout(root_node, Scene.size as f32)
        // 4. 收集 i32 Rect：taffy f32 layout location/size -> round 为 i32
        // 5. clip = rect ∩ parent.clip（根 parent=None -> clip=None）
        // 6. order = DFS 前序遍历递增计数
        // 7. 组装 RenderItem { space, parent, content, rect, clip, layer, z_index:0, order }
    }
}
```

映射：

```
Arrangement::Flex { direction } -> taffy FlexDirection (Horizontal=Row, Vertical=Column)
Arrangement::Flex { gap }        -> taffy gap
Arrangement::Flex { align }      -> taffy align_items (Stretch/Start/Center/End)
Sizing::Fixed(x)                 -> taffy length (x as f32)
Sizing::Grow(x)                  -> taffy flex_grow (x as f32)
SpaceKind::Host                  -> taffy leaf node
```

`MeasureFunc` v0.1 不实现（layout_design §3.7 留作扩展点）。若 Taffy leaf 无 measure 且无显式尺寸，按 `Grow`/父约束分配，满足 v0.1 需求。

## 6. TuiRenderer 投影

```rust
pub struct TuiRenderer<W: Write> {
    output: Output<W>,
}

impl<W: Write> Renderer for TuiRenderer<W> {
    fn draw_content(&mut self, content: ContentId, store: &ContentStore, rect: Rect, _clip: Option<Rect>) {
        match store.content_kind(content) {
            ContentKind::Text      => self.draw_editor(store.editor_state(), rect),
            ContentKind::StatusBar => self.draw_status(store.editor_state(), rect),
            _ => {}
        }
    }
    fn flush(&mut self) -> io::Result<()> { self.output.flush() }
}
```

- `draw_editor(state, rect)`：`hide_cursor`；对 `row in 0..rect.height` 画 `buffer.line(viewport.top_row + row)` 到 `rect.y + row`；光标定位到 `(rect.y + cursor.row - viewport.top_row, rect.x + cursor.col - viewport.left_col)`；`show_cursor`。
- `draw_status(state, rect)`：`move_cursor(rect.y, rect.x)`；`clear_line`；`write_str(status_line(editor))`。
- 整数 rect = cell，零转换（layout_design §9）。
- `status_line` 复用 v0.1 现有实现。

渲染顺序：`render()` 按 `(layer, z_index, order)` 排序后逐项绘制。v0.1 全 `Layer::Base`，`order` 由 DFS 决定（root → editor → status）。

## 7. App 主循环

```rust
pub struct App {
    store: ContentStore,
    scene: Scene,
    engine: TaffyEngine,
    renderer: TuiRenderer<BufWriter<Stdout>>,
    input: Input,
    guard: TerminalGuard,
}

impl App {
    pub fn new(path: Option<&str>) -> io::Result<Self> { /* open_path + 建 Scene + 初始化 */ }

    pub async fn run(&mut self) -> io::Result<()> {
        self.render()?;
        while !self.store.editor_state().editor().should_quit() {
            let ev = self.input.next_event().await?;
            if let FrontendEvent::Resize(r) = &ev {
                self.scene.resize(r.width as i32, r.height as i32);
            }
            let mut patches = PatchList::new();
            self.store.handle_event(ev, &mut patches)?;  // 委托 Editor + viewport 跟随
            self.render()?;                                // 全量重算 ResolvedScene
        }
        Ok(())
    }

    fn render(&mut self) -> io::Result<()> {
        let resolved = self.engine.layout(&self.scene, &self.store);
        crate::layout::resolved::render(&resolved, &self.store, &mut self.renderer);
        self.renderer.flush()
    }
}
```

`CorePatch` 仍由 `Editor::handle_event` 产出，App 忽略（全量重算）。`FrontendEvent` 保留（Key/Resize/QuitRequest），`Resize` 更新 `Scene.size` 后下一帧 `TaffyEngine` 重算。

## 8. 不变量（SceneBuilder 构建期校验）

`SceneBuilder::finish(root)` 校验，非运行时：

1. root 指向有效 Space。
2. Space Tree 无环（DFS 检测）。
3. Container 不持 Content（类型保证：`SpaceKind::Container` 无 `content` 字段）。
4. Host 不持 children（类型保证）。
5. Content 不拥有 Space（`ContentStore` 持状态，Content 无 Space 引用）。
6. Layer 不参与空间分配（Taffy 只读 layer 用于排序，不传入布局）。
7. Root Sizing 忽略（`TaffyEngine::layout` 对 root 用 `Scene.size`，忽略 root.sizing）。

## 9. 不回退保证

v0.1 全部行为保留：
- 打开文件：NotFound→NewFile、非 UTF-8→OpenFailed、正常→None。
- 编辑：Char / Enter / Backspace（含 `char_idx==0` noop）。
- 光标：Left/Right/Up/Down。
- Ctrl+S 保存（Saved/SaveFailed）、Ctrl+Q 退出。
- Resize → 重绘。
- 渲染：文本行 + 最后一行 status_line + 光标定位。

`Editor`/`Buffer`/`Cursor`/`Status` 及其现有单元测试不动，保证核心行为不变。

## 10. 测试

- **保留**：`core/*`（editor/buffer/cursor/status）现有测试全部保留。
- **新增 `layout::ids`**：`SpaceId`/`ContentId` Copy/Eq/Hash。
- **新增 `layout::scene`**：`SceneBuilder` 构造 root: Vertical [editor Grow(1), status Fixed(1)]；`finish` 校验不变量；无环检测对环拒绝。
- **新增 `layout::taffy_engine`**：给定 `Size { 80, 24 }`，`Fixed(1)` status 高 1、`Grow(1)` editor 高 23；`gap` 生效；`Align::Stretch` 交叉轴填满；`order` 为 DFS 前序；`clip` 传递。
- **新增 `tui::renderer`**：假 `Output<Vec<u8>>`，断言 VT 输出含文本行、status_line（`[No Name]`/`row:col`）、光标 MoveTo 序列；多行文本与滚动。
- **新增 `app`**：`ScriptedInput`（保留 v0.1 脚本前端思路，改为脚本事件驱动 `Input`）跑插入 `a` + Ctrl+Q，断言输出含 `a` 且退出。
- **`protocol`**：`CorePatch` 保留但不再被 App 使用，其测试保留。

## 11. 与 layout_design 的对齐

| layout_design 概念 | 本 spec 实现 |
|---|---|
| Scene / Space / Content / ContentStore | §4 全部落地 |
| Layer | 仅 `Layer::Base`，手动 `Ord`（§4.2） |
| Focus | `Scene.focused: Option<ContentId>`，v0.1 单焦点占位（editor），不实现转移 |
| Sizing | `Fixed(i32)`/`Grow(u32)` + `Align`（§4.2） |
| ResolvedScene | `RenderItem` 带 `parent`/`clip`/`order`（§4.5） |
| Renderer | 经 `ContentStore` 取数据（§4.5/§6） |
| Taffy adapter i32↔f32 | §5 |
| MeasureFunc | v0.1 不实现，留扩展点 |
| 整数单位 | 全程 `i32`，TUI 零转换 |
| Portal/Slot/Anchor/多 Arrangement | 不实现（Non-goals） |

## 12. Non-goals

- 不实现 Portal / Slot / Anchor。
- 不实现 Grid / Stack，仅 Flex。
- 不实现 Layer::Overlay/Modal/Debug（仅 Base）。
- 不实现 Focus 转移规则（仅单焦点占位）。
- 不实现 MeasureFunc（v0.1 文本 Host 用 Grow/Fixed，无需内容测量）。
- 不实现多 Content 状态（v0.1 单 Editor，两 ContentId 共享）。
- 不实现增量重算（每帧全量重算 ResolvedScene）。
- 不实现指针事件 / hit_test（v0.1 键盘驱动）。

## 13. 风险

- **Taffy leaf 无 measure 行为**：需在实现时验证 Taffy 对无 measure 的 leaf + `flex_grow` 的尺寸分配符合预期；若不符，回退为给 leaf 显式 `min_size`/`size`。
- **Big bang 中间不可运行**：替换期间存在不可编译的中间态。缓解：按模块自底向上实现（ids → space → scene → content → resolved → taffy_engine → renderer → app），每个模块带测试先通过编译，最后接 App。
- **Viewport 滚动正确性**：从旧 `TuiFrontend` 迁移到 `EditorState`，需保证光标跟随逻辑不回退。
