# Codebase Cleanliness and Consistency Roadmap

**状态：** 待逐项处理
**创建日期：** 2026-07-17
**最近更新：** 2026-07-17

## 1. 文档定位

本文记录当前代码在架构约束、模块组织、职责封装、命名、测试组织和注释维护方面的整洁性
问题。它是问题台账和处理顺序，不是 Superpowers implementation plan。

每次只选择一个编号进入处理。涉及命令模型、跨层所有权或公共类型变化时，先在
`docs/design/` 编写或更新 design spec；纯文件移动、测试拆分和不改变行为的
整理可以直接实施，但必须保持改动范围窄。

状态约定：

- `待处理`：问题已经确认，尚未开始；
- `处理中`：已有当前实施项；
- `已完成`：代码、测试和相关文档均已更新并通过验证；
- `暂缓`：保留问题，但等待真实需求或前置项。

## 2. 当前基线

截至 2026-07-17：

- `cargo test` 通过，共 393 个测试；
- `cargo clippy --all-targets --all-features` 通过；
- `cargo fmt --check` 通过；
- `git diff --check` 通过；
- 当前问题以结构债务和一致性债务为主，不表示已有用户路径发生功能错误。

后续整理必须保持以下既有边界：

- `app` 不依赖 `tui`，`tui` 不依赖 `app`；
- `core` 不引入终端、布局、异步任务或渲染概念；
- `ContentStore` 仍是唯一内容表；
- `View` 仍是 app 层实体，selection 继续按 `ViewId` 归属；
- viewport 和布局尺寸仍归 Frontend/TUI；
- 不为了整理代码提前实现脚本运行时、远程 transport 或多客户端；
- 不改变现有 `Selection { anchor, head }` 语义。

## 3. 处理顺序总览

| 编号 | 优先级 | 状态 | 问题 |
| --- | --- | --- | --- |
| R01 | P0 | 已完成 | `app/mod.rs` 同时承担模块入口、核心实现和大型测试 |
| R02 | P0 | 已完成 | `Kernel` / `ClientSession` 只有字段分组，没有封装所属行为 |
| R03 | P0 | 已完成 | `ContentCommand` 分类与执行归属不一致，`Sequence` 可表达非法组合 |
| R04 | P1 | 待处理 | `core` 内存在双向模块依赖，泛型模块被具体命令污染 |
| R05 | P1 | 待处理 | `ContentStore` 使用并行表维护 Content 和 revision，重复 ID 静默覆盖 |
| R06 | P1 | 待处理 | editor/status ContentId 在启动路径存在重复真相源 |
| R07 | P1 | 待处理 | Vim action 名称在注册、执行和 keymap 中重复为字符串 |
| R08 | P1 | 待处理 | `buffer.rs`、`mode.rs`、`scene_renderer.rs` 等文件职责过密 |
| R09 | P2 | 待处理 | `dead_code` 抑制、可见性和阶段性注释失真 |
| R10 | P2 | 待处理 | View presentation 通过 selection 是否存在进行隐式推断 |
| R11 | P2 | 待处理 | Scene identity、ID 溢出策略和 Frontend View 生命周期不一致 |
| R12 | P2 | 待处理 | 测试组织、模块命名和当前架构文档存在局部不一致 |

## 4. 问题明细

### R01：整理 app 模块入口

**状态：** 已完成（2026-07-17）
**性质：** 不改变行为的结构重构

当前 `src/app/mod.rs` 同时包含模块声明、`App<F>`、主循环、输入队列、命令执行、保存调度、
布局生命周期、focus 修复、`AppQuery` 和大规模集成测试。这与 `core/mod.rs`、
`protocol/mod.rs`、`terminal/mod.rs` 和 `tui/mod.rs` 主要作为模块集合的约定不一致。

目标：

- `app/mod.rs` 只保留模块声明、必要的 re-export 和模块级文档；
- 将运行循环、保存、布局和查询适配器按现有职责移动到独立文件；
- 将现有 app 集成测试移入 `src/app/tests/` 或等价的测试子模块；
- 测试仍使用 app 测试模块内的 `ScriptedFrontend`，不恢复全局 `HeadlessFrontend`。

完成标准：

- 生产行为和公开接线不变；
- 原有 app 测试全部保留并通过；
- `main.rs` 仍只通过稳定入口构造和运行 `App`。

处理结果：

- `app/mod.rs` 仅保留模块声明、`App` re-export、模块文档和测试模块声明；
- `App` 定义与构造、运行时、保存、布局、查询适配器分别迁入
  `application.rs`、`runtime.rs`、`save.rs`、`layout.rs`、`query.rs`；
- app 集成测试整体迁入独立的 `tests.rs`，继续使用模块内 `ScriptedFrontend`；
- `main.rs` 仍通过 `app::App` 构造并运行应用。

### R02：让 Kernel 和 ClientSession 拥有行为

**状态：** 已完成（2026-07-17）
**前置：** R01

`Kernel` 和 `ClientSession` 当前主要是 `pub(super)` 字段集合，保存调度、View 分配、
Scene 修改、focus 修复、revision 更新和 Dispatcher 失效仍由 `App` 直接跨字段维护。当前
所有权拆分是名义上的，尚未形成行为封装。

目标：

- session-owned 操作由 `ClientSession` 维护 Scene、View、focus 和 scene revision 不变量；
- kernel-owned 操作由 `Kernel` 或独立保存协调器维护任务、消息和 pending save；
- `App` 保留事件循环和跨边界协调，不直接拼装内部不变量。

完成标准：

- 减少 `pub(super)` 字段；
- split/close/replace 的失败回滚和 ViewId 分配仍保持现有语义；
- 保存期间退出、错误退出和 queued snapshot 测试继续通过。

处理结果：

- `Kernel` 和 `ClientSession` 的状态字段全部改为私有，通过所属行为或只读查询接口访问；
- `Kernel` 统一维护取消信号、消息接收、后台任务、pending save、queued snapshot 和
  save completion 校验，`App` 只处理 ContentEffect 的跨边界协调；
- `ClientSession` 统一维护输入分发、resize、View 状态变换、ViewId 分配、Scene 修改、
  focus 修复、Dispatcher 失效和 scene revision；
- `App` 的布局入口变为薄委托，不再直接拼装 split/close/replace 的失败回滚不变量；
- app 集成测试改用只读接口和明确的 `#[cfg(test)]` 辅助入口，不再依赖内部字段可见性。

### R03：统一命令名称、执行归属与 Sequence 契约

**状态：** 已完成（2026-07-17）
**性质：** 跨层设计，实施前需要 design spec

`ContentCommand` 当前同时包含真正由 Content 执行的命令、由 View 的 `ModeInstance`
执行的 `Mode`，以及由 Frontend/TUI 解析布局尺寸的 `Viewport`。

`App` 会拦截后两类命令，而 `Content::execute` 对 `Mode` panic、对 `Viewport`
返回 `NotHandled`。此外，`Sequence(Vec<ContentCommand>)` 可以组合这些不兼容变体；
如果前序命令已经修改 Content、后序命令返回 `NotHandled`，可能出现部分执行但
ContentStore revision 不前进的状态。

目标：

- 命令类型名称与真实执行归属一致；
- 不为了修复问题制造过多顶层命令分类；
- `Sequence` 只能表达可按既定原子性规则执行的命令；
- 非法组合在执行前被拒绝，不能部分修改后再返回 `NotHandled`；
- unknown Mode/action 不再被无诊断地吞掉。

处理结果：

- 设计契约记录在 `docs/design/command-execution-ownership.md`；
- `ContentCommand` 只保留由 Content 执行的命令，`Save` 与编辑类命令通过穷尽的执行上下文
  分类区分是否需要 `ContentViewState`；
- `ModeCommand` 与 `ViewportCommand` 成为独立顶层命令；全局 keymap 可以直接绑定 viewport；
- Mode 原地更新私有状态后返回 `Option<Command>`，可以产生 Content、Viewport、App 等操作，
  并以原始 View 为来源复用与 keymap 相同的 Dispatcher 目标解析入口；
- `Sequence(Vec<ContentCommand>)` 改为 `Sequence(ContentSequence)`，验证容器在构造阶段拒绝
  `Save` 等执行上下文不兼容的成员；
- Buffer 只接收合法 Sequence，执行过程中不再因后序 `NotHandled` 造成部分修改和 revision
  丢失；`Content::execute` 删除了 Mode panic 与 Viewport `NotHandled` 分支；
- unknown mode/action、inactive mode 和注册但未实现的 action 通过 `ModeError` 返回明确诊断，
  App 不再静默吞掉；Mode 命令链使用有上限的迭代执行，循环组合会返回明确错误。

### R04：消除 core 内部双向模块依赖

**状态：** 待处理

当前主要依赖环包括：

```text
command <-> mode
buffer  <-> motion
command -> mode -> keymap -> command
```

`Keymap<A>` 本是泛型 trie，但 `core::keymap` 为 `bind_edit` 直接依赖具体
`Command/ContentCommand/EditCommand`；`core::motion` 又从 `core::buffer` 借用
`forward_word_start` 和 `line_end_insert`。

目标：

- 泛型 keymap/input 模块只依赖泛型 action 和中立按键协议；
- 文本位置、词法和行边界辅助逻辑拥有中立归属；
- Mode、Command、Motion 和 Buffer 的依赖方向可单向说明；
- 不把 Vim grammar 移入 Buffer。

### R05：收紧 ContentStore 的存储不变量

**状态：** 待处理

`ContentStore` 当前用两个 `HashMap` 分别保存 `Content` 和 `Revision`，依靠插入和
执行路径人工保持同步；重复 `ContentId` 会静默覆盖旧 Content 并重置 revision。

目标：

- 使用单一 `ContentEntry { content, revision }` 或等价结构；
- 插入重复 ID 时显式返回错误或旧值，策略与 Mode/View/Scene identity 保持一致；
- query、execute、revision 和 StatusBar target revision 行为不变。

### R06：统一启动阶段的 ID 分配

**状态：** 待处理
**前置：** 建议在 R05 后处理

`App::new` 创建 editor/status Content 时使用 `ContentId(0/1)`，而
`create_editor_session` 再次硬编码相同 ID。两处代码必须同步，属于隐藏耦合。

目标：

- 由一个 bootstrap 过程分配并传递 ContentId/ViewId；
- session 构造不重新猜测 ContentStore 中的角色 ID；
- 不要求 App 长期保存 editor/status 角色字段，仍可从 Scene/View 关系推导运行时目标。

### R07：为 Vim action 建立单一真相源

**状态：** 待处理

Vim action 名称当前同时出现在 `VIM_ACTION_NAMES`、`Mode::execute` 的字符串 match 和
keymap 构造代码中。`vim_mode_command(&str)` 还允许构造任意字符串，拼写错误只能在运行时
表现为 unknown action。

目标：

- 内建 Vim 使用 `VimAction` 或集中常量表达 action；
- 注册列表、keymap 和执行分派从同一组定义派生；
- 只有跨动态边界时才转换为 owned `ModeActionName`；
- 为 action 集合完整性提供集中测试，而不是依赖每个按键测试间接覆盖。

### R08：拆分职责过密的大文件

**状态：** 待处理
**说明：** 每个文件单独处理，不进行一次性全仓拆分

候选文件和职责：

- `core/buffer.rs`：文件生命周期、状态、事务历史、selection 变换、移动、编辑原语和词法辅助；
- `core/mode.rs`：trait、registry、instance、Vim 状态机、action、keymap 和测试辅助；
- `tui/scene_renderer.rs`：渲染流程、viewport、文本 cell 映射、selection paint 和状态栏格式化；
- `core/edit.rs`：大型命令 match 和重复的 selection 遍历；
- `app/dispatcher.rs`：固定序列、动态 Awaiting、目标解析和 global keymap。

拆分原则：

- 按变化原因拆，不按行数机械拆；
- 若改为目录模块，`mod.rs` 继续只做声明和 re-export；
- 先移动测试和私有辅助，再考虑类型边界；
- 每次只处理一个文件，并保持行为不变。

### R09：清理 dead_code、可见性和阶段性注释

**状态：** 待处理

当前源码有大量 `#[allow(dead_code)]`，其中部分已经失效，例如生产路径正在使用但仍标记为
预留或 Test helper。另有较多仅 crate 内使用的类型和方法采用宽泛 `pub`，降低了可见性对
所有权的表达能力，也让 dead-code lint 难以提供有效信号。

目标：

- 删除已经失效的 lint 抑制和错误注释；
- 真正预留的 API 使用准确、可验证的原因说明；
- 评估使用带 reason 的 `#[expect(dead_code)]`，让预期失效时产生反馈；
- 将仅限当前模块、父模块或 crate 的接口收紧为 private、`pub(super)` 或 `pub(crate)`；
- 将“v0.1/v0.2/本轮”等易过期描述改为当前语义或关联 roadmap 条目。

本项不授权仅因未使用就删除 AGENTS.md 明确保留的预留 API。

### R10：显式表达 View presentation

**状态：** 待处理

`AppQuery::view` 当前通过 `View::selections() -> Option<_>` 推断
`ViewPresentation::Text/StatusBar`，把“没有 selection”隐式等价为“StatusBar”。以后出现
只读文本或其他无 selection Content 时，该推断不再成立。

目标：

- presentation 由 Content/View 的显式契约给出；
- App 不通过具体 Content 类型探测 presentation；
- 新增 Content 类型时，编译器或集中分派能够提示必须补齐 presentation；
- pull query 和 TUI 的显式 `ViewPresentation` 分派保持不变。

### R11：统一 Scene 和 View 生命周期细节

**状态：** 待处理

待处理的小型一致性问题：

- `SpaceId` 同时存在于 Scene nodes 的 HashMap key、`SpaceNode.id` 和 `Space.id`；
- `Scene::from_parts` 允许构造三处 identity 不一致的快照；
- `SceneBuilder::alloc` 使用未检查加法，而 Revision、ViewId、ModeId 使用 `checked_add`；
- `SceneRenderer::viewports` 没有清理已从 Scene 删除的 ViewId。

目标：

- Scene identity 只有一个真相源，或构造时完成一致性校验；
- ID 分配使用统一的溢出策略；
- Frontend 在保留移动 View viewport 的同时清理真正消失的 View 状态；
- 不把 viewport 所有权移回 App。

### R12：统一测试组织、模块命名和当前文档

**状态：** 待处理

当前测试既有大型内联测试模块，也有 `tui/test_scene.rs` 形式的共享 fixture；部分 protocol
测试只验证 derive、构造或字段保存，回归价值较低。模块命名还存在
`tui::tui_frontend::TuiFrontend` 等重复。当前架构文档关于 Unicode width 的描述也已落后
于 `SceneRenderer` 实现。

目标：

- 小型、紧邻实现的单元测试继续内联；
- 大型集成流程和共享 fixture 使用统一测试子模块组织；
- 保留真正验证边界和行为的测试，减少只验证编译器 derive 的低价值测试；
- 消除明显的模块/type 名称重复，但不进行无关全仓改名；
- `docs/design/editor-kernel-architecture.md` 始终描述当前实现，不混入已完成前的旧阶段语义。

## 5. 每项通用验收

每个编号完成时至少满足：

- 只修改该编号及其必要接线，不顺带清理相邻问题；
- 行为变化先有失败测试，纯结构移动保持原测试覆盖；
- Rust 代码运行 `cargo test`；
- API、类型或跨层边界变化同时运行
  `cargo clippy --all-targets --all-features`；
- 运行 `cargo fmt` 或 `cargo fmt --check`，并检查格式化没有扩大 diff；
- 运行 `git diff --check`；
- 更新本文件对应条目的状态、结果和必要的后续项。

## 6. 推荐起点

下一项建议处理 **R04：消除 core 内部双向模块依赖**。该项会调整 Command、Mode、Keymap、
Motion 与 Buffer 的依赖方向，应先确认拆环边界，再进入实现。
