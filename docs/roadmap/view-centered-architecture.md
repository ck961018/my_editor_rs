# View 中心架构升级计划

状态：执行中

更新日期：2026-08-10

本文取代旧的《Vell 架构改造设计文档》和命令系统路线图。
领域术语以 [Vell Context](../../CONTEXT.md) 为准；本文只描述升级方向、
阶段边界与验收场景，不固定尚未经过实现验证的 API 细节。

## 1. 目标

本轮升级只围绕一个核心判断展开：

> Content 是数据，View 是完整交互单元，Pane 是 View 控制的显示区域。

这项判断需要同时解决三个问题：

1. 一个 View 可以使用多个 Content 时，Mode 应该附加在哪里；
2. TypeScript 插件作者如何判断自己需要扩展哪一层；
3. 用户命令如何从“切换 Buffer”转为“切换完整 View”。

目标不是一次性设计出所有 View 类型，而是先建立稳定、容易解释的边界，
再用真实的复合 View 验证它。

## 2. 面向插件新手的最小心智模型

插件作者只需要先理解三个对象和三种扩展方式。

### 2.1 三个对象

- **Content** 保存数据，例如文本内容。它没有焦点、Pane 或快捷键。
- **View** 决定数据如何呈现和交互。它可以使用零个、一个或多个 Content。
- **Pane** 是 View 划出的一个显示或接收事件的区域，本身没有独立生命周期。

### 2.2 三种扩展方式

- `Mode`：给已有 View 增加行为，例如 Vim 操作或语法高亮。
- `View extension`：给已有 View 增加 Pane，例如独立 minimap 区域。
- `View definition`：定义一个完整的新 View，例如 DiffView。

后台计算仍使用 `Worker`。Worker 是异步助手，不是第四种界面扩展方式，
也不属于某个 Mode 的状态模型。

### 2.3 插件作者的判断顺序

开发插件时依次回答：

1. 我只是要改变已有 View 的行为吗？如果是，定义 Mode。
2. 我只是要给已有 View 增加一个区域吗？如果是，扩展 View。
3. 我是否拥有一套完整交互和生命周期？如果是，定义 View。
4. 我是否只是在后台计算结果？如果是，使用 Worker，并将结果发布给前
   三者之一。

插件作者不需要理解 Scene、Space、Taffy、终端坐标或宿主对象所有权。

## 3. 用三个例子固定边界

### 3.1 同一文本的两个 BufferView

一个文本 Content 可以同时被两个 BufferView 使用：

```text
Content A
├── BufferView 1：Vim Mode，光标在第 10 行
└── BufferView 2：普通编辑，光标在第 80 行
```

Content A 的文本和 revision 被共享。两个 View 的光标、滚动位置、
Mode view state 和焦点互不共享。

因此：

- 文本属于 Content；
- 光标和交互状态属于 View；
- Mode 运行时附加在 View；
- 可复用的分析结果可以按 `(ModeId, ContentId)` 共享。

### 3.2 DiffView

```text
DiffView（switchable = true）
├── BufferView left（switchable = false） -> Content A
└── BufferView right（switchable = false） -> Content B
```

DiffView 拥有左右对比、差异导航和整体生命周期。左右 BufferView 各自拥有
文本编辑、光标和与语言相关的 Mode。

当焦点位于右侧时：

- `view.switch` 替换最近的可切换祖先，即整个 DiffView；
- `diff.setRightContent` 只重绑右侧命名 Content binding；
- 后者不是通用 View 切换，也不改变 DiffView 的身份。

### 3.3 SearchResultsView

SearchResultsView 可以引用很多 Content 来展示匹配项，但这些引用不会让它
自动获得所有文件的语言 Mode。它只附加搜索结果 View 自己需要的 Mode。

用户进入某个匹配项时，可以创建或切换到一个 BufferView。语言 Mode 此时
根据该 BufferView 直接编辑的 Content 决定。

这个例子排除了一个错误规则：不能把复合 View 引用的所有 Content 对应的
Mode 做并集，然后全部附加到父 View。

## 4. 已确认的领域规则

### 4.1 Content

- Content 只负责数据和数据生命周期。
- Buffer 是一种 ContentKind，不是用户交互对象。
- View 通过带角色名称的 binding 使用 Content，例如 `document`、`left`
  或 `right`。
- 改变 binding 会保留 View 的身份；替换 View 会结束原 View 的生命周期。

### 4.2 View

- View 是呈现、交互、状态、行为和生命周期的最小完整单元。
- View 可以绑定零个、一个或多个 Content，也可以组合子 View。
- 同一 View 可以拥有多个 Pane。
- `switchable` 是 View 的替换边界，不是 Content 的属性。
- 结构不变量属于 View definition，不能依赖某个可选 Mode 才成立。

### 4.3 Pane

- Pane 只有稳定的 PaneKey、显示职责和事件入口。
- Pane 不拥有 Content、Mode、焦点状态或独立生命周期。
- Pane 到 Space 的映射由宿主管理，不暴露给 TypeScript 插件。
- 不引入 PaneState、PaneType 或第二棵插件侧布局树。

### 4.4 Mode

- Mode definition 是全局可注册的行为定义。
- Mode attachment 属于具体 View。
- content state 可以按 `(ModeId, ContentId)` 在多个 attachment 间共享。
- view state 按 attachment 所在的 View 隔离。
- 同一 View 可以附加多个有序 Mode。
- Mode 只能提交自身 draft 和 typed operation，不能借用可变宿主对象。

## 5. View 如何决定需要哪些 Mode

引入单一的 `ModeResolver`，统一做附加决策。它读取：

- View definition；
- View 的命名 Content bindings；
- binding 对应 Content 的分类结果；
- 工作区或用户的显式覆盖；
- Mode definition 声明的匹配条件与顺序。

它输出有序的 `ModeAttachmentPlan`，不直接执行 Mode callback。

### 5.1 Content 分类

文件后缀只能是分类信号之一。`ContentClassifier` 可以综合：

- 路径与后缀；
- 显式 language id；
- shebang 或模型识别结果；
- ContentKind；
- 用户覆盖。

分类结果属于 Content 元数据，不等于 Mode attachment。

### 5.2 默认解析规则

默认规则应当容易解释：

1. 先附加只依赖 View definition 的 Mode；
2. 再检查 Mode 声明所指定的命名 binding；
3. 只读取该 binding 的 Content 分类；
4. 最后应用用户禁用、启用与顺序覆盖。

BufferView 默认以 `document` binding 决定语言 Mode。DiffView 的父 View
只获得 Diff 级 Mode，左右子 BufferView 分别解析自己的语言 Mode。

### 5.3 禁止的隐式行为

- 不扫描整个 View 子树后合并所有 Mode；
- 不因为 View 间接引用某个 Content 就附加该 Content 的语言 Mode；
- 不让每个插件自行重复文件后缀识别；
- 不在 Rust bootstrap 中按内建插件名称选择 Mode。

## 6. 命令模型

公开命令使用领域语义，而不是底层存储类型。

### 6.1 Content 生命周期命令

这类命令只管理数据：

- `content.create`
- `content.open`
- `content.save`
- `content.reload`
- `content.close`
- `content.list`

它们可以返回 ContentId，但不会把 Content 当作焦点或切换目标。

### 6.2 View 命令

- `view.focus` 将焦点移动到已经存在的 View。
- `view.switch` 在当前焦点处寻找最近的 switchable View，并将整个 View
  替换为新的 View spec。
- View 特有命令可以重绑 Content，例如 `diff.setRightContent`。

一个面向用户的 `open` 操作可以组合 `content.open` 和 `view.switch`，
但组合内部仍遵守两个独立生命周期。

### 6.3 明确移除 Buffer 命令

公共命令空间中不应存在：

- `buffer.new`
- `buffer.open`
- `buffer.save`
- `buffer.list`
- `buffer.switch`

旧 `buffer.*` 语义按以下原则迁移：

- 数据生命周期迁到 `content.*`；
- 焦点移动迁到 `view.focus`；
- 完整交互单元替换迁到 `view.switch`；
- 某个 View 内的数据替换迁到该 View 的专有命令。

Buffer 仍可以出现在 core 内部类型、adapter 和文本 operation 名称中，
但不再作为用户命令目标。

### 6.4 命令执行基础设施的定位

原命令路线图中的注册、解析和执行能力只保留为通用调用机制。命令行、
Vim Ex 命令和 TypeScript evaluator 都应调用同一命令接口，但不能反向决定
Content、View 或 Mode 的领域边界。

持久 REPL、Promise continuation 和编辑器级类型环境不是本轮升级的前置
条件。如仍有需求，应在 View 模型稳定后单独规划。

## 7. 目标模块边界

### 7.1 ViewWorkspace

在 app 层形成一个深模块，暂称 `ViewWorkspace`。它用小接口隐藏：

- View 语义树；
- View 与 Pane 的所有权；
- PaneKey 与 SpaceId 映射；
- SceneBuilder、Scene 和 ID 分配；
- switchable 祖先解析；
- 子树创建、替换、关闭和焦点修复；
- View 生命周期事件的原子发布。

候选接口只表达领域操作：

```text
create(ViewSpec) -> ViewId
focus(ViewId)
switch_from(FocusTarget, ViewSpec) -> ViewId
rebind(ViewId, BindingKey, ContentId)
close(ViewId)
snapshot() -> Scene
```

具体名称可随实现调整，但调用方不得直接拼装 Scene 或分别清理 View、Pane、
Space、Mode state 和焦点。

`ClientSession` 负责协调输入与执行帧，`ViewWorkspace` 负责维护 View 结构
不变量。两者的 seam 应通过真实用例形成，不为假设中的前端提前抽象。

### 7.2 ModeResolver

`ModeResolver` 隐藏分类匹配、覆盖、排序和 attachment diff。调用方只提交
View 快照与相关 Content 元数据，并得到声明式计划。

安装和卸载 attachment 仍由执行帧管理，以复用预算、rollback 和原子发布
语义。

### 7.3 View adapter

Native View 与未来 TypeScript View 应共享同一宿主 View contract。只有在
至少一个 Native 复合 View 验证 contract 后，才建立 TypeScript adapter。

不要为了提前统一实现而立即引入泄漏内部对象的 `Box<dyn View>`。真正的
seam 应围绕 View spec、事件、operation 和 presentation 形成。

## 8. TypeScript 目标接口

下面的名称用于说明心智模型，不承诺最终字段形状。

### 8.1 Mode：给 View 加行为

```ts
editor.modes.define({
  id: "example.typescript",
  attach: {
    view: "core.buffer",
    binding: "document",
    language: "typescript",
  },
  on: { /* input and lifecycle handlers */ },
});
```

插件声明自己关心哪种 View、哪个 binding 和哪类 Content。它不查询全部
Content，也不操作 Scene。

### 8.2 View extension：给已有 View 加 Pane

```ts
editor.views.extend("core.buffer", {
  id: "example.minimap",
  panes: { minimap: { /* host-supported presentation */ } },
});
```

宿主分配 ID、挂载 Pane、处理布局和卸载。扩展不能接管整个 BufferView 的
生命周期。

### 8.3 View definition：定义完整交互单元

```ts
editor.views.define({
  id: "example.diff",
  bindings: ["left", "right"],
  children: [/* child View specs */],
  panes: { /* host-supported presentation */ },
  on: { /* lifecycle and event handlers */ },
});
```

宿主继续拥有 ViewId、PaneKey、SpaceId、预算、路径权限、故障隔离和原子
发布。插件只能返回 JSON-compatible state、typed operation 和受支持的
presentation。

## 9. 实施阶段

各阶段都应保持可提交、可回滚，并优先用公共 interface 测试行为。

### M0：术语和迁移清单

目标：让代码、文档和测试使用同一套领域语言。

工作：

- 记录 View attachment 和无 Buffer 命令空间的架构决策；
- 搜索所有用户侧 `buffer.*` 命令、文档和示例；
- 区分内部 Buffer 文本 operation 与公开命令；
- 为后续重命名建立兼容期或一次性迁移清单。

验收：新贡献者能仅通过 Context 和本文解释三个对象及三种扩展方式。

产物：

- [Mode attachment ADR](../adr/0003-mode-attachments-are-view-scoped.md)；
- [命令目标 ADR](../adr/0004-commands-target-content-or-view.md)；
- [View 命令迁移清单](view-command-migration-inventory.md)。

### M1：命令语义归位

目标：命令系统不再把 Buffer 当作切换对象。

工作：

- 建立 `content.*` 生命周期命令；
- 建立 `view.focus` 和 `view.switch`；
- 让 switch target 解析到最近的 switchable View；
- 移除公开 `buffer.*` 注册、类型声明和示例；
- 保持一次命令对应一个 ExecutionFrame。

验收：`view.switch` 从来源 View 解析到最近的 switchable 祖先，并只准备一个
拓扑副作用；当前 `core.buffer` View 的替换保持原子性，失败会恢复 View、
Content、input、history 和准备中的副作用。复合 View 子树的实际替换由 M2
统一收口；M1 明确拒绝带子 View 的切换目标，避免在 `ViewWorkspace` 形成前
维护两套分步清理协议或留下孤儿 Space。

### M2：深化 ViewWorkspace

实现状态：已完成。

目标：让完整 View 子树成为唯一的结构生命周期单位。

工作：

- 将 View 树和直属 Pane 映射收口到 `ViewWorkspace`；
- 统一创建、替换、关闭、焦点修复和 ID 分配；
- 让 Scene 只作为生成的只读快照流向前端；
- 删除调用方对 View、Pane、Space 的分步清理协议。

验收：删除或替换复合 View 后，没有孤儿 Space、Pane 映射、焦点或
Mode view state。

实现结果：`ViewWorkspace` 已成为 View 语义树、直属 Pane、Scene、焦点与
结构 ID 的唯一所有者。split、close、replace 和状态栏结构变更均先在完整
workspace 副本中执行并校验，再一次发布。`ClientSession` 只根据成功发布的
removed-view 事件清理 Mode、输入与 Face；App 再清理事务 owner 和 pending
command。
复合 View 的 switch 与 close 回归测试同时检查 Scene leaf、Pane 所有权、
焦点和 Mode view state，不再保留 M1 的临时拒绝路径。

### M3：命名 Content bindings

实现状态：已完成。

目标：支持一个 View 使用多个 Content，同时保持明确语义。

工作：

- 将单一 `ContentId` 演进为由 View definition 声明的命名 binding；
- 让 BufferView 使用 `document`；
- 区分 rebind operation 和 View replacement；
- 为 close Content 增加引用校验和明确的失败语义。

验收：重绑 DiffView 的 `right` 不会重建 DiffView，也不会影响 `left`。

实现结果：Kernel 的 definition registry 唯一持有 binding schema，View
实例只保存稳定的 definition id 和 `BindingKey -> ContentId` 映射。
BufferView 使用保留的 `document` binding；只有该 binding 携带与
ContentKind 对齐的 ContentViewState。typed rebind 在唯一的
ExecutionFrame 中原子发布，保留 ViewId、Pane 和其他 binding，并与完整
View replacement 明确分离。关闭 Content 会忽略同批删除的 document View
子树，但拒绝任何仍存活的 binding 引用，`force` 也不能绕过。

### M4：ModeResolver 和 attachment 生命周期

目标：集中回答“这个 View 需要哪些 Mode”。

工作：

- 建立 ContentClassifier 的稳定结果模型；
- 建立基于 View、binding、分类和 override 的 ModeResolver；
- 对 attachment plan 做增量 diff、排序和原子安装；
- 保持 content state 共享、view state 隔离；
- 补齐 binding revision 或 generation 校验。

验收：同一 Content 的两个 BufferView 可以有不同 Mode attachment，
但共享符合约定的 content analysis state。

### M5：用 Native DiffView 验证 contract

目标：在开放 TypeScript View API 前验证复合 View 的真实需求。

工作：

- 实现最小 Native DiffView；
- 左右使用不可切换的子 BufferView；
- 实现 `diff.setRightContent` 和整体 `view.switch`；
- 验证事件路由、Mode 解析、rollback、渲染和关闭清理。

验收：本文 3.2 节的全部行为由集成测试覆盖，且不需要 Pane 自有状态。

### M6：开放 TypeScript View extension

目标：先允许插件在已有 View 上安全增加 Pane。

工作：

- 从 Native DiffView 经验中提取最小 View extension contract；
- 只开放宿主支持的 presentation algebra；
- 实现插件卸载、预算、故障隔离和原子发布；
- 用 minimap 插件验证 Pane 扩展。

验收：卸载插件会完整移除新增 Pane 和回调，不改变宿主 View 的数据与
生命周期。

### M7：开放 TypeScript View definition

目标：允许插件定义完整 View，而不泄漏宿主内部结构。

工作：

- 在至少两个真实 View 适配器后稳定共享 contract；
- 支持声明 bindings、子 View、Pane、state 和事件；
- 限制 state 为 JSON-compatible owned data；
- 增加模块路径、预算、rollback、诊断和 unload 测试；
- 更新插件作者指南和最小模板。

验收：TypeScript DiffView 与 Native DiffView 遵守相同生命周期、
operation 和 presentation contract。

### M8：上层命令体验

目标：让命令行、Vim Ex 或其他入口复用稳定的领域命令。

工作：

- 评估是否需要持久 evaluator 和增量类型环境；
- 将入口限制为命令解析与调用，不允许旁路 ExecutionFrame；
- 将旧 Buffer 命令迁移为 View 或 Content 语义；
- 单独评估 Promise continuation，不与 View contract 捆绑。

验收：不同命令入口调用同一 registry 后具有相同目标解析、rollback 和
诊断语义。

## 10. 关键不变量

整个升级过程中必须持续满足：

- ContentStore 仍是唯一 Content 表；
- app 不匹配 Buffer 等具体 Content 变体；
- View 的结构变化只能由 ViewWorkspace 完成；
- Scene、Space 和布局不成为插件公共模型；
- 渲染路径不调用 Mode、V8 或 Worker；
- 异步结果通过 revision、generation 或 slot 校验后才能安装；
- 一次输入、timeout 或命令仍对应一个 ExecutionFrame；
- Mode 和 View callback 不能绕过预算、draft 和 typed operation；
- `view.switch` 替换完整 View，不退化为 ContentId 赋值；
- 公共命令和 TypeScript 声明中不存在 `buffer.*` 命令。

## 11. 测试策略

每一阶段至少覆盖以下层次：

- `vell-core`：Content 数据和 binding 所需的纯变换；
- `vell-mode`：resolver 输入、排序、state 共享与隔离；
- `vell-app`：ViewWorkspace、ExecutionFrame、rollback 和生命周期；
- `vell-plugin-v8`：schema、adapter、预算、故障与 unload；
- `vell-tui`：Scene 快照、Pane 几何与事件目标；
- `runtime`：TypeScript 声明、内建插件和迁移示例。

重点场景：

1. 同一 Content 同时出现在两个 BufferView；
2. 两个 BufferView 使用不同 Mode 顺序和 view state；
3. DiffView 左右 Content 使用不同语言 Mode；
4. 子 View 中的 `view.switch` 替换完整 DiffView；
5. `diff.setRightContent` 只改变命名 binding；
6. SearchResultsView 不继承全部被引用 Content 的 Mode；
7. 插件崩溃或超预算不会留下半个 Pane 或半个 View；
8. 全仓公开 API、文档和示例不再提供 Buffer 命令。

跨 crate 或 TypeScript 边界改动完成后运行：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
pnpm typecheck
```

## 12. 暂缓决策

以下问题应由真实实现反馈决定，不在当前计划中提前固定：

- View definition 的最终 Rust 类型擦除方式；
- TypeScript View schema 的具体字段名称；
- 一个 Mode 是否需要直接附加到复合 View 的某个 binding；
- 自定义 View 的全部布局表达能力；
- 插件卸载时复杂子 View 的用户恢复策略；
- 持久 TypeScript evaluator 与 Promise continuation 的实现方式。

默认优先通过子 View 表达独立编辑区域。只有当至少两个真实用例证明
“attachment 指向某个 binding”比子 View 更清楚时，才扩大 Mode contract。

## 13. 完成定义

当以下条件全部满足时，本路线图完成：

- 新手能用 Content、View、Pane 和三种扩展方式解释插件结构；
- ViewWorkspace 隐藏完整的 View 子树生命周期；
- ModeResolver 集中决定 attachment，插件不重复做文件类型分派；
- Native 和 TypeScript 扩展共享经过真实 View 验证的 contract；
- 用户通过 `view.focus` 与 `view.switch` 操作交互单元；
- Content binding 和 View switch 在 API、命令、文档与测试中明确分离；
- 公共表面不再存在 Buffer 命令。
