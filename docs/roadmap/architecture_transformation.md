# 架构改造 Roadmap

**状态：** 近期架构改造已完成（P0 至 P4）

**范围：** 执行事务、Mode state、Presentation 与后续插件演进

本文描述 `my_editor_rs` 的渐进式架构改造方向。它不是一次性重写计划，
也不把尚无现实需求的插件能力提前固化为公共 API。

当前仓库已经具备合理的基础。现状以
[Composable Mode 架构][1]、[脚本架构][8]和当前源码为准：

- `Kernel` 拥有 Content、Mode content state、history 和后台任务；
- `ClientSession` 拥有 Scene、View、Mode view state 和输入状态；
- View 上可以附加有序 Mode chain；
- native Mode 与 TypeScript Mode 使用同一 `Mode` contract；
- 脚本操作进入 Rust typed executor；
- 脚本 decorations 缓存在 Rust state 中，渲染时不进入 V8；
- Frontend 通过 `RenderQuery` pull 可见内容和呈现数据。

这些边界应当保留。近期真正需要收敛的是 app 执行层：
[`runtime.rs`][2] 仍直接协调 snapshot、rollback、history checkpoint、
deferred effect 和命令预算。新增一种可变状态或外部 effect 时，容易遗漏
失败路径。

因此采用以下策略：

> 保留现有所有权与 Mode 模型，先统一执行事务，再统一 operation，
> 最后让 presentation 完全脱离 render-time Mode 调用。

---

# 一、目标与非目标

## 1. 近期目标

近期改造只解决以下问题：

1. 一次物理输入或显式命令具有统一、可审计的原子边界；
2. rollback、history 增量和外部 effect 不再散落在 executor 中；
3. command、Mode effect 和脚本原语逐步汇入同一 typed operation 后端；
4. Mode callback 修改 state 时使用明确的 draft/commit 语义；
5. renderer 只读取不可变 presentation cache，不调用 Mode 或 V8；
6. 保持现有用户行为、插件 API 和 pull render 模型。

## 2. 近期非目标

以下事项不与执行内核改造绑定：

- 新的 TypeScript Plugin API；
- 插件市场、依赖解析和版本求解；
- 热重载和 state migration；
- 每插件 V8 isolate；
- workspace/crate 拆分；
- virtual text、gutter、overlay 等新显示能力；
- 跨 Content 的原子编辑；
- 新的网络或文件系统宿主 API。

这些能力可以在核心边界稳定后独立立项。

## 3. 不变的行为契约

改造期间必须保持：

- operation 严格按声明顺序观察前序状态；
- edit plan 在轮到该 edit 时基于当时的 selections 生成；
- 一次输入失败不会发布 Save、Quit 或 frontend viewport mutation；
- Save 在操作序列中的位置决定它捕获哪个 `SaveSnapshot`；
- viewport move 先根据实际布局解析行数，再修改 cursor；
- `ExecutionFrame` 不等于 undo/redo `HistoryTransaction`；
- 输入失败只撤销本次追加的 history 增量；
- 被动 Mode callback 失败不能回滚已经成功的文本修改；
- 同一 Content 的多个 View 共享 content mode state；
- 每个 View 保持独立的 selections、viewport 和 view mode state；
- render path 不进入 V8；
- Frontend 继续通过 pull query 获取可见数据。

---

# 二、目标结构

```text
Frontend Event / Command
          |
          v
      Dispatcher
          |
          v
   Frozen ModeChain
          |
          v
  OperationRequest[]
          |
          v
  Target Resolver
          |
          v
  ResolvedOperation queue
          |
          v
  ExecutionFrame
  - checkpoint journal
  - mode state drafts
  - prepared effects
  - execution budget
          |
          v
       Commit
          |
          +--> publish prepared effects
          +--> schedule background jobs
          +--> refresh presentation cache
          +--> Frontend pull/render
```

## 1. 所有权

保持当前大方向：

```text
Kernel
|- ContentStore
|- ModeRegistry
|- ModeContentStore
|- TransactionManager
|- background mode jobs
|- save tasks
`- application messages

ClientSession
|- Scene + SceneBuilder
|- View store
|- ModeViewStore
|- input/dispatch state
|- FaceRegistry
`- PresentationLayerStore

App<F: Frontend>
|- Kernel
|- ClientSession
`- Frontend
```

`App` 继续负责组合服务。不要引入同时拥有全部对象并暴露任意可变访问的
`EditorManager`。

## 2. ExecutionFrame 的边界

`ExecutionFrame` 只拥有一次执行的事务元数据，不拥有 `Kernel`、
`ClientSession` 或 `Frontend`：

```rust
pub struct ExecutionFrame {
    checkpoints: CheckpointJournal,
    mode_state_drafts: ModeStateDraftStore,
    prepared_effects: Vec<PreparedEffect>,
    budget: ExecutionBudget,
}
```

app executor 仍通过 `Kernel` 和 `ClientSession` 的窄方法维护各自不变量。
Frame 不能成为绕过子系统 API 的通用 `&mut` 入口。

## 3. 两类事务必须分离

```text
ExecutionFrame
    一次物理输入或一次显式命令的原子执行范围

HistoryTransaction
    用户可见的 undo/redo 分组
```

若 frame 开始前活动 history transaction 已包含 `A`，本次执行追加 `B`
后失败，则只撤销 `B`；`A` 继续存在且 transaction 保持打开。

---

# 三、分阶段 Roadmap

| 阶段 | 核心目标 | 用户行为 | 状态 |
| --- | --- | --- | --- |
| P0 | 固化语义级行为基线 | 不变 | 已完成 |
| P1 | 提取统一 ExecutionFrame | 不变 | 已完成 |
| P2 | 统一 scoped operation | 不变 | 已完成 |
| P3 | Mode callback state draft 化 | 不变 | 已完成 |
| P4 | 建立 PresentationLayerStore | 不变 | 已完成 |

P0 至 P4 是当前架构改造范围。后续插件和 crate 事项放在本文末尾，
作为独立候选项目。

---

# P0：建立语义级行为基线

状态：已完成（2026-07-20）。

仓库已经有较多单元测试和 app 集成测试。P0 不追求把所有内部步骤做成
golden trace，而是确认迁移期间真正稳定的可观察语义。

## 1. BehaviorSnapshot

测试辅助层可以为一次输入或命令生成规范化结果：

```rust
pub struct BehaviorSnapshot {
    pub contents: Vec<ContentBehavior>,
    pub views: Vec<ViewBehavior>,
    pub history: Vec<HistoryBehavior>,
    pub mode_probes: Vec<ModeProbeBehavior>,
    pub prepared_effects: Vec<EffectBehavior>,
    pub published_effects: Vec<EffectBehavior>,
    pub faults: Vec<ModeFaultBehavior>,
    pub outcome: ExecutionOutcome,
}
```

`ModeProbeBehavior` 由测试 Mode 显式提供，只比较测试关心的状态。P0 不要求
任意 `Box<dyn ModeState>` 支持通用序列化或相等比较。

只比较稳定边界，不把以下内部细节冻结成兼容协议：

- executor 函数调用顺序；
- checkpoint 的具体容器；
- adapter 产生的中间类型数量；
- Mode callback trace 的内部格式；
- queue 或 stack 的具体实现。

内部 trace 可以在 test/debug feature 下提供，用于定位差异，但不能要求
新旧执行器产生完全相同的内部 trace。

## 2. 必须覆盖的行为矩阵

- 普通文本输入和 selection replacement；
- Vim normal/insert/visual/visual-line 行为；
- operator + motion；
- 多 selection；
- split view；
- 同一 Content 的多个 View；
- undo、redo 和打开的 history transaction；
- Mode chain 的 `forward()`；
- timeout、replay 和输入 capture；
- nested Mode invocation 和预算耗尽；
- TS callback 抛异常或返回非法 state；
- Content/View Mode state 回滚；
- 被动 callback fault isolation；
- Save、Quit 和 viewport 延迟发布；
- Save 在有序 operation 中捕获正确版本；
- stale resolved edit 被拒绝；
- decorations 和 ViewPolicy 合成；
- worker 取消、过期结果和 revision 校验；
- 插件加载顺序。

## 3. 兼容接口

P0 至 P4 期间，下列接口按 v1 兼容协议处理。TypeScript 侧声明以
[`editor.d.ts`][7] 为准：

```text
Command
ContentCommand
ModeEffect
ModeResult
editor.modes.define()
context.cursor
context.text
context.history
context.viewport
context.mode
context.app
```

可以在内部增加 adapter，但不能要求现有内置插件或用户配置一次性迁移。

## 4. 完成标准

- 关键失败路径都有语义级回归测试；
- 新旧路径可以比较 `BehaviorSnapshot`；
- 测试不依赖将被替换的内部类型；
- 任何行为变化都有单独说明，而不是混入架构 PR。

## 5. 实现记录

P0 已增加测试专用的 [`BehaviorSnapshot`][9]。它只读取稳定语义，
不会进入非测试构建，也不要求生产类型实现通用序列化：

- 内容、View 和 history 能力按 ID 排序并规范化；
- Mode state 由测试显式构造 `ModeProbeBehavior`；
- Mode fault 按 content attachment 和 view attachment 记录；
- effect 分别记录 prepared 和 published 边界；
- outcome 只区分成功和规范化后的错误信息。

[`app` 集成测试][10]新增以下语义基线：

- 相同场景可生成完全相等的快照；
- 失败 frame 保留 prepared effect 证据，但不发布 effect；
- 失败 frame 回滚内容和 history；
- 显式 Mode probe 与被动 callback fault 可同时观察。

TypeScript 测试补充了 callback 抛异常和非法 state 的状态不发布语义；
Kernel 测试补充了过期 worker completion 被拒绝；插件测试固定 manifest
顺序。其余矩阵继续由已有 Vim、selection、split view、history、dispatcher、
save、viewport、presentation 和 worker 测试覆盖。

P0 只增加测试观察接缝。运行时中的 effect 记录均受 `cfg(test)` 约束，
没有新增生产 API 或改变用户行为。

---

# P1：提取统一 ExecutionFrame

状态：已完成（2026-07-20）。

先在不改变 `Command` 和 `ModeEffect` 的前提下，收敛现有执行事务。
这样可以把行为变化与类型迁移分开验证。

## 1. CheckpointJournal

Frame 在第一次写入对象时保存 checkpoint：

```text
第一次修改 Content
    -> checkpoint Content

第一次修改 View
    -> checkpoint View state

第一次修改 ModeContent
    -> create ModeContent draft or checkpoint v1 state

第一次修改 ModeView
    -> create ModeView draft or checkpoint v1 state

第一次修改 history
    -> checkpoint target TransactionFlow

第一次修改 input/dispatch state
    -> checkpoint input state
```

当前 [`Kernel::command_transaction`][3] 已经为 history 提供 lazy checkpoint。
迁移时应复用其语义，再决定由 Kernel 继续持有还是并入 journal；不能平行
维护两份 history rollback 协议。

第一版可以继续使用已有 `ContentSnapshot`、selection snapshot 和
`ModeStateSnapshot`。先集中所有权，再逐项优化复制成本。

## 2. PreparedEffect

外部 effect 在 operation 执行到其位置时完成参数解析和数据捕获，但只有
frame 成功后才发布：

```rust
pub enum PreparedEffect {
    HistoryCommit {
        content: ContentId,
    },
    Save {
        content: ContentId,
        snapshot: SaveSnapshot,
    },
    Viewport {
        view: ViewId,
        command: ViewportCommand,
        lines: usize,
    },
    Quit,
}
```

这里的关键不是简单地把 `ContentId` 放进 `post_commit` 列表，而是保留
effect 在有序 operation 中观察到的状态：

- Save 必须携带当时捕获的 `SaveSnapshot`；
- viewport 必须携带前端根据当时布局解析出的 `lines`；
- history commit 必须保持与其他操作的逻辑顺序；
- 后续 operation 失败时不得发布任何 prepared effect。

## 3. 执行过程

```text
begin frame
-> execute ordered operations
-> validate frame invariants
-> commit state drafts
-> discard checkpoint journal
-> publish prepared effects in order
-> schedule background jobs
-> refresh invalid presentation
-> render
```

失败：

```text
restore checkpoints in reverse order
-> discard state drafts
-> discard prepared effects
-> restore input state
-> return the original error
```

## 4. 统一预算

```rust
pub struct ExecutionBudget {
    pub operations: usize,
    pub nested_mode_calls: usize,
    pub replayed_inputs: usize,
}
```

预算属于 frame。所有 adapter、nested Mode call 和 replay 共享同一预算。
本阶段可以保留现有深度优先语义；在 P2 再改成显式 queue/stack。

## 5. 完成标准

- 物理输入和测试用显式命令复用同一执行事务实现；
- `runtime.rs` 不再分别维护多套 rollback/publish 流程；
- history frame rollback 继续保留活动 transaction 的既有部分；
- Save、Quit 和 viewport 的失败原子性保持不变；
- 增加一种可变状态时有唯一 checkpoint 接入点；
- `cargo test` 和 clippy 全部通过。

## 6. 实现记录

- 新增 [`app::execution`][11]，集中持有 `CheckpointJournal`、
  `PreparedEffect` 和 `ExecutionBudget`；frame 不持有 Kernel、session 或
  frontend 的可变引用。
- 普通输入、输入超时和测试显式命令统一通过同一套 frame begin/finish
  生命周期；[`runtime.rs`][2] 不再分别实现 rollback 和 publish。
- Content 与同 content 的 selections 在第一次变更前按需 checkpoint；
  ModeContent、ModeView 仍按 attachment 首次变更保存状态。输入状态在
  dispatch 前保存，history 继续复用 Kernel 的 lazy command transaction，
  没有引入第二套 history rollback 协议。
- Save、viewport 和 Quit 在执行到其有序位置时捕获完整参数，frame 成功后
  才按顺序发布；任一后续 operation 失败都会丢弃全部 prepared effects。
- operation、nested mode call 和 replay 分别计数，但统一归属当前 frame；
  `ModeEffect` 与 `ContentCommand::Sequence` adapter 也纳入 operation 预算。
- Mode callback draft 和显式 operation queue 仍留给 P2、P3，本阶段保持
  现有深度优先执行与 mode API 不变。

---

# P2：统一 scoped operation

状态：已完成（2026-07-20）。

本阶段替换重复的 command/effect 执行分支，但不把“意图”和任意
`TargetRef` 做成可以自由组合的笛卡尔积。

## 1. Request 类型不表示非法组合

```rust
pub enum OperationRequest {
    Content {
        target: ContentTarget,
        operation: ContentOperation,
    },
    View {
        target: ViewTarget,
        operation: ViewOperation,
    },
    History {
        target: ContentTarget,
        operation: HistoryOperation,
    },
    Mode {
        target: ModeTarget,
        invocation: ModeInvocation,
    },
    App(AppOperation),
}

pub enum ContentTarget {
    Current,
    Id(ContentId),
}

pub enum ViewTarget {
    Current,
    Id(ViewId),
}

pub enum ModeTarget {
    CurrentContent,
    CurrentView,
}
```

`Current` 由明确的 `OperationOrigin` 解析：

```rust
pub struct OperationOrigin {
    pub scope: OperationOriginScope,
    pub view: Option<ViewId>,
    pub content: Option<ContentId>,
    pub mode: Option<ModeId>,
}
```

`scope` 区分 app、content 和 view 来源。它不是可选 target 的替代品，而是
resolver 判断 history owner 和 capability 边界所需的来源事实。例如
content-scoped 来源即使携带 source View，也不能直接发出 View operation。

缺少所需来源时 resolver 返回错误，不使用 focused View 静默补齐不相关的
目标。

跨 View 或跨 Content 操作在未来引入插件 capability 后再开放。当前 adapter
只产生 `Current` 或已有 app command 明确携带的 ID。

## 2. ResolvedOperation

```rust
pub enum ResolvedOperation {
    Content {
        content: ContentId,
        operation: ContentOperation,
    },
    View {
        view: ViewId,
        content: ContentId,
        operation: ViewOperation,
    },
    History {
        content: ContentId,
        owner: Option<ViewId>,
        operation: HistoryOperation,
    },
    Mode {
        mode: ModeId,
        scope: ResolvedModeScope,
        invocation: ModeInvocation,
    },
    App(AppOperation),
}
```

每个 variant 只包含该操作合法的目标。View variant 同时携带 `view` 和
`content`，resolver 必须验证二者绑定关系。

## 3. 目标解析与 edit planning 分离

`ResolvedOperation` 只表示目标已经确定，不表示所有编辑都提前规划完毕。

```rust
pub enum ContentOperation {
    Edit(EditCommand),
    Apply(ContentAction),
    Save,
}
```

`EditCommand` 在执行到该 operation 时，基于当时的 selections 生成
`ContentEditPlan`。这样以下序列仍然正确：

```text
Undo
-> InsertText
```

后一个 edit 必须观察 Undo 后的文本和 selections。

## 4. ViewEditPlan 与 stale 校验

`ResolvedViewEdit` 不再作为 Mode/插件公共 effect，但执行器内部仍需要一个
短生命周期的原子编辑计划：

```rust
struct ViewEditPlan {
    expected: ViewPrecondition,
    content: Option<ContentAction>,
    view: Option<ViewAction>,
}

enum ViewPrecondition {
    Selections(Selections),
    Revision(Revision),
}
```

当前 `ResolvedViewEdit::before` 同时承担 stale 校验和 history selection
before-state。迁移时应拆分职责：

- `ViewPrecondition` 继续拒绝 stale plan；
- frame 在第一次应用 View edit 时捕获 history before-state；
- transaction record 在成功后记录 before/after；
- 不能因为 history 改由 frame 捕获，就删除 stale 校验。

## 5. Adapter

兼容转换需要来源、可能失败，也可能产生多个 operation，因此使用显式函数，
不使用隐藏上下文的 `From` 实现：

```rust
fn adapt_command(
    command: Command,
    origin: OperationOrigin,
) -> Result<Vec<OperationRequest>, OperationError>;

fn adapt_mode_effect(
    effect: ModeEffect,
    origin: OperationOrigin,
) -> Result<Vec<OperationRequest>, OperationError>;
```

迁移顺序：

1. 新 executor 接收 `ResolvedOperation`；
2. `Command` 通过 adapter 进入新 executor；
3. `ModeEffect` 通过 adapter 进入新 executor；
4. 脚本 primitives 直接产生 `OperationRequest`；
5. 内置 Mode 逐步迁移；
6. 没有调用者后删除旧 executor 分支。

## 6. 执行 queue

使用显式 queue/stack 表达原有深度优先语义。nested Mode invocation 产生的
operation 必须插入当前 operation 之后、原列表剩余部分之前。

`ContentCommand::Sequence` 在 adapter 中展开，但整个展开结果仍属于同一个
frame。删除类型不能改变 sequence 的原子性和顺序。

## 7. 完成标准

- request 和 resolved 类型不能表达 target/operation 非法组合；
- `Command`、`ModeEffect` 和脚本 primitive 进入同一 executor；
- edit planning 继续发生在有序执行点；
- stale View edit 继续被拒绝；
- nested invocation、sequence 和预算语义保持不变；
- 旧插件 API 无需修改；
- 新旧路径的 `BehaviorSnapshot` 一致。

## 8. 实现记录

- 新增 [`app::operation`][12]，集中定义 typed request、resolved operation、
  origin、target、adapter、短生命周期 `ViewEditPlan` 和显式执行队列元素。
- `OperationOrigin` 额外保存 app/content/view scope。resolver 同时验证
  current target 是否存在、View/Content 绑定、history owner 和 mode target
  是否与来源匹配；content-scoped 来源不能借 source View 越权执行 View
  operation。
- [`runtime.rs`][2] 只保留一个 `ResolvedOperation` executor。Mode callback
  产生的 operation 以前插方式进入当前 queue，保持原有深度优先顺序；
  `ContentCommand::Sequence` 在 adapter 展开后仍属于同一 frame。
- edit planning 在 View operation 到达执行点时发生。内部 `ViewEditPlan`
  分离 selections/revision stale precondition 与 history before-state，后者在
  通过 stale 校验后从当前 View 捕获。
- `Command` 和旧 `ModeEffect` 保留兼容 adapter；`ResolvedViewEdit` 作为旧
  Mode API 的兼容 variant 暂不删除。脚本 primitive 已直接创建
  `OperationRequest`，仅通过 `ModeEffect::Operation` 携带到统一 executor。
- legacy sequence 与 nested effect 在 adapter 中保留显式 `Noop` 预算步，
  因而 operation、nested mode call 和 replay 的既有上限语义不变。
- 新增 adapter 顺序、非法 scope、预算兼容和 resolver capability 回归测试；
  P0 的 BehaviorSnapshot 测试继续覆盖成功与失败原子性。

---

# P3：Mode callback state draft 化

状态：已完成（2026-07-20）。

现有 Mode 同时拥有 per-content state 和 per-view state，这个模型保持不变。
需要改变的是 callback 对持久 state 的写入时机。

## 1. v1 与 v2 state 策略

当前 native state 是任意 `Box<dyn ModeState>`，当前 [`ModeState`][4]
提供的通用复制协议只有 `clone_box()`。不能只定义一个
`ModeStateDraft<'a>` 名称，就假设复制问题已经解决。

第一步采用可实现的兼容策略：

```text
v1 native Mode
    第一次写时 clone_box -> owned draft

v1 ScriptMode
    第一次写时 clone ScriptModeState -> owned draft

callback success
    seal draft, 暂不替换持久 state

next callback in the same frame
    read the latest sealed draft

all ordered operations success
    swap draft into ModeContentStore/ModeViewStore

frame failure
    drop draft
```

这一步的目标是统一提交时机，而不是承诺立即消除所有 clone。若后续 profiling
证明 clone 成本显著，再为 v2 state 设计 COW、persistent data 或专用
`fork_for_update()` 协议。

同一 frame 内的有序可见性必须保持：前一个 callback 成功修改 state 后，
后续 nested Mode invocation 必须读到最新 draft。所谓“提交后才可见”是指
对 frame 外的后续输入不可见，不是让同一 frame 内的调用读取旧 state。

## 2. ModeCall

```rust
pub struct ModeCall<'a> {
    pub context: ModeContext<'a>,
    pub content_state: ModeStateDraft<'a>,
    pub view_state: Option<ModeStateDraft<'a>>,
    pub operations: OperationCollector,
}

pub struct ModeCallResult {
    pub flow: InputFlow,
    pub presentation: PresentationInvalidation,
}
```

Mode callback 只能：

- 读取 Content/View snapshot；
- 修改自己的 content/view state draft；
- 追加 typed operation；
- 返回输入流向；
- 标记 presentation 失效；
- 创建只读后台分析请求。

Mode callback 不能：

- 保存 `&mut Content`、`&mut View` 或宿主引用；
- 绕过 executor 修改 Content、View 或 history；
- 直接调用 Frontend 或 Renderer；
- 在异步任务中直接修改宿主；
- 在 observer 中产生新的编辑操作。

## 3. 主动 callback 与被动 callback

必须明确两种错误语义。

主动 callback 包括 input、显式 content action 和 view action：

```text
callback/error validation fails
-> entire ExecutionFrame fails
-> discard drafts and prepared effects
-> restore frame checkpoints
```

本阶段的被动 callback 包括 content observer 和 view content observer：

```text
callback fails
-> discard only that callback draft
-> mark the corresponding attachment faulted
-> keep the successful Content/View mutation
```

若外层 frame 后续又失败，因本次已回滚的 change 产生的 fault 也不应发布。
因此 fault transition 应和其他 state draft 一样在 frame commit 时确认。

presentation refresh 也应采用相同的局部失败原则，但它依赖 P4 的
`PresentationLayerStore`。P3 不提前引入另一份 presentation cache。

## 4. Presentation refresh 时机

`present()` 不得由 renderer 调用。它只能在以下时机刷新缓存：

- Mode 初始化或 attachment 变化；
- callback 返回 `PresentationInvalidation`；
- Content revision 变化；
- background result 成功安装；
- theme/Face revision 变化。

refresh 在主循环的受控阶段执行。失败只 fault 对应 presentation attachment，
不能回滚已经提交的文本编辑。

## 5. 完成标准

- 执行 frame 中的 callback 只在 frame commit 时发布 Mode state；
- v1 native/script Mode 都通过明确 adapter 工作；
- 文档不承诺未经验证的零复制 state 方案；
- 主动与被动 callback 的错误语义有独立测试；
- 一个 Mode action 可以产生有序的 Content 与 View operation；
- renderer 不参与可变 Mode callback；只读 presentation callback 在 P4
  迁出 renderer。

## 6. 实现记录

- `ExecutionFrame` 持有唯一 `ModeDraftJournal`。content/view state 第一次
  写入时通过 `clone_box()` 建立 owned draft；同一 frame 的后续 callback
  读取最新 draft，成功时分别提交到 Kernel 和 ClientSession，失败时直接
  丢弃。
- input capture、timeout、显式 content/view action 和 nested Mode invocation
  都使用 frame draft。dispatcher 的输入 checkpoint 只保留 dispatcher
  状态，不再复制整条 Mode chain；旧 `ModeStateSnapshot` rollback 路径已
  删除。
- content/view observer 在 callback 前保存局部 draft checkpoint。observer
  失败只撤销该 callback 的 state 写入并在 draft 中记录 attachment fault；
  外层 frame 失败时 fault 也随 draft 丢弃，成功文本修改不会因 observer
  失败而回滚。
- input cancel、后台任务提取和后台结果安装发生在用户 frame 外，但仍通过
  短生命周期 `ModeDraftJournal` 执行，并在各自受控生命周期边界一次提交。
  后台结果失败只提交 fault transition，不发布 callback 的部分 state。
- native Mode 和 `ScriptModeState` 复用同一 first-write clone adapter；本阶段
  没有引入未经 profiling 验证的 COW 或零复制协议。
- Mode state 引起的 View revision touch 随 frame 延迟到 commit，避免失败
  输入留下仅 revision 可见的痕迹。
- 新增 Mode draft 单元测试，直接验证同 frame 可见性、commit 前隔离、drop
  回滚和被动 fault 的提交时机；既有 app/script 测试继续覆盖主动失败、
  observer 隔离、有序 Content/View operation 和脚本 callback 原子性。
- `view_policy` 与 decorations 仍是 render-time 只读 Mode 查询。它们没有
  可变 state 权限，也不会进入 V8；P4 将用 `PresentationLayerStore` 移除这
  最后一条 render-to-Mode 依赖。

---

# P4：建立 PresentationLayerStore

状态：已完成（2026-07-20）。

目标是保留 pull render，同时让 render path 不再访问 Mode trait。

## 1. View state 与 Presentation 分离

View operation 修改可交互状态：

```text
selections
viewport
preferred column
focus
scroll anchor
```

Presentation layer 只描述显示策略：

```text
cursor style
cursor domain
selection shape
selection face
text decorations
```

不要把 presentation patch 混入 `ViewAction`。

## 2. 第一版数据模型

只迁移当前已经存在的能力，不预留尚未实现的 virtual text、gutter、overlay
等字段：

```rust
pub struct PresentationLayerStore {
    content_layers: HashMap<
        (ModeId, ContentId),
        ContentPresentationLayer,
    >,
    view_layers: HashMap<
        (ModeId, ViewId),
        ViewPresentationLayer,
    >,
}

pub struct ContentPresentationLayer {
    pub source_revision: Revision,
    pub decorations: Vec<NamedTextDecoration>,
}

pub struct ViewPresentationLayer {
    pub content_revision: Revision,
    pub view_revision: Revision,
    pub policy: ModeViewPolicy,
    pub decorations: Vec<NamedTextDecoration>,
}
```

名称使用 `*Layer`，避免与 protocol 中已经存在的
`ContentPresentation`、`ViewPresentation` 混淆。

插件 lifecycle 将来引入 `PluginGeneration` 时，再把 generation 加入 layer
identity；P4 不提前建立尚不存在的插件身份模型。

## 3. 组合规则

Mode chain 按高优先级到低优先级排列：

- cursor style：第一个显式值生效；
- cursor domain：第一个显式值生效；
- selection shape：第一个显式值生效；
- selection face：第一个显式值生效；
- decorations：全部组合；
- decoration 冲突：高优先级 layer 后绘制；
- Mode 只能替换自己拥有的 layer；
- faulted 或无法映射到当前 revision 的 stale layer 不参与组合。

这些规则应成为公开测试契约，不再只由 `merge_missing()` 的调用顺序隐含。

## 4. 保留 RenderQuery pull 模型

[`Frontend`][6] 接口继续保持：

```rust
fn render(
    &mut self,
    scene: &Scene,
    scene_revision: Revision,
    query: &dyn RenderQuery,
    focused: SpaceId,
) -> io::Result<()>;
```

[`AppQuery`][5] 改为从 `PresentationLayerStore` 读取已缓存 layer。
TUI 仍只查询可见行，不预先物化全部 Content 和全部文本。

本阶段不引入包含所有 View/Content 的大型 `RenderSnapshot`，因为它会削弱
现有 pull、visible-range 和低复制边界。

## 5. 完成标准

- `AppQuery` 和 renderer 不调用 Mode trait；
- render path 不调用 V8、worker 或 plugin runtime；
- stale layer 能根据 source revision 拒绝或映射；
- split View 共享 content layer，并保留独立 view layer；
- visible-row decoration query 保持不变；
- 当前 cursor、selection 和 decoration 渲染行为保持不变。

## 6. 实现记录

- 新增 [`app::presentation`][13]，由 `ClientSession` 持有唯一
  `PresentationLayerStore`。store 保存共享 `(ModeId, ContentId)` content
  layer、独立 `(ModeId, ViewId)` view layer，以及每个 View 的 Mode 优先级
  顺序。
- v1 `Mode::decorations()` 通过默认 `view_decorations()` adapter 保持兼容；
  新增 `content_decorations()` 接缝。`ScriptMode` 将已有 content/view
  `DecorationSet` 分别导出，因此 syntax highlighting 的 content layer 在
  split View 间真正共享，而 view layer 保持独立。
- presentation 在初始化、attachment/layout 变化、成功 frame commit、后台
  任务调度或安装以及 save completion 后的主循环受控阶段刷新。刷新使用全
  content range 建立 immutable layer；Frontend 仍按 visible row range pull，
  `AppQuery` 只从 cache 过滤所需 decorations。
- 每个 content layer 记录 source revision，每个 view layer 同时记录 content
  与 view revision。`AppQuery` 只组合 revision 完全匹配的 layer；未刷新或
  faulted attachment 的 stale layer 不参与 cursor、selection 或 decoration
  呈现。
- policy 按 Mode chain 高到低采用第一个显式值；decorations 按低优先级到
  高优先级、同一 Mode 内 content layer 到 view layer 的顺序组合，保持高
  优先级后绘制的既有行为。
- [`AppQuery`][5] 不再持有 `ModeViewStore` 或 `ModeContentStore`，renderer
  调用链只读取 Content、View、presentation cache 和 FaceRegistry。Mode
  presentation 方法只在受控 refresh 阶段执行，render path 不进入 Mode、
  V8、worker 或 plugin runtime。
- 新增 stale view layer、split View layer 共享/独立性和 render 不回调 Mode
  的测试；现有 syntax highlighting、cursor、selection、named face、被动
  fault 和 TUI visible-row 测试继续覆盖行为兼容。

---

# 四、推荐的实际 PR 顺序

每个 PR 只改变一个主要边界。

1. 补齐语义级失败路径测试和 `BehaviorSnapshot` helper。
2. 引入 `PreparedEffect`，保持现有 `ModeEffect` API。
3. 提取 `CheckpointJournal`，统一两套 rollback 流程。
4. 逐项改成 content/view/mode state lazy checkpoint。
5. 将现有 history command checkpoint 接入 frame。
6. 引入内部 `ResolvedOperation` executor。
7. 添加 `Command` 和 `ModeEffect` 显式 adapter。
8. 引入 operation queue，迁移 nested call 和 sequence。
9. 将 `ResolvedViewEdit` 收敛为内部 `ViewEditPlan`。
10. 让脚本 primitive 直接产生 `OperationRequest`。
11. 引入 v1 Mode state draft adapter。
12. 新增 `PresentationLayerStore`，先迁移 ViewPolicy。
13. 迁移 content/view decorations，移除 render-time Mode 调用。
14. 删除没有调用者的旧 executor 类型和分支。

每个 PR 都必须满足：

```text
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features
git diff --check
```

涉及 Markdown 的 PR 还必须保证所有行不超过 80 列。

---

# 五、后续独立候选项目

以下项目只有在出现明确产品需求后才进入设计和实施阶段。

## 1. TypeScript Plugin API v2

v2 可以考虑：

- 显式 `apiVersion`；
- 稳定 `PluginId`；
- typed `OperationRequest` collector；
- 明确的 content/view state；
- presentation layer ownership；
- v1 `editor.modes.define()` adapter。

Capability 必须区分：

```text
requested capabilities
    插件 manifest 声明希望使用的能力

granted capabilities
    用户或信任策略实际授予的能力

effective capabilities
    host API 每次调用时校验的交集
```

manifest 不能通过自行声明 `network` 或 `filesystem.workspace` 自动获得权限。

在 v2 spec 确认前，不扩充当前 TS API 作为执行内核改造的一部分。

## 2. 插件 lifecycle 与隔离

引入 unload/reload 前，需要先定义：

- PluginId 与 PluginGeneration；
- Mode registration owner；
- attachment 和 presentation layer 清理；
- worker result generation/revision 校验；
- 原子 load/unload；
- fault recovery；
- state 是否迁移。

一个 V8 `Context` 只能隔离全局对象，不能提供独立 heap、CPU budget 或故障
边界。严格资源隔离需要独立 `Isolate`，其成本和同步 callback 延迟必须通过
实际测量决定。

## 3. V8 feature 化

将 V8 设为可选 feature 或独立 crate 可以减少不使用脚本时的构建成本，
但当前默认编辑行为由 TypeScript Mode 提供。

在关闭 V8 仍能形成有用产品之前，必须先选择并实现一种策略：

- 提供最小 native editing Mode；
- feature-off 构建明确只用于 headless/core 测试；
- 或保持正式二进制始终启用 V8。

不能只把 Cargo dependency 标记为 optional，就声称已经支持无脚本编辑器。

## 4. 拆分 crate

当前不预先承诺九个 crate。只有满足下列条件时才拆分：

- 边界已经在单 crate 内稳定；
- 拆分能带来明确的编译时间、feature 或复用收益；
- 不需要为了绕过循环依赖而泄露 app 私有类型；
- 拆分后的依赖方向与仓库约束一致。

候选依赖方向仍是：

```text
protocol <- core
protocol <- frontend
protocol + core + frontend <- app
protocol + frontend + terminal <- tui
app + tui + terminal <- bin
```

如果未来增加 `mode-api` 或 `script-v8` crate，它们必须依赖稳定的中立
contract，不能反向要求 `frontend` 或 `core` 依赖 app。

## 5. 新 Presentation 能力

virtual text、inline hints、gutter、overlay、code lens 等能力在各自出现
真实需求时单独设计。它们可以复用 layer ownership 和 revision 规则，但不在
P4 中预留空字段或未验证的组合协议。

---

# 六、必须坚持的架构规则

## 1. Mode 控制行为，但不拥有宿主对象

```text
Mode -> typed OperationRequest
Mode -> private state draft
Mode -> PresentationInvalidation
```

禁止：

```text
Mode -> &mut Content
Mode -> &mut View
Mode -> Frontend
Mode -> Renderer
```

## 2. 非法 target/operation 组合不能被正常构造

优先使用 scoped enum variant，而不是独立 `TargetRef + OperationIntent`。
resolver 负责解析 `Current` 和验证 View/Content 绑定，不负责修补模糊来源。

## 3. 有序操作必须观察前序状态

target 可以提前解析；依赖 selections、history 或当前文本的 edit plan 必须在
执行到该 operation 时生成。

## 4. 外部 effect 必须 prepare 后延迟发布

prepare 在有序执行点捕获完整 payload；publish 只在 frame commit 后发生。
不能把裸 `ContentId` 留到提交后重新读取状态。

## 5. Presentation 必须声明式且可缓存

Mode 更新自己的 layer；`AppQuery` 合成 cache；Frontend pull 可见数据。
renderer 不调用 Mode、V8 或 worker。

## 6. Rust 与 TypeScript Mode 使用同一执行后端

```text
native Mode ---+
               +-> OperationRequest -> ExecutionFrame
ScriptMode ----+
```

TypeScript adapter 可以负责坐标转换和 state 校验，但不能建立第二套 JSON
command executor。

## 7. 异步任务不能直接修改宿主

```text
worker
-> owned result
-> main loop validates identity/revision
-> install Mode state draft
-> invalidate presentation
```

需要修改 Content 时，必须重新读取最新 snapshot，并通过一次显式 operation
进入正常执行路径。

---

# 七、近期完成标准

P0 至 P4 完成后，应满足：

1. `runtime.rs` 不再手工维护重复的 rollback 流程；
2. 一次输入或命令只有一个 `ExecutionFrame`；
3. checkpoint、prepared effect 和 budget 有明确所有者；
4. Save 和 viewport prepared effect 携带完整、顺序正确的 payload；
5. history transaction 与 execution frame 保持独立；
6. request/resolved operation 不能表达非法 target 组合；
7. edit planning 在有序执行点发生；
8. stale View edit 仍有显式前置校验；
9. `Command` 和 `ModeEffect` 只作为 v1 adapter 输入；
10. Mode state 只有 frame commit 后才对后续输入可见；
11. 主动 callback 失败回滚 frame；
12. 被动 callback 失败只 fault 对应 attachment；
13. `AppQuery` 和 renderer 不调用 Mode trait 或 V8；
14. Frontend 继续使用 `RenderQuery` pull 模型；
15. 当前 TypeScript 插件无需修改；
16. 所有语义级回归测试、clippy 和 diff check 通过。

近期工作的优先顺序是：

```text
P0 semantic baseline
-> P1 ExecutionFrame
-> P2 scoped operation
-> P3 Mode state draft
-> P4 PresentationLayerStore
```

Plugin API v2、plugin lifecycle、crate 拆分和新显示能力不属于这条关键路径。

[1]: ../design/composable-mode-architecture.md
[2]: ../../src/app/runtime.rs
[3]: ../../src/app/kernel.rs
[4]: ../../src/app/mode.rs
[5]: ../../src/app/query.rs
[6]: ../../src/frontend/mod.rs
[7]: ../../runtime/editor.d.ts
[8]: ../scripting.md
[9]: ../../src/app/behavior.rs
[10]: ../../src/app/tests.rs
[11]: ../../src/app/execution.rs
[12]: ../../src/app/operation.rs
[13]: ../../src/app/presentation.rs
