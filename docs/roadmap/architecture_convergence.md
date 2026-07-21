# 架构收敛与扩展系统 Roadmap

**状态：** 已完成（R1 至 R10 均已实施）

**更新日期：** 2026-07-21

**已完成基线验证：** `cargo fmt -- --check`、470 项测试、全目标 Clippy、
严格 TypeScript 类型检查、`git diff --check` 和 Markdown 行宽检查均通过。

## 1. 文档定位

此前的执行事务、typed operation、Mode state draft 和 presentation cache
改造已经完成。当前架构以
[`editor-kernel-architecture.md`][1] 和源码为准；R1 至 R10 保留已完成的
收敛结论，未来方向继续放在独立 roadmap，不写回当前实现文档。

R1 至 R5 消除了以下重复真相来源：

- 新旧 Mode callback 与 effect 同时存在；
- `Content` 和 `ContentStore` 都按具体 Content 变体分发；
- 新 View 模板与已有 View mode chain 分属不同所有者；
- 可见行 pull query 与全范围 presentation 刷新并存。

后续阶段完成了以下边界收敛：

- TypeScript v2 使用 command-first adapter DSL，键位和输入流语义已收敛。
- v1 脚本通过显式兼容层迁移到同一 registered adapter 形态。
- 后台 analysis 已与普通 command DSL 分层，不增加基础 Mode 的认知成本。

## 2. 审查结论

外部架构评估的主要方向合理。以下是实施前由源码确认的基线问题，现已在
R1 至 R5 中完成收敛：

- `Mode` 同时保留多代 callback，`ModeResult` 仍以 `ModeEffect` 为主输出；
- `AppOperation::Noop` 被 adapter 用作预算占位，而不是业务操作；
- `ContentStore` 在 `Content` 已分发一次后再次识别 `Buffer` 和
  `StatusBar`；
- `Kernel::new_view_modes` 保存了 session 的新 View 创建策略；
- `refresh_presentation` 遍历全部 View 和 Mode，并请求
  `0..usize::MAX`；
- `ContentQuery::Text` 可以在同步路径复制全文；
- Mode 名或 action 名冲突仍可能在注册阶段触发 panic；
- `core-dependency-direction.md` 对 Buffer 所有权的描述已经过时。

有三点需要限定：

1. 从现有同 Content View 执行普通 split 时，代码会复制源 View 的 mode
   chain，动态挂载不会在该路径丢失。真正的问题是新 View 模板、已有 chain
   和跨 Content 创建路径存在多套来源。
2. 全量 presentation 刷新是已确认的扩展性风险，但尚无 profiling 证明它
   已造成用户可感知的性能回归。先建立调用次数和大文件基线，再决定索引
   结构。
3. 当前只有 `Buffer` 和 `StatusBar`，暂不引入动态 Content trait object、
   通用依赖图或插件 Content registry。出现第三种 Content 或外部注册需求时
   再升级。

最新讨论提出的总体原则合理，本 Roadmap 采用以下结论：

1. Content 是 Rust 内核维护的封闭代数类型。新增 Content 必须修改源码并由
   穷尽匹配暴露遗漏，不提供脚本注册 Content 的能力。
2. Mode 是开放的动态注册实体。一个 Mode 通过一个或多个 Content adapter
   声明支持范围，adapter 的存在同时表达“是否支持”和“如何支持”。
3. native Mode 与 TypeScript Mode 最终进入同一套 adapter、state、operation
   和 presentation 后端，但 TypeScript API 不镜像 Rust SPI。
4. TypeScript 以命令为第一等概念，键位只是命令绑定；普通命令不暴露
   `OperationRequest`、`ExecutionFrame` 或 `ModeActionScope`。
5. 一个 Mode 的多个 adapter 可以共享不可变定义和配置，但持久运行状态仍
   只存在于 `(ModeId, ContentId)` 和 `(ModeId, ViewId)`。不隐式增加
   `ctx.shared` 或 Mode-global 可变状态。

讨论中的 `on.buffer` / `on.statusBar` 组织形式、Content 专属 context 和
`void | ctx.pass()` 返回语义都适合作为 v2 方向。每种 Content 单独建立大量
长期 trait、立即开放全局命令注册和直接暴露 worker 生命周期则不作为前置
条件；先用最小 adapter contract 验证内建 Vim 与语法高亮两个真实用例。

## 3. 不变项

改造期间必须保持：

- `core`、`protocol`、`app`、`frontend` 和 `tui` 的现有依赖方向；
- `App<F: Frontend>` 的泛型静态分发；
- Content 与 View state 分离，同 Content 的多个 View 共享文本但拥有独立
  selections；
- `Content` 和 `ContentKind` 都是封闭枚举，不引入动态 Content registry；
- Mode 保持动态注册，一个定义可以提供一种或多种 Content adapter；
- native Mode 与 TypeScript Mode 使用同一执行后端；
- 一次输入只使用一个 `ExecutionFrame`，失败时保持现有原子性；
- operation 顺序、nested Mode、`forward()`、history 和 prepared effect
  语义；
- render path 不调用 Mode、V8 或 worker；
- TypeScript v1 用户配置在明确发布新 API 前继续可用。

## 4. R1：收敛 Mode contract

**状态：** 已完成（2026-07-20）

**优先级：** 最高

目标是让 canonical path 只表达 `OperationRequest`，不再让兼容类型渗入
registry、executor 和测试 Mode。

实施范围：

1. 将仓库内 native Mode 迁移到唯一一组 state、input 和 action callback；
2. 让 `ModeResult` 直接携带有序的 `OperationRequest`；
3. 在 TypeScript v1 adapter 内直接完成旧脚本返回值到 typed operation 的
   转换；
4. 删除旧 callback 转发方法、`ModeEffect` 和 `ResolvedViewEdit`；
5. 删除 `AppOperation::Noop` 占位；实际 operation、nested Mode 和 replay
   继续使用 `ExecutionFrame` 已有的独立预算；
6. 让 `ModeRegistry::register` 返回结构化错误，至少覆盖重复 Mode 名和重复
   action 名。

本项目是二进制 crate，当前没有外部 Rust Mode 实现者。优先直接收紧现有
`Mode` trait，不为一次性迁移新增长期存在的 `ModeBehavior` 和
`LegacyModeAdapter` 双 trait。

验收：

- registry 和 executor 不再匹配 `ModeEffect`；
- 空 operation 列表表达 no-op；
- 重复脚本 Mode 或 action 返回配置错误，不 panic；
- operation 顺序不变，无限 nested Mode 仍会耗尽执行预算；
- nested Mode、失败回滚和 TypeScript 插件回归测试通过。

## 5. R2：统一 Mode attachment 所有权

**状态：** 已完成（2026-07-20）

**依赖：** R1

将“某个 Content 的新 View 应挂载哪些 Mode”归 `ClientSession` 所有，删除
`Kernel::new_view_modes`。

提供单一 attachment 操作，同时维护：

- Content 的新 View mode profile；
- 当前绑定该 Content 的所有 View chain；
- Mode content/view state attachment；
- face 注册、dispatcher 失效和 presentation 失效；
- background job 调度。

所有 View 创建路径都从同一 profile 读取 chain。复制源 View chain 只能作为
明确的用户行为，不能成为绕过 profile 的隐式备用规则。

验收：

- 动态 attach 后，从同 Content View split 的新 View chain 一致；
- 动态 attach 后，从其他 Content pane 创建该 Content 的 View 也一致；
- replace、close 和最后一个 attachment 的销毁计数正确；
- 同 Content 的 Mode content state 共享，Mode view state 独立。

## 6. R3：收紧 Content 与查询边界

**状态：** 已完成（2026-07-20）

**依赖：** R1，可与 R2 分开实施

短期继续保留静态 `Content` enum，但把具体变体判断收敛到一个位置。
`ContentStore` 只负责 ID、entry revision、生命周期和跨 Content 查询协调。

实施范围：

1. 将 presentation、view state 创建、snapshot 和普通 query 分派集中到
   `Content`；
2. 用窄的依赖查询上下文表达 `StatusBar -> target Content`，避免 Store
   直接识别 `StatusBar`；
3. 明确 own revision 与 dependency revision 的组合规则；
4. 从渲染共用的 `ContentQuery` 移除无界全文读取；Mode 后台分析使用已有
   `TextSnapshot`，远程读取保持有界；
5. 修正 `core-dependency-direction.md` 中 Buffer 仍持有 selection 和 history
   的过时描述。

暂不把 `ContentViewState` 改成可扩展 property bag，也不立即引入 trait
object。增加第三种有状态 Content 时，再将其改为显式 enum；需要运行时注册
Content 时，再单独设计 registry 和动态 state。

验收：

- `ContentStore` 不匹配 `Content::Buffer` 或 `Content::StatusBar`；
- StatusBar 的查询和有效 revision 仍随目标文档更新；
- TUI 只使用有界的 rows/points 查询；
- 同步公共查询不存在复制任意大小全文的入口；
- derived Content 依赖测试覆盖缺失目标和非法依赖。

## 7. R4：增量 presentation 失效

**状态：** 已完成（2026-07-20）

**依赖：** R1、R2

先为 Mode presentation callback 增加测试计数和大文件基线。确认全量刷新成本
后，以 dirty key 替代每次完整 `replace`：

```text
(ModeId, ContentId) -> shared content layer
(ModeId, ViewId)    -> independent view layer
ViewId              -> chain/order metadata
```

Content、View 和 Mode state revision 组成每个 layer 的 signature。刷新只
重算 signature 变化或新出现的 key，并淘汰已移除的 chain/View。face 在
查询时按名称解析，不需要重算 decoration layer。renderer 继续从不可变缓存
按可见行 pull。

第一版使用现有 decoration vector 和可见行裁剪。只有 profiling 证明筛选成本
成为瓶颈时，才引入 row index 或 interval index。

验收：

- 单个 View state 变化不重算无关 View layer；
- 单个 Content 变化只重算相关 shared layer 和绑定 View layer；
- render 不调用 Mode；
- 常规刷新不再向 Mode 请求 `0..usize::MAX`；
- 多 View、大文件和 stale worker 结果有回归测试。

## 8. R5：清理协调层债务

**状态：** 已完成（2026-07-20）

**依赖：** R1 至 R4

完成前述所有权调整后，再按实际调用形态决定是否：

- 用短生命周期 façade 替换 `mode_runtime_parts` 和
  `mode_attachment_parts`；
- 按 registry、state store、job 和 presentation 职责拆分 `mode.rs`；
- 将历史设计文档标记为“当前”“已取代”或“历史记录”。

若 parts tuple 在 R1 至 R4 后自然消失，不新增 façade。若 `mode.rs` 仍能通过
清晰导航维护，不为了文件长度单独拆分模块。

最终复审保留两个窄 parts 方法：它们只解决同一 `Kernel` 内不可变 Content/
Registry 与可变 Mode content store 的借用拆分。façade 仍需暴露相同三个引用，
不能减少协调点；把跨 Kernel/Session 操作移入 façade 反而会模糊所有权。
`mode.rs` 的 contract、registry、content store 和 view store 仍按类型边界连续
排列，因此本轮不做纯文件长度驱动的拆分。历史设计文档已明确标记当前、
已取代或历史状态。

## 9. R6：封闭 Content 的显式类型边界

**状态：** 已完成（2026-07-21）

**优先级：** 最高

**依赖：** R1 至 R5

`Content` 继续是 Rust 内核拥有的封闭枚举。下一步不是把它改成 trait object，
而是让所有与 Content 种类相关的状态也具有同样清晰的穷尽边界。

实施范围：

1. 增加封闭的 `ContentKind::{Buffer, StatusBar}`，并由 `Content::kind()`
   唯一映射；
2. 将 `ContentViewState { selections: Option<Selections> }` 改为显式枚举，
   例如 `Buffer(BufferViewState)` 与 `StatusBar(StatusBarViewState)`；
3. selection、view state 变换和 presentation 创建按 `(Content,
   ContentViewState)` 穷尽匹配，非法组合返回结构化 invariant error；
4. `ContentStore` 继续只管理 ID、revision、生命周期和依赖协调，不复制具体
   Content 的业务分派；
5. derived Content 依赖仍是内核能力。没有真实的多依赖 Content 前，不为
   adapter 工作提前引入通用依赖图。

`ContentKind` 是内核判别值，不是插件可扩展字符串，也不进入单独 registry。
新增第三种 Content 时，编译器必须提示 view state、presentation、query 和
renderer 中所有需要处理的位置。

验收：

- 仓库中不再用 `Option<Selections>` 表示 Content 种类；
- Buffer View 始终具有有效 selections，StatusBar View 无法误用文本状态；
- 新增测试覆盖 mismatched Content/View state，生产路径不依赖 `expect`；
- 没有新增 `Box<dyn Content>`、Content factory registry 或 property bag。

完成结果：`ContentKind`、`Content` 与 `ContentViewState` 现在保持封闭且
一一对应；编辑、共享 View 变换和 presentation query 都会拒绝种类错配。
`RenderQuery::view` 返回结构化 `RenderQueryError`，TUI 与远程查询不再把
错配变成 panic 或 release-only 静默行为。跨 View 变换失败会回滚 Content、
source selection/revision 和 history。实现经过三轮审查，最后一轮未发现明确
问题。

## 10. R7：将 Mode-Content adapter 提升为一等契约

**状态：** 已完成（2026-07-21）

**依赖：** R6

Mode registry 保存一个 Mode 身份和它提供的 adapter 集合。概念结构为：

```text
RegisteredMode
├── descriptor: name、order、faces、command names
└── adapters
    ├── Buffer adapter?
    └── StatusBar adapter?
```

adapter 的存在是支持关系的唯一真相。不要同时提供独立的
`supports(ContentKind)`；否则声明和实际 callback 仍可能不一致。

实施范围：

1. registry 可以按 `(ModeId, ContentKind)` 取得可选 adapter；
2. Buffer adapter 只接收文本、selection、edit、history、viewport 和文本
   presentation 能力；
3. StatusBar adapter 只接收状态查询和 StatusBar presentation 能力，不暴露
   cursor 或 text edit；
4. attachment 在修改 profile、创建 state 或注册 face 前验证 adapter，失败时
   返回 `UnknownMode`、`UnknownContent` 或 `UnsupportedContent` 等结构化错误；
5. 一个 Mode 可以同时提供多个 adapter，但每个 attachment 只实例化与目标
   Content kind 对应的 state 和 callback；
6. adapter 输出继续进入现有 `OperationRequest`、Mode draft、
   `ExecutionFrame` 和 presentation cache，不建立第二套执行代数。

第一版先设计最小的 closed adapter enum 或 adapter table。只有真实 native
Mode 证明按 Content 分开的 trait 能减少非法能力暴露时，才把每种 adapter
固化为独立 trait；不为了形式对称复制整套 `Mode` 默认方法。

状态作用域保持：

```text
(ModeId, ContentId) -> adapter content state
(ModeId, ViewId)    -> adapter view state
```

多个 adapter 可以共享 Mode 定义中的只读配置、face 和命令描述，但不自动共享
可变运行状态。需要跨 Content 协作时，先设计显式 session/service 所有权，不能
用未定义的 `ctx.shared` 绕过现有生命周期。

验收：

- 不支持的 Mode/Content 组合在 attachment 前失败且不留下部分 profile；
- 同一 Mode 可以分别挂载到 Buffer 和 StatusBar，并获得不同的合法 context；
- 同 Content 多 View 仍共享 content state、隔离 view state；
- registry、native Mode 和 ScriptMode 都只通过 canonical adapter 执行；
- render path 仍不调用 Mode、V8 或 worker。

完成结果：`ModeAdapters` 现在是注册时冻结的 closed support table，
registry 为每个已声明 `ContentKind` 返回绑定 Mode definition 的
canonical adapter。content/view context 按 Buffer 与 StatusBar 强类型
分支，Buffer 不再暴露通用 `ContentQuery`。动态 attachment 会在写入
profile 前验证 Mode、Content、adapter 以及所有已有 View state，并用
结构化错误拒绝不支持或错配的组合。ScriptMode 显式注册为
Buffer adapter。实现经过两轮审查，最后一轮未发现明确问题。

## 11. R8：TypeScript command-first adapter DSL

**状态：** 已完成（2026-07-21）

**依赖：** R7

TypeScript v2 以 Mode 为命名空间，以 Content adapter 为行为边界，以命令为
主要扩展单位。建议的最小形态为：

```ts
editor.modes.define({
  name: "pairs",
  on: {
    buffer: {
      state: () => ({ enabled: true }),
      viewState: () => ({ insertedPairs: 0 }),
      commands: {
        quote(ctx) {
          if (!ctx.state.enabled) return ctx.pass();
          ctx.edit.insert('""');
          ctx.cursor.moveLeft();
          ctx.viewState.insertedPairs += 1;
        },
      },
      keys: { '"': "quote" },
    },
  },
});
```

用户只需要理解：

- `state` 由同一 `(Mode, Content)` 的 View 共享；
- `viewState` 归具体 `(Mode, View)` 所有；
- `commands` 定义行为，`keys` 只引用命令；
- 正常返回 `void` 表示已处理，`return ctx.pass()` 继续下一个 Mode；
- callback 抛错时，本次 state 和暂存 operation 一起回滚。

实施范围：

1. `on.buffer` 与 `on.statusBar` 分别编译成 R7 的 canonical adapter；
2. `.d.ts` 根据 adapter key 缩窄 context，StatusBar callback 不出现文本编辑
   API，Buffer callback 不出现 StatusBar 专属输出；
3. mode-local 命令获得稳定的限定名，例如 `pairs.quote`，可被键位、其他命令
   和未来命令面板引用；
4. 普通命令不声明 `content | view` scope。由 adapter 和调用入口决定内部
   target，执行器继续保留必要的 `ModeActionScope`；
5. raw input、hooks 和 presentation 作为 adapter 的可选能力，不要求简单 Mode
   理解 background job、revision checkpoint 或 worker slot；
6. `ctx.edit`、`ctx.cursor` 等原语继续只暂存 typed operation，callback 成功后
   才交给现有 execution frame。

不再把 `boolean`、`ModeActionResult.continue`、`handled() -> false` 和
`forward() -> true` 作为 v2 用户模型。presentation snapshot 可以继续由返回值
或专用 API 发布，但输入流只有 `void | Pass` 一套语义。

验收：

- 自动配对示例不需要理解内部 action scope 或 operation 类型；
- TypeScript 类型层拒绝在 StatusBar adapter 使用 `ctx.edit`；
- 命令可以脱离默认键位调用和重新绑定；
- `void`、`ctx.pass()`、异常回滚和多个 Mode 的执行顺序有回归测试；
- Vim 的 raw input 能力不迫使普通 keymap Mode 使用同样复杂的入口。

完成结果：v2 schema 以 `on.buffer` / `on.statusBar` 生成 canonical
adapter，并按 Content 安装强类型运行时 context。命令以稳定限定名通过
`ctx.commands.invoke()` 调用，`void | ctx.pass()` 是唯一 v2 输入流语义；
嵌套限定命令共享 operation、事务帧和递归预算，但其整个调用子树不会覆盖
调用者的 flow。`statusBar.changed` 因没有独立 `ContentChange` 源而未暴露。
严格 `.d.ts` 负向用例由 `runtime/type-tests/tsconfig.json` 固化。实现经过
三轮审查，最后一轮未发现明确问题。

## 12. R9：v1 兼容迁移与内建插件验证

**状态：** 已实施（2026-07-21）

**依赖：** R8

迁移期只保留一个 `editor.modes.define` 公共入口。宿主根据定义是否包含 `on`
区分 v2 schema；旧的 `content/view/actions/keys` schema 由 v1 adapter 转换到
同一 canonical registration，不让两种 schema 渗入 registry 和 executor。

实施顺序：

1. 先为 v2 schema、错误信息和 `.d.ts` 增加独立测试；
2. 将内建 Vim 迁移到 `on.buffer`，验证 raw input、view state 和 pass 语义；
3. 将 Tree-sitter 高亮迁移到 `on.buffer`，验证 content state、worker 和
   revisioned decoration；
4. 更新 `docs/scripting.md`，默认只展示 v2 简单 Mode；
5. v1 用户配置在明确发布迁移说明前继续工作，并输出一次性 deprecation
   诊断；
6. 只有内建插件、示例和兼容测试全部通过后，才安排单独版本删除 v1 parser。

验收：

- v1 与 v2 定义最终产生相同的 registered adapter 形态；
- 内建 Vim 和 Tree-sitter 不依赖 v1 parser；
- 同名 Mode、重复命令、未知 adapter 和非法键位返回结构化配置错误；
- 用户配置升级有可执行示例，不要求一次性重写全部配置；
- 删除 v1 前，Rust 侧不长期维护两套 Mode runtime。

实施结果：内建 Vim 和 Tree-sitter 已迁移到 v2 adapter，v1 schema 继续由
parser 兼容并按 host 输出一次 deprecation 诊断。Vim raw input 使用独立的
`ModeInput` 内部通道，不注册为公开命令，也不能被键位或限定命令调用。
内建插件和类型测试纳入同一严格 TypeScript 检查。实现经过三轮审查，最后
一轮未发现明确问题。

## 13. R10：隔离高级 analysis 与独立命令能力

**状态：** 已实施（2026-07-21）

普通 Mode API 稳定后，后台派生计算已包装成命名高级能力：

```ts
analysis: {
  syntax: {
    worker: "worker.ts",
    snapshot: "text",
    input(ctx) { /* return worker message */ },
    apply(ctx) { /* publish state/presentation */ },
  },
}
```

宿主自动管理 slot、generation、revision、输入签名、取消、旧结果丢弃、state
transaction 和独立 presentation layer；普通 v2 Mode 文档不再暴露
`job/applyJob/slot/includeText`。一次 poll 会先计算全部 analysis input，再批量
发布取消和替换请求。当前 analysis 的 post-apply input 视为结果的一部分，不会
形成 self-feedback；其他 analysis 只在自己的 message 变化时重跑。

独立 `editor.commands.define` 明确延期。当前没有命令面板、外部 RPC 或
非 Mode 键表这一真实调用入口；现阶段继续使用 Mode-local 限定命令和
`ctx.commands.invoke()`，避免建立第二套脚本 action registry。未来出现调用入口
时，独立命令必须复用现有 adapter context、operation queue 和事务帧。

实施结果：内建 Tree-sitter 已迁移到 `analysis.syntax`。v2 parser、`.d.ts` 和
类型负向测试拒绝 raw worker 生命周期、StatusBar analysis、非法 snapshot 和
宿主保留字段。多 analysis 批量调度、state-only 替换、Disabled 取消、跨 slot
stale 拒绝和 self-feedback 均有回归测试。实现经过三轮审查，最后一轮未发现
明确问题。

## 14. 推荐顺序

```text
R1 Mode contract
-> R2 attachment ownership
-> R3 Content/query boundary
-> R4 incremental presentation
-> R5 coordination cleanup
-> R6 closed Content state
-> R7 Mode-Content adapters
-> R8 TypeScript command-first DSL
-> R9 compatibility and built-in migration
-> R10 advanced analysis and standalone commands
```

R1 至 R10 均已完成。R6 至 R10 每一阶段均单独审查，先写对应设计和能够失败的
架构回归测试，再修改实现。R10 由内建 Tree-sitter 真实插件用例驱动，没有与
基础 adapter 迁移捆绑。Rust 代码阶段完成后运行：

```text
cargo fmt
cargo test
cargo clippy --all-targets --all-features
git diff --check
```

[1]: ../design/editor-kernel-architecture.md
