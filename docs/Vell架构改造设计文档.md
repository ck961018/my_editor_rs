# Vell 架构改造设计文档

**View、Pane、Content 与可扩展展示模型**

- **设计状态：** 提案（Proposal）
- **版本：** 0.1
- **日期：** 2026-08-06

> **核心主题：** 保留现有 Workspace / Scene / Space 布局模型，将 View 扩展为可拥有多个 Pane 并可递归组合其他 View 的完整展示单元。

## 文档结构

- 1. 背景与问题
- 2. 设计目标与非目标
- 3. 核心概念与架构原则
- 4. View、Pane 与 Content 的关系
- 5. 标准 BufferView 设计
- 6. 复合 View：DiffView 示例
- 7. View 切换语义
- 8. 渲染、事件与查询接口
- 9. TypeScript 扩展模型
- 10. 数据、状态与生命周期
- 11. 核心不变量
- 12. 当前架构迁移方案
- 13. 风险、待决问题与验收标准
- 14. 最终设计决议

## 执行摘要

**本设计的核心判断是：**不应为 gutter、状态栏、正文区等显示区域分别创建 View，也不应再引入 ViewSurface 或 PanePresentation。

View 是完整的展示控制器；Pane 维持当前布局系统中的定义，是 View 的显示区域；Content 是 View 持有或借用的数据来源。

```text
BufferView
├── left gutter Pane
├── body Pane
├── right gutter Pane
├── top bar Pane
└── bottom bar Pane

DiffView
├── Left BufferView
└── Right BufferView
```

**BufferView 内部没有子 View。**其 gutter、bar 和正文都是由同一 BufferView 控制的 Pane。

**DiffView 包含两个 BufferView。**因为左右两侧是两个完整且具有独立状态、数据绑定和生命周期的编辑视图。

| **议题** | **设计结论** | **说明** |
| --- | --- | --- |
| 布局模型 | 保持现状 | 继续使用 Workspace / Scene / Space / Arrangement，不引入第二套布局树。 |
| Pane | 保持现有定义 | Pane 只承担布局与承载；不增加 PaneState、PaneType 或 PanePresentation。 |
| View | 扩展为完整展示单元 | 一个 View 可控制多个 Pane，也可包含其他完整 View。 |
| BufferView | 只拥有 Pane，不含子 View | 正文、gutter、bar 都共享同一 Buffer 与 ViewState。 |
| DiffView | 包含两个 BufferView | 左右侧各自是完整编辑会话，由 DiffView 协调。 |
| 切换目标 | 最近的可切换祖先 | 从焦点 Pane 所属 View 开始向上查找 switchable=true 的 View。 |
| 切换策略 | 实例级 bool 即可 | 不引入 ViewSwitchPolicy；复杂约束留待真实需求出现。 |

## 1. 背景与问题

### 1.1 初始设计的演化

**最初设计中，状态栏被建模为一种 Content，并指向另一个 Content。**这种方案容易实现任意视图组合，但会模糊 Content 的语义：状态栏并不是独立业务数据模型，而是某个展示单元的一个显示区域。

后续将状态栏移动到 View 下，方向更接近正确边界，但当前实现仍把状态栏作为特殊 View 处理，导致 View、Content 和布局之间出现例外路径。

### 1.2 当前模型暴露出的主要问题

- View 被限定为“单个 Content + 单个 ContentViewState + 单个显示位置”，难以表达一个 View 对应多个 Pane。
- 状态栏通过特殊字段、目标 View 或专用 presentation 分支存在，产生大量例外逻辑。
- 同一 View 只能映射到一个 Space 的隐含假设，使 gutter、bar 与正文无法自然共享状态。
- 如果把每个显示区域都提升为 View，会造成 View 粒度过细，父子 View 树充斥无独立生命周期的对象。
- 如果把 Pane 提升为组件或状态所有者，则会复制现有布局概念，并把展示逻辑错误地下沉到 Pane。

> **问题本质：**需要扩大 View 的表达能力，而不是修改 Pane 或重建布局系统。

## 2. 设计目标与非目标

### 2.1 设计目标

- 让一个 View 能够拥有任意数量的 Pane，并根据 Pane 身份决定显示内容。
- 让一个 View 可以递归包含其他完整 View，从而构建 DiffView、DashboardView 等复合展示。
- 保留当前 Workspace / Scene / Space / Arrangement 的布局结构与概念。
- 让 BufferView 以易理解的方式支持左右 gutter 与上下 bar，同时保持这些区域只是普通 Pane。
- 让 TypeScript 插件可以扩展标准 View，也可以定义完全自定义的 View。
- 建立简单、确定的 View 切换规则，支持嵌套 View。
- 消除状态栏的特殊 Content、特殊 View 及特殊状态路径。

### 2.2 非目标

- 不引入 ViewSurface。
- 不引入 PanePresentation、PaneState、PaneType 或 PaneBinding。
- 不修改现有布局节点类型和排列算法。
- 不把 gutter、bar、toolbar、overview 等概念提升为核心架构类型。
- 不构建类似 DOM、React 或通用 UI Component Tree 的第二套系统。
- 不提前引入复杂的 ViewSwitchPolicy；当前仅使用实例级 switchable 布尔值。

## 3. 核心概念与架构原则

| **概念** | **定义** |
| --- | --- |
| Content | 具有身份、生命周期和行为的数据模型。View 可以持有或借用一个或多个 Content。 |
| View | 完整的展示、状态和行为单元。View 可以控制多个 Pane，并可包含其他完整 View。 |
| Pane | 沿用当前 Workspace 布局中的承载区域。Pane 不拥有业务数据、状态或 presentation 类型。 |
| ViewDefinition | 定义一种 View 如何创建数据、状态、直属 Pane、子 View 以及初始布局。 |
| View instance | ViewDefinition 创建出的运行时实例，具有独立 ViewId、状态、Pane 集合和子 View。 |
| switchable | 实例级布尔属性，表示通用 View 切换操作是否可以替换该 View。 |

**总体原则：**Content 提供数据；View 决定展示、状态和行为；Pane 决定显示位置；现有布局系统决定几何关系。

```text
Content  →  View  →  Pane
  数据       控制      显示区域

View  →  child View
完整单元    完整单元的组合
```

## 4. View、Pane 与 Content 的关系

### 4.1 View 与 Pane：一对多

**一个 View 可以关联多个 Pane。**这些 Pane 可以位于 Workspace 布局的不同位置，但它们的显示内容、事件处理和状态都由同一个 View 决定。

```text
SpaceId A ─┐
SpaceId B ─┼── ViewId = BufferViewId
SpaceId C ─┘
```

因此，同一个 BufferView 可以同时通过多个 Pane 显示正文、行号、Git 标记和状态信息，而不需要为每个区域创建额外 View。

### 4.2 Pane 是显示区域，不是展示对象

- Pane 不知道自己是 gutter、bar、正文还是 Web 区域。
- Pane 不绑定 Content。
- Pane 不持有 selection、fold、mode state 等状态。
- Pane 不选择 renderer，也不产生 presentation。
- Pane 只提供布局位置、尺寸、焦点和事件来源身份。

**“gutter”与“bar”只是 BufferView 对 Pane 位置和用途的友好命名，**不是 Workspace 或核心协议中的新 Pane 类型。

### 4.3 View 与子 View：完整单元的组合

只有具有独立数据绑定、状态、生命周期和复用价值的展示单元才应成为子 View。

| **应作为 Pane** | **应作为 View** |
| --- | --- |
| 行号、Git gutter、diagnostics gutter | BufferView |
| 状态栏、命令栏、toolbar | WebView |
| minimap、scrollbar、diff overview | TerminalView |
| breadcrumb、临时提示区域 | DiffView、DashboardView |

**判断标准：**该对象是否应当作为完整单元被独立复用、嵌套或切换？如果否，通常是 Pane；如果是，通常是 View。

### 4.4 View 树与布局树是不同维度

**View 父子关系表达语义所有权、状态协调和生命周期；现有 Space 树表达几何布局。**二者可以关联，但不应互相替代。

```text
View 语义树：
DiffView
├── Left BufferView
└── Right BufferView

现有布局树：
Horizontal Container
├── Left BufferView 的若干 Content Space
└── Right BufferView 的若干 Content Space
```

## 5. 标准 BufferView 设计

### 5.1 结构

```text
BufferView
├── top bar Pane(s)
├── left gutter Pane(s)
├── body Pane
├── right gutter Pane(s)
└── bottom bar Pane(s)
```

**BufferView 不包含任何子 View。**所有区域都直接由 BufferView 控制，并共享同一个 Buffer 绑定和 BufferViewState。

### 5.2 数据与状态

```text
BufferView
  data:
    buffer: ContentId

  state:
    selections
    folds
    wrapping
    mode view state
    buffer-view-specific configuration

  panes:
    body
    left[]
    right[]
    top[]
    bottom[]
```

**Pane 本身不保存这些数据。**BufferView 在渲染某个 Pane 时，根据 PaneKey 或 SpaceId 从自己的数据和状态中生成对应展示。

### 5.3 Pane 身份

布局协议继续使用 SpaceId；View 内部需要维护 SpaceId 与稳定语义键之间的映射。

```rust
type PaneKey = String;

struct ViewPaneMap {
    by_space: HashMap<SpaceId, PaneKey>,
    by_key: HashMap<PaneKey, SpaceId>,
}
```

PaneKey 只是 View 识别直属 Pane 的稳定标识，不是新的布局节点类型。

### 5.4 View 根据 Pane 决定显示内容

```rust
impl BufferView {
    fn render(
        &self,
        pane: &PaneKey,
        context: &ViewRenderContext,
    ) -> Result<ViewPresentation> {
        match pane.as_str() {
            "body" => self.render_body(context),
            "builtin.line-numbers" => self.render_line_numbers(context),
            "builtin.status" => self.render_status(context),
            other => self.render_extension_pane(other, context),
        }
    }
}
```

**不需要 PanePresentation。**返回值仍是 View 针对某个 Pane 生成的 ViewPresentation。

## 6. 复合 View：DiffView 示例

### 6.1 语义结构

```text
DiffView
├── direct Pane: toolbar       (可选)
├── direct Pane: overview      (可选)
├── Left BufferView
└── Right BufferView
```

**左右两侧必须是子 View，而不是 DiffView 的两个 Pane。**因为每侧都是完整的 Buffer 编辑会话，分别拥有自己的 BufferViewState、gutter、bar 和正文 Pane。

### 6.2 职责分配

| **对象** | **职责** |
| --- | --- |
| DiffView | 持有或借用 diff 模型，协调左右侧映射、同步滚动、差异状态和跨侧命令。 |
| Left BufferView | 显示左 Buffer，拥有左侧 selection、fold、mode state 与直属 Pane。 |
| Right BufferView | 显示右 Buffer，拥有右侧 selection、fold、mode state 与直属 Pane。 |
| DiffView 直属 Pane | 显示 toolbar、overview 或其他仅属于 DiffView 的区域。 |

### 6.3 子 View 的 switchable

```text
DiffView                 switchable = true
├── Left BufferView      switchable = false
└── Right BufferView     switchable = false
```

这使通用切换命令在左右 BufferView 内触发时自动作用于整个 DiffView。

## 7. View 切换语义

### 7.1 当前 View 的定义

**当前 View 是当前焦点 Pane 的所属 View。**BufferView 的正文、gutter 或 bar Pane 获得焦点时，当前 View 始终是同一个 BufferView。

### 7.2 默认切换目标

**从当前 View 开始沿 parent 链向上查找，第一个 switchable == true 的 View 即为切换目标。**

```rust
fn resolve_switch_target(
    focused_space: SpaceId,
    views: &ViewStore,
) -> Option<ViewId> {
    let mut current = views.owner_of_space(focused_space)?;

    loop {
        let view = views.get(current)?;
        if view.switchable {
            return Some(current);
        }
        current = view.parent?;
    }
}
```

**DiffView 示例：**焦点位于右侧 BufferView 的任意 Pane；右 BufferView 不可切换；向上找到 DiffView；最终替换整个 DiffView。

### 7.3 switchable 是实例属性

**switchable 不应固定在 ViewType 上。**同一种 BufferView 在普通编辑区可以为 true，在 DiffView 子位置中可以为 false。

```text
普通编辑区域：
BufferView switchable = true

DiffView 内：
BufferView switchable = false
```

**当前阶段不需要 ViewSwitchPolicy。**如果未来出现真实的按类型限制、分级许可等需求，再在 replace 操作的校验层增加约束，而不是提前复杂化核心模型。

### 7.4 更换数据不等于切换 View

**将右侧 BufferView 从 Buffer A 改为 Buffer B，不是 View 切换。**View 实例、Pane 集合和 ViewType 均未变化，只是 View 使用的数据发生变化。

| **操作** | **语义** |
| --- | --- |
| set/open content | 修改现有 View 持有或借用的数据。 |
| switch/replace view | 替换完整 View，包括其状态、直属 Pane、子 View 和初始布局。 |

## 8. 渲染、事件与查询接口

### 8.1 打破 ViewId 与 SpaceId 一对一假设

**同一 View 可以对应多个 Content Space，因此仅凭 ViewId 无法确定要渲染哪个 Pane。**渲染和事件接口必须同时携带来源 SpaceId，或携带经 View 解析后的 PaneKey。

```rust
trait RenderQuery {
    fn view(
        &self,
        view: ViewId,
        space: SpaceId,
        viewport: &Viewport,
    ) -> Result<ViewData, RenderQueryError>;
}
```

**View 内部执行：**

```rust
let pane = view.panes.key_for_space(space)?;
view.implementation.render(pane, context)
```

### 8.2 Scene 与布局协议保持不变

```rust
enum SpaceKind {
    Container { arrangement: Arrangement },
    Content { view: ViewId, focusable: bool },
}
```

**允许多个 Content Space 使用同一个 ViewId 即可。**不需要增加 Pane 节点、View 节点或新的 Arrangement。

### 8.3 事件路由

输入事件首先由前端命中 SpaceId，然后解析为所属 View 和 PaneKey。

```text
Frontend event
  → SpaceId
  → owner ViewId
  → PaneKey
  → View::handle_event(PaneKey, event)
  → 可选：沿 View.parent 冒泡
```

**布局相邻关系不应用于业务事件冒泡；**语义事件应沿 View 父子关系传播。

### 8.4 ViewPresentation 仍属于 View

未来支持 Web、Tree、Canvas 等展示时，可以扩展 ViewPresentation，但不应创建 PanePresentation。

```rust
enum ViewPresentation {
    Text(TextPresentation),
    Status(StatusPresentation),
    Web(WebPresentation),
    Tree(TreePresentation),
    Canvas(CanvasPresentation),
    Custom(CustomPresentation),
}
```

枚举的每个值表示“某个 View 针对某个 Pane 的展示结果”。

## 9. TypeScript 扩展模型

*以下 API 仅用于表达目标开发体验，不作为最终命名承诺。*

### 9.1 扩展 BufferView 的 Pane

```typescript
editor.views.extend("core.buffer", {
  panes: [
    {
      id: "git.changes",
      region: "left",
      order: 200,
      size: { fixed: 1 },

      render(ctx) {
        return renderGitGutter({
          buffer: ctx.view.data.buffer,
          viewState: ctx.view.state,
          viewport: ctx.viewport,
        });
      },

      onPointerDown(ctx, event) {
        ctx.commands.execute("git.show-change", {
          line: event.contentLine,
        });
      },
    },
  ],
});
```

**插件注册的是 BufferView 的直属 Pane，不是子 View。**region 只属于 core.buffer 的友好扩展协议。

### 9.2 定义 BufferView 的初始布局

```typescript
editor.views.define({
  id: "core.buffer",

  create(ctx) {
    const body = ctx.pane("body");
    const left = ctx.extensionPanes("left");
    const right = ctx.extensionPanes("right");
    const top = ctx.extensionPanes("top");
    const bottom = ctx.extensionPanes("bottom");

    ctx.initialLayout(
      column([
        ...top,
        row([...left, body, ...right]),
        ...bottom,
      ]),
    );
  },

  render(ctx, pane) {
    // View 根据 pane.key 决定显示内容。
  },
});
```

**initialLayout 只创建初始 Workspace 布局。**实例化后，Pane 就是现有 Workspace 中的普通 Pane，由现有布局系统管理。

### 9.3 定义复合 DiffView

```typescript
editor.views.define({
  id: "core.diff",

  create(ctx) {
    const left = ctx.view("core.buffer", {
      switchable: false,
      data: { buffer: ctx.data.left },
    });

    const right = ctx.view("core.buffer", {
      switchable: false,
      data: { buffer: ctx.data.right },
    });

    const overview = ctx.pane("overview");

    ctx.initialLayout(row([
      left,
      overview,
      right,
    ]));
  },
});
```

**ctx.pane 创建当前 View 的直属 Pane；ctx.view 创建完整子 View。**

## 10. 数据、状态与生命周期

### 10.1 数据与状态由 View 持有或借用

**View 可以拥有数据，也可以通过稳定句柄借用 Content、父 View 或服务中的数据。**动态 View 树不应长期保存 Rust 的直接 &T 引用，应使用 ContentId、ViewId、StateKey 等可解析句柄。

```rust
enum ViewDataSource {
    Content(ContentId),
    ParentView { view: ViewId, key: DataKey },
    OtherView { view: ViewId, key: DataKey },
    Service { service: ServiceId, key: DataKey },
    Owned(DynamicValue),
}
```

**直属 Pane 不持有这些数据。**渲染 Pane 时，View 从自身数据和状态中读取所需内容。

### 10.2 View 运行时外壳

```rust
struct View {
    id: ViewId,
    type_id: ViewTypeId,

    parent: Option<ViewId>,
    children: Vec<ViewId>,

    panes: ViewPaneMap,
    layout_root: SpaceId,

    switchable: bool,
    revision: Revision,

    implementation: Box<dyn ViewInstance>,
}
```

```rust
trait ViewInstance {
    fn render(
        &self,
        context: &ViewRenderContext,
        pane: &PaneKey,
    ) -> Result<ViewPresentation, ViewError>;

    fn handle_event(
        &mut self,
        context: &mut ViewEventContext,
        pane: &PaneKey,
        event: ViewEvent,
    ) -> EventResult;
}
```

*上述结构是方向性示例。*具体实现可以继续使用 enum，以降低初期动态分发复杂度。

### 10.3 生命周期

- 创建 View：初始化数据与状态，创建直属 Pane，递归创建子 View，生成初始布局。
- 布局调整：只修改现有 Space 树，不改变 Pane 所属 View 或 View 父子关系。
- 销毁 View：销毁其直属 Pane，并递归销毁子 View。
- 切换 View：在解析出的目标位置销毁旧 View 子树，并用新 ViewDefinition 创建替代子树。
- 插件卸载：移除插件创建的直属 Pane、状态和回调，同时保持 View 其余部分有效。

## 11. 核心不变量

1. 每个 Pane 在任一时刻只由一个 View 直接控制。
1. 一个 View 可以直接控制零个、一个或多个 Pane。
1. 一个 View 可以包含零个、一个或多个子 View。
1. BufferView 的 gutter、bar 和 body 都是直属 Pane，不是子 View。
1. DiffView 的左右 BufferView 是子 View，不是直属 Pane。
1. Pane 不拥有 Content、业务状态或 presentation 类型。
1. View 根据 SpaceId / PaneKey 决定某个 Pane 的显示内容和事件行为。
1. View 父子关系不等同于 Space 布局父子关系。
1. 现有 Space 树是几何布局的唯一事实来源。
1. switchable 是 View 实例属性。
1. 通用切换目标是焦点 Pane 所属 View 的最近 switchable 祖先。
1. 修改 View 数据与替换完整 View 是两类不同操作。

## 12. 当前架构迁移方案

### 12.1 阶段一：消除状态栏特殊身份

- 删除状态栏作为独立 Content 的设计。
- 删除 status_target、is_status_bar、View::status_bar 等特殊路径。
- 将状态栏改为 BufferView 的直属 Pane。
- 将全局临时消息等特殊覆盖逻辑改为 BufferView 或 Workspace 对应 View 的 Pane 数据。

### 12.2 阶段二：支持一个 View 对应多个 Space

- 允许多个 SpaceKind::Content 使用同一个 ViewId。
- 将 space_for_view() 的单值假设改为 spaces_for_view() 或显式映射表。
- 为 View 增加 SpaceId ↔ PaneKey 映射。
- 渲染、viewport、鼠标与键盘事件接口携带 SpaceId。

### 12.3 阶段三：抽象通用 View 实例

- 将当前固定的 ContentId + ContentViewState View 结构迁移为通用 View 外壳。
- 把现有 BufferViewState 放入 BufferView 的具体实现中。
- 增加 parent、children、panes、layout_root、switchable 等运行时信息。
- 保持初期实现可使用 Rust enum，待插件自定义 View 成熟后再转为注册表或 trait object。

### 12.4 阶段四：实现 DiffView 验证模型

- 创建包含两个 BufferView 的 DiffView。
- 设置两个子 BufferView 的 switchable=false。
- 验证焦点位于任一子 BufferView Pane 时，通用切换命令作用于 DiffView。
- 验证左右 BufferView 拥有独立状态，而 DiffView 可以协调同步滚动和差异数据。

### 12.5 阶段五：开放 TypeScript 扩展

- 允许插件向 core.buffer 的 left/right/top/bottom 区域注册直属 Pane。
- 允许插件定义 render 与 event handler，由父 View 统一调度。
- 允许插件定义自定义 View、直属 Pane、子 View 和 initialLayout。
- 实现插件卸载时的 Pane、View、状态和回调清理。

| **当前概念/代码** | **目标方向** |
| --- | --- |
| View { content, state } | 通用 View 外壳 + BufferView 具体实现。 |
| 状态栏专用 View | BufferView 的直属 Pane。 |
| status_target / is_status_bar | 删除。 |
| 一个 View 对应一个 Space | 一个 View 对应多个 Space，每个 Space 映射到 PaneKey。 |
| RenderQuery::view(ViewId) | RenderQuery::view(ViewId, SpaceId, …)。 |
| ModeViewPolicy::status_bar | BufferView Pane 扩展或内建 Pane 定义。 |
| ViewPresentation::StatusBar 特判 | View 按 Pane 生成对应 presentation；是否保留具体变体由协议演进决定。 |

## 13. 风险、待决问题与验收标准

### 13.1 主要风险

| **风险** | **缓解措施** |
| --- | --- |
| ViewId 与 SpaceId 一对一假设分散在代码中 | 先建立全局搜索与测试清单，再逐步改为显式多映射。 |
| 同一 View 多 Pane 的 viewport 同步复杂 | 保留现有 viewport 机制，但所有查询携带 SpaceId；同步策略由 View 明确控制。 |
| View 树与布局树关系混淆 | 在代码命名和文档中始终区分 semantic parent 与 layout parent。 |
| 插件卸载留下 Pane 或状态 | 所有插件资源记录 owner ExtensionId，并提供统一清理路径。 |
| 切换复合 View 时布局替换边界不明确 | 每个 View 记录基于现有 Space 的 layout_root；跨边界移动规则需要在实现前固定。 |

### 13.2 待决问题

- View 的 layout_root 是否限制其直属 Pane 和子 View 的移动范围；若允许跨 root 移动，切换和销毁时如何处理。
- 同一 BufferView 的正文、gutter 与 bar 使用何种 viewport 同步协议；哪些 Pane 可独立滚动。
- ViewPresentation 对 Web/GUI 原生组件的长期协议形式。
- View 状态、子 View 与 Pane 布局的序列化及会话恢复格式。
- 插件动态增加或移除 Pane 时，是只影响新建 View，还是同步更新已有 View 实例。

### 13.3 验收标准

- BufferView 可以同时拥有正文、左右 gutter 和上下 bar，且所有 Pane 使用同一 ViewId。
- 移除状态栏专用 Content、View 和 status_target 后，现有状态栏功能保持可用。
- 同一 View 的不同 Pane 可以返回不同 presentation，并正确接收独立事件。
- DiffView 可以包含两个独立 BufferView，并保持各自 selection、viewport 和 mode state。
- 焦点在 DiffView 任一子 BufferView 内时，最近可切换祖先解析为 DiffView。
- 普通独立 BufferView 可设置 switchable=true，并能被通用切换命令替换。
- TypeScript 插件能够向 BufferView 添加 gutter/bar Pane，而无需创建 Content 或子 View。
- TypeScript 插件能够定义包含子 View 的复合 View。
- Scene / Space / Arrangement 的核心布局类型与算法无需重构。

## 14. 最终设计决议

> **架构定义：**Pane 是 View 的显示区域；View 是拥有数据、状态、行为和 Pane 的完整展示单元；View 仅在组合其他完整展示单元时包含子 View。

```text
BufferView
  owns/borrows: Buffer + BufferViewState
  controls: body/gutter/bar Panes
  children: none

DiffView
  owns/borrows: diff coordination data/state
  controls: optional toolbar/overview Panes
  children: Left BufferView + Right BufferView
```

**切换规则：**当前焦点 Pane → 所属 View → 沿 parent 向上找到最近的 switchable View。

**实施原则：**优先以最小改动打破 View 与 Space 的一对一假设，随后再开放通用 View 与 TypeScript 扩展；不修改现有布局概念，不提前引入复杂策略。
