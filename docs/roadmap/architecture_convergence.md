# 架构收敛 Roadmap

**状态：** 已完成

**更新日期：** 2026-07-20

**最终验证：** `cargo fmt --check`、440 项测试、全目标 Clippy、
`git diff --check` 和 Markdown 行宽检查均通过。

## 1. 文档定位

此前的执行事务、typed operation、Mode state draft 和 presentation cache
改造已经完成。当前架构以
[`editor-kernel-architecture.md`][1] 和源码为准；本文只记录下一轮仍需
收敛的边界，不重复保存已完成阶段的实施日志。

这轮工作的目标不是增加更多抽象，而是消除以下重复真相来源：

- 新旧 Mode callback 与 effect 同时存在；
- `Content` 和 `ContentStore` 都按具体 Content 变体分发；
- 新 View 模板与已有 View mode chain 分属不同所有者；
- 可见行 pull query 与全范围 presentation 刷新并存。

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

## 3. 不变项

改造期间必须保持：

- `core`、`protocol`、`app`、`frontend` 和 `tui` 的现有依赖方向；
- `App<F: Frontend>` 的泛型静态分发；
- Content 与 View state 分离，同 Content 的多个 View 共享文本但拥有独立
  selections；
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

## 9. 推荐顺序

```text
R1 Mode contract
-> R2 attachment ownership
-> R3 Content/query boundary
-> R4 incremental presentation
-> R5 coordination cleanup
```

每一阶段单独审查，先补能够失败的架构回归测试，再修改实现；全部阶段通过
最终审查后统一提交。Rust 代码阶段完成后运行：

```text
cargo fmt
cargo test
cargo clippy --all-targets --all-features
git diff --check
```

[1]: ../design/editor-kernel-architecture.md
