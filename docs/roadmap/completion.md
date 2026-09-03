# 高性能可扩展补全系统 roadmap

状态：规划中

更新日期：2026-08-13

## 1. 文档定位

本文规划 Vell 的文档内补全系统。目标不是先做一个 LSP 弹窗，再逐步向外
打补丁，而是建立一个高性能的补全平台：多个来源可以独立触发、取消和增量
返回候选；统一引擎负责匹配、排序、缓存、交互与原子接受；native 与
TypeScript 扩展共享同一条执行和故障隔离路径。

本文替代 `docs/roadmap/` 中已经完成的 View、命令迁移和 gutter 路线图。
当前架构事实仍以 `docs/design/`、`docs/adr/` 和源码为准。

## 2. 目标、预算与非目标

### 2.1 产品目标

- 手动触发和自动触发均不阻塞输入；
- buffer words、snippet、path、LSP 和第三方来源可以并行工作；
- 慢来源不会阻止快来源先显示，也不会覆盖更新 session 的结果；
- 候选匹配、排序、裁剪和选择完全位于 Rust 热路径；
- 来源可以提供精确 edit、lazy documentation、额外 edit、commit character、
  snippet 和接受后动作；
- TypeScript 可以定义来源和批量装饰，但 render path 不进入 V8；
- 同一套语义 presentation 可以被 TUI 和未来远程 Frontend 使用；
- 每个阶段都由延迟、吞吐、内存和正确性门禁验收。

### 2.2 初始性能预算

M0 必须记录测试机器、编译 profile、数据集和测量方法。下表是第一轮目标，
不是跨硬件的产品承诺；基线建立后，CI 同时检查绝对预算和相对回退。

| 场景 | 目标 |
| --- | --- |
| 打字路径增加的同步工作 | p99 小于 0.5 ms |
| 本地来源到首批可见候选 | p95 小于 8 ms |
| 10,000 项重筛并取得前 100 项 | p95 小于 4 ms |
| 100,000 项重筛并取得前 100 项 | p95 小于 16 ms |
| 已安装异步结果触发重绘 | 下一次 render tick 内完成 |
| stale、取消或越界结果安装 | 0 次 |
| 100,000 个普通候选的 session 增量内存 | 小于 64 MiB |

热路径预算不包含 LSP 或其他外部进程的响应时间。外部延迟单独记录
time-to-first-result、time-to-final-result、取消确认和超时。

### 2.3 第一阶段非目标

- 不在第一版实现 AI 行级或多行 inline completion；
- 不把 signature help、code action 或 hover 混入 completion 协议；
- 不在 TUI、`vell-core` 或 LSP adapter 中建立第二套排序器；
- 不承诺首版支持多 selection 接受；
- 不执行来自项目目录的未受信任补全脚本；
- 不用通用 `OverlayView`、`PopupContent` 或插件布局树承载候选菜单；
- 不为还不存在的 command-line View 预先抽象通用 minibuffer 补全。

## 3. 参考系统与明确取舍

### 3.1 Neovim

Neovim 的 insert completion 区分查找来源、候选 item、popup 交互、接受和
取消；候选可以携带 `abbr`、`kind`、`menu`、`user_data`、预选和 commit
characters。[Neovim insert completion][nvim-insert] 的 `completefunc` 与
`omnifunc` 仍是同步的两阶段函数契约，耗时来源需要主动检查取消。

Neovim 的 LSP completion 会在接受后处理 snippet、text edit、额外 edit 和
关联命令，并在选中时 lazy resolve 预览信息。
[Neovim LSP completion][nvim-lsp]

Vell 采用：

- 明确区分查询、展示、选择和接受；
- 支持丰富但 owned 的候选数据；
- LSP item 的副作用必须由统一接受路径解释。

Vell 不采用：

- 在一次按键调用中同步运行来源；
- 由来源控制 popup 或直接修改 buffer；
- `findstart` 与 `base` 两次回调组成的弱类型协议。

### 3.2 Emacs

Emacs 的 `completion-at-point-functions` 返回
`start/end/collection/properties`。collection 可以延迟生成，匹配策略由
调用方决定，`:exclusive` 可以让另一个来源接管。
[Completion in buffers][emacs-capf]

programmed completion 还提供 category、annotation、affixation、group、
display sort 和完成后动作等 metadata。这种“候选集合与策略分离”的接口
是本 roadmap 最重要的扩展性参考。
[Programmed completion][emacs-programmed]

Emacs 官方文档同时要求 CAPF 快速返回，并建议昂贵集合使用 lazy table，
因为 CAPF 可能被频繁调用。本文不把 Emacs 的整体性能归因于单一实现；对
Vell 的直接推论是：不要让扩展作者承担高频调用协议，也不要把动态脚本
predicate、annotation 或 comparator 留在每次按键的逐候选热路径。

Vell 采用：

- 来源只描述候选集合、范围、category 和接受语义；
- matcher、排序、分组和外观是可组合策略；
- 支持 bulk transform、lazy resolve 和 accepted hook。

Vell 不采用：

- 一个回调根据 flag 模拟多种操作的多态接口；
- 任意脚本函数逐项参与过滤、比较或绘制；
- 依赖来源自行缓存才能保持可用延迟。

### 3.3 blink.cmp

blink.cmp 明确使用 `trigger -> sources -> fuzzy -> render` 四阶段流水线；
source interface 负责候选、触发字符、resolve 和取消，Rust matcher 负责
过滤与排序。[blink.cmp architecture][blink-architecture]

它的来源可以配置异步、timeout、最大项数、最短 keyword、fallback 和
score offset。[blink.cmp sources][blink-sources] source contract 还支持
流式批次、incomplete 标记、取消函数、lazy resolve 和 execute hook。
[blink.cmp source interface][blink-source]

blink.cmp 的 Rust matcher 支持大列表、Unicode、proximity 和 frecency；
其文档也明确指出自定义 Lua comparator 会使排序退回 Lua，并影响大列表
性能。[blink.cmp fuzzy][blink-fuzzy]

Vell 采用：

- 流水线和 source cancellation；
- Rust 内完成匹配、排序、top-K 和 frecency；
- 每来源 timeout、item cap、最短 query、fallback 与 score bias；
- 对选中项按需 resolve，而不是预先填充所有文档。

Vell 的差异：

- 所有结果必须经过 Content/View/revision/request epoch 校验；
- 接受候选必须进入 `ExecutionFrame`，而不是由 source execute 自行写文本；
- 脚本 bulk hook 只在批次进入 Rust 前执行一次，不能接管 Rust 排序热路径；
- popup 是 View 的瞬态派生呈现，不是 Neovim window 或 Vell Pane。

## 4. 领域模型

### 4.1 核心术语

**Completion source**：产生候选的扩展贡献。它属于已附加的
Content-bound Mode adapter，不拥有 View、Content、菜单或 history。

**Completion session**：一个 View 在一个文档 revision、selection 和查询
范围上的交互状态。它包含 request epoch、来源批次、匹配结果、选择和
presentation cache。

**Request epoch**：每次需要向来源重新请求时递增的 session-local identity。
异步结果只有完整 identity 仍匹配时才能安装。

**Completion request**：给 source 的 owned 查询快照，包含触发原因、文档和
View identity、revision、cursor、替换范围、当前 query 及受限上下文。

**Completion batch**：一个 source 对一个 request epoch 返回的候选集合。
批次可以 replace 自己的旧结果，后续阶段再开放有序 append streaming。

**Completion item**：来源语义候选。它包含 filter/sort/insert 字段和 provider
payload，但不包含最终 fuzzy score、屏幕坐标或 TUI cell。

**Matched item**：引擎为当前 query 计算出的候选视图，包含稳定 CandidateId、
match positions、各排序分量和显示分段。

**Resolved item**：只为当前选中或即将接受的 item 补齐 detail、documentation、
additional edits 或 command 后的结果。

**Completion acceptance**：由引擎冻结的接受计划，携带原 revision、range、
主 edit、额外 edit、snippet 数据和接受后通知。App 重新校验后才能执行。

### 4.2 稳定 identity

最小 identity 链为：

```text
CompletionSessionId
└── RequestEpoch
    └── SourceKey
        └── SourceBatchVersion
            └── CandidateId
```

`SourceKey` 是 `vell-completion` 中的 opaque key。App 从
`(ModeId, source-local id)` 派生它，并另外保存 Mode origin；这样
`vell-completion` 不反向依赖 `vell-mode`。禁止使用 label、数组位置或 LSP
`sortText` 作为 CandidateId。来源未提供稳定 key 时，adapter 对同一批次分配
ordinal；批次被替换后旧 identity 立即失效。

### 4.3 候选最小数据

内部 item 至少表达：

```rust
pub struct CompletionItem {
    pub label: String,
    pub label_detail: Option<String>,
    pub kind: Option<CompletionKind>,
    pub filter_text: Option<String>,
    pub sort_text: Option<String>,
    pub insert: CompletionInsert,
    pub tags: Vec<CompletionTag>,
    pub commit_characters: Vec<char>,
    pub preselect: bool,
    pub source_bias: i32,
    pub data: CompletionData,
}
```

代码只用于固定能力集合，不预先承诺最终字段名。正式实现前必须同时对照
LSP `CompletionItem`、TypeScript schema、大小预算和 UTF-16 转换规则。

## 5. 目标模块与依赖方向

### 5.1 新增深模块 `vell-completion`

新增一个无业务 IO 的 library crate：

```text
vell-completion -> vell-protocol
vell-mode       -> vell-completion + vell-core + vell-protocol
vell-app        -> vell-completion + vell-mode + vell-core
                   + vell-frontend + vell-protocol
vell-plugin-v8  -> vell-completion + vell-mode + vell-core
                   + vell-protocol
vell-tui        -> vell-frontend + vell-protocol
```

`vell-completion` 是一个深模块。它用小 interface 隐藏 session reducer、批次
归并、匹配、排序、增量缓存、top-K、selection stability 和接受计划生成。
其外部 interface 以事件和结果为主：

```rust
impl CompletionEngine {
    fn transition(&mut self, event: CompletionEvent) -> CompletionEffects;
    fn snapshot(&self, view: ViewId) -> Option<CompletionSnapshot>;
}
```

真实类型应经 M0 原型和测试收敛。调用方不得读取引擎内部 source map、score
cache 或 matcher scratch buffer；这些是内部 seam。

删除 `vell-completion` 后，如果 request identity、增量筛选、稳定选择、top-K
和接受校验会重新散落到 app、Mode、V8 与 TUI，则该模块通过 deletion test。

### 5.2 各 crate 职责

`vell-protocol`：

- 定义 Frontend 可见的 owned `CompletionPresentation`；
- 定义 anchor、menu rows、match spans、selection 和 documentation DTO；
- 扩展 fallible `RenderQuery`，不保存 matcher 或 provider payload；
- 保持零业务 IO、零 V8、零 app 依赖。

`vell-completion`：

- 定义 source-neutral request、item、batch、resolve 与 acceptance 数据；
- 拥有 session 状态机、匹配、排序、缓存、限额和统计；
- 不读取 ContentStore，不启动 task，不调用 Mode 或 Frontend；
- 提供纯内存 fake/event 测试和 Criterion benchmark。

`vell-mode`：

- 在 Buffer adapter 上声明零个或多个 completion source；
- 定义 source callback 的 typed adapter 和 fault phase；
- 从 Mode content/view state 只读创建 owned source request/task；
- 不拥有跨来源 session、全局排序或 popup。

`vell-app`：

- `Kernel` 持有 source definition 及跨 View 可复用的 provider runtime；
- `ClientSession` 持有 `CompletionEngine` 和每 View 的交互状态；
- 从当前 Mode chain 得到有序 source 集合，不建立第二个语言路由器；
- 安排 debounce、task、取消、AppMessage 和 revision 校验；
- 把 accept 转成 typed operation，并在 `ExecutionFrame` 中执行；
- 通过 `AppQuery` 暴露 cached presentation。

`vell-plugin-v8`：

- 把 TypeScript source definition 适配到通用 Mode completion contract；
- 在 V8 边界一次校验并转换整个 batch；
- 对 item 数、字符串、payload、resolve 和 callback 设置独立预算；
- 不在 fuzzy、排序、top-K 或 render 时回调 JavaScript。

`vell-tui`：

- 根据 body Pane 的 cursor、viewport 和最终 Rect 放置菜单；
- 在上下空间中选择方向，裁剪宽高并绘制 match spans；
- menu 和 documentation 最后绘制，不改变 Scene 或 viewport owner；
- 不读取 provider payload，不重排候选，不调用 app。

### 5.3 所有权

```text
Kernel
├── ModeRegistry + source definitions
├── provider runtimes and reusable indexes
└── completion task table

ClientSession
├── CompletionEngine
│   └── ViewId -> optional CompletionSession
├── current source order from Mode chains
└── cached CompletionPresentation

Frontend
└── geometry, clipping and paint only
```

completion session 是 View 交互状态，不属于 Content。共享 Content 的两个
View 可以有不同 cursor、query、selected item 和菜单；provider index 可按
`(SourceKey, ContentId, revision)` 共享，但不得共享 session selection。

## 6. Source interface

### 6.1 来源随 Mode attachment 生效

只有当前 View 已附加的 Content-bound Mode source 才参与文档补全。source
顺序首先来自 `ModeResolver` 的 Mode chain，再应用 source-local order 和用户
配置。不要新增按文件名扫描 provider 的第二套 attachment 规则。

View-only Mode 没有 document Content context，不能注册文档 source。未来
出现非文档补全时，应为真实的第二种上下文设计新 adapter，而不是伪造
ContentId。

### 6.2 来源行为

source interface 需要表达：

- 静态 id、category、trigger characters 和能力；
- `enabled` 的轻量上下文判断；
- `start(request)` 返回 skip、ready batch 或 cancellable task；
- 可选 `resolve(candidate)`；
- 可选 `accepted(candidate, outcome)` 通知；
- 可选 streaming append，但每个 chunk 必须带顺序和最终标记。

source 不得：

- 直接写 Content、View、Mode draft、Scene 或 Frontend；
- 返回借用 buffer/V8 handle/Promise 的对象；
- 根据当前 query 预过滤完整集合，除非协议语义标记结果 incomplete；
- 保留调用期 context；
- 通过 execute hook 绕过统一 acceptance。

### 6.3 请求快照

请求至少携带：

- `ContentId`、`ViewId`、Content revision、View revision；
- primary selection 的 `head` 和是否 collapsed；
- query range、query text、trigger kind 和可选 trigger character；
- language classification、resource name/path 的 owned 值；
- 当前行或有界 cursor context；
- request epoch 和 cancellation token 所属 key。

不能默认把全文复制到每个 TypeScript source。native source 可以持有廉价
`TextSnapshot`；script source 只得到有界文本上下文。需要全文的扩展应通过
content-change/Worker 建立增量 index，再在请求时查询 index。

### 6.4 批次规则

- 每个 batch 必须有 item cap 和总字节 cap；
- item 默认由引擎匹配，source 不按 query 删除 complete list 中的候选；
- `is_incomplete_forward/backward` 决定 query 变化时重筛还是重请求；
- replace 只替换同一 SourceKey 的批次；
- append chunk 必须单调递增且不能在 final 后继续发送；
- source error 只结束该 source，不关闭其他来源或编辑器；
- 超限批次整体拒绝，不安装截断到语义不明的半个 item。

### 6.5 配置策略

每个 source 可以配置：

- 自动/手动启用；
- 自动触发最短 query；
- trigger characters；
- debounce、soft deadline 和 hard timeout；
- item cap、group、source bias；
- fallback dependencies；
- 是否允许 ghost text、documentation 和 preselect。

fallback 是来源依赖图，不是串行全局等待。只有上游 definitive empty、禁用、
超时或按配置失败时才激活下游。图必须在启动期检查未知节点和环。

### 6.6 TypeScript 目标接口

建议把 source definition 放进 Buffer Mode adapter，复用既有 attachment 和
owner。TypeScript 请求必须在独立 provider Worker 中执行，而不是在事件循环的
主 isolate 上调用任意脚本：

```ts
const words = new URL("./words-completion.ts", import.meta.url);

editor.modes.define({
  name: "words",
  attach: {
    view: "core.buffer",
    binding: "document",
  },
  on: {
    buffer: {
      completion: {
        sources: {
          words: {
            worker: words,
            triggerCharacters: ["_"],
            options: { minimumLength: 2 },
          },
        },
      },
    },
  },
});
```

worker module 接收 owned request/options，并导出 `complete`、可选 `resolve`
和 `accepted`。同一 source 最多有一个运行请求和一个 latest-only queued
request；hard timeout 可以终止并重建该 provider Worker，而不终止共享
ScriptHost。具体导出名和消息格式在 M4 冻结。

这只是 interface 方向，不是待直接复制的最终 declaration。M4 必须先用两个
native adapter 验证 seam，再冻结 TypeScript 名称。独立 Worker 会让 source
失去直接访问 Mode state 的能力；需要的配置必须在注册时以 `ScriptData`
传入，需要的文档事实必须来自 owned request 或增量消息。这是保证输入隔离的
有意取舍。

脚本扩展性采用两档：

- hot-path-safe：声明式 source bias、group、limits、排序键和 display fields；
- bounded bulk hook：每批一次 transform，或选中项一次 resolve/accepted。

明确不提供逐候选 JavaScript predicate、pairwise comparator 或 render
callback。扩展需要自定义排序时，返回预计算 `sortText`、`source_bias` 或
有限的声明式排序字段。

## 7. 触发、并发与状态机

### 7.1 状态机

```text
Closed
  └── trigger -> Collecting(epoch N)
                    ├── first batch -> Visible(epoch N)
                    ├── query extends -> Refiltering(epoch N)
                    ├── incomplete -> Collecting(epoch N + 1)
                    ├── selection -> Resolving(candidate)
                    ├── accept -> Accepting(candidate)
                    └── cancel/stale -> Closed
```

`Visible` 和 `Collecting` 可以同时成立：菜单已显示时，慢来源仍可返回。实际
实现可以使用 flags/reducer，而不必照抄枚举嵌套；可观察转移必须与上图一致。

### 7.2 不阻塞输入

自动补全只在成功的 `ExecutionFrame` 提交后观察 Content/selection 变化：

```text
physical key
-> existing Mode/input frame
-> commit text and selection
-> derive CompletionEvent from committed snapshot
-> schedule source work
-> return to event loop
```

来源 callback、LSP 请求、全文索引和 fuzzy 大列表不得进入物理按键的同步
frame。当前 query 的小规模增量重筛可以同步执行，但必须受 0.5 ms 输入预算；
超过预算时转为 cancellable task，并继续显示上一 snapshot 或关闭菜单。

### 7.3 触发原因

支持：

- manual invoke；
- identifier typing；
- source trigger character；
- incomplete list retrigger；
- delete/backspace retrigger；
- 显式 refresh。

manual invoke 不受最短 query 限制。trigger character 只激活声明该字符的来源；
identifier typing 可以激活普通自动来源。paste、undo/redo、selection movement、
focus、rebind、switch 和 close 各有显式策略，不通过“文本似乎变了”猜测。

### 7.4 debounce 与 deadline

- debounce 按 source 配置，不使用一个全局延迟；
- 本地缓存来源默认零 debounce；
- LSP/外部来源可以短 debounce，manual/trigger character 可绕过；
- 同一 source 只保留一个 running 和最新 queued request，不能积压每次按键；
- soft deadline 只影响 fallback 和 loading 状态，不阻止已有候选显示；
- hard timeout 取消 source request，并记录结构化诊断；
- 新 epoch 立即取消旧 task，不等待 provider 合作后才更新 UI。

### 7.5 结果安装门禁

安装 batch、resolve 或 accepted result 前必须校验：

- session 仍存在；
- `ViewId` 仍存在且仍绑定原 `ContentId`；
- Content revision、View revision 和 selection signature 匹配；
- request epoch、SourceKey 和 batch sequence 匹配；
- source 的 Mode 仍附加且没有 fault；
- payload、item 和 edit 仍满足大小与范围约束。

任一条件失败都静默丢弃结果并更新计数器；不能把结果改投当前焦点 View。

## 8. 匹配、排序与缓存

### 8.1 单一 Rust pipeline

```text
source batch
-> normalize once
-> deduplicate policy
-> query-aware filter
-> fuzzy score and match positions
-> source/context bonuses
-> deterministic total order
-> top-K
-> presentation rows
```

同一 item 的 label、filter text、case-folded key、字符边界和 display segments
只预处理一次。每次 query 变化不重复跨 V8 转换、Unicode 分段或 owned
字符串复制。

### 8.2 匹配策略

M2 先实现正确的 Unicode-aware fuzzy subsequence matcher，并保留 ASCII 快速
路径。M3 通过 benchmark 决定采用 Nucleo、Frizbee 或仓内实现；不得只因
blink.cmp 使用 SIMD 就提前锁定依赖。

默认行为：

- smart case；
- exact prefix、word/camel/snake boundary bonus；
- 连续 match bonus 和 gap penalty；
- 可配置但有上限的 typo resistance；
- 返回 match character positions，供 TUI 高亮；
- 空 query 只按 source 与 sort key 排序，不做模糊扫描。

### 8.3 确定性排序

默认 total order 建议为：

1. 是否有效 exact/prefix match；
2. fuzzy score；
3. provider `sortText` 所表达的上下文顺序；
4. source bias；
5. frecency；
6. proximity；
7. normalized label；
8. SourceKey 与 source ordinal。

具体优先级由 M0 corpus 校准。必须有最终稳定 tie-breaker，异步来源的返回
时序不得让相同输入产生随机顺序。

用户可以从有限内建 sort keys 组成顺序，但不能在热路径注入任意脚本
comparator。来源若掌握更好语义，应使用 `sortText`，而不是要求用户按语言
编写比较器。

### 8.4 增量筛选

当 query 在同一 range 内向后扩展，source batch 未变化且不是 incomplete 时，
可以只筛上一轮 matched set。删除字符时从该 batch 最近的缓存 checkpoint
恢复；range、context、source version 或 normalization 变化时从完整 batch
重算。

缓存键至少包含：

```text
(SourceKey, batch version, query, matcher configuration revision)
```

不能只用 query 字符串，否则共享 Content、配置变化和新批次会互相污染。

### 8.5 top-K 与资源限制

- matcher 不为所有候选构造完整 presentation；
- 使用有界 top-K 或经 benchmark 证明更快的 partial selection；
- menu 初始最多 materialize 100 项，向下翻页可按需扩大到硬上限；
- 每 source 和整 session 都有 item/byte 上限；
- documentation、provider payload 和 snippet body 分别计入预算；
- session 关闭或 batch 替换立即释放旧字符串与 scratch buffer。

### 8.6 frecency 与 proximity

frecency 只记录“source + semantic key 被接受”，不记录 provider 私有 payload。
存储更新不得阻塞接受 frame；M3 先使用 session 内存统计，持久化在格式、隐私、
淘汰和损坏恢复明确后再开放。

proximity 只对有可信 location 的 item 生效。没有 location 的来源得到中性值，
不能通过伪造当前行取得无限 bonus。

## 9. UI、输入与 presentation

### 9.1 popup 不是 View 或 Pane

补全菜单是一个 View 的瞬态派生呈现：

- 没有 ViewId、ContentId、PaneKey、SpaceId 或 Mode attachment；
- 不进入 Scene tree，不参与 split、focus、switch 或 close；
- anchor 是 body Pane 中的文本位置；
- View 关闭、失焦、rebind 或 session cancel 时自然消失。

`vell-protocol` 增加 completion-specific owned presentation，而不是过早建立
通用 overlay 平台。真实出现第二类共享瞬态呈现后，再用两个 adapter 验证
通用 seam。

### 9.2 presentation 内容

`CompletionPresentation` 至少包含：

- anchor View/Space 和文本位置；
- 有界 menu rows；
- selected CandidateId 和可见 index；
- label、kind、detail、source/group、deprecated 状态；
- fuzzy match spans；
- loading/incomplete/source fault 摘要；
- 可选已 resolve 的 documentation；
- 可选 ghost text，后续阶段启用。

protocol 不暴露 fuzzy raw score、frecency 数据库 key 或 provider payload。

### 9.3 TUI 布局

`SceneRenderer` 使用已经解析的 body Rect、ViewId viewport、cursor screen row
和 cell width：

1. 优先放在 cursor 下方；
2. 下方不足时比较上下可用行数；
3. 宽度按 Unicode cell 计算并限制在 body/terminal Rect；
4. menu、scroll indicator 和 documentation 均裁剪；
5. 清除上一帧 overlay 遗留 cell；
6. popup 最后绘制，不能修改正文 viewport。

窄终端必须退化为单列 label；kind、detail、source 和 documentation 按优先级
依次隐藏，不能覆盖 cursor 或越界写 Canvas。

### 9.4 输入优先级

popup 打开时，`CompletionInteractor` 位于现有 Mode chain 之前，但只消费
明确绑定的瞬态命令：

- next/previous/page；
- accept/accept-and-insert-character；
- cancel；
- show/hide documentation；
- manual refresh。

普通字符、Backspace 和 Mode-specific key 仍先执行正常输入 frame，再由
提交后的事件更新 completion session。这样 Vim Mode 不需要知道 popup，
completion 也不会重实现 Vim insert state。

Tab、Enter、Ctrl-N/P 等默认键必须可配置，并有 fallback 语义。例如没有
selected item 时，Tab 继续交给下一层，而不是无条件吞掉。

### 9.5 稳定选择

异步 batch 到达时，若当前 CandidateId 仍存在，保持选择；否则选择最接近的
下一项或回到“无选择”。禁止只按 index 保持，因为插入高分候选会导致用户
在接受瞬间得到另一项。

## 10. 接受、编辑与 history

### 10.1 接受必须原子

接受候选产生一个 typed `CompletionOperation::Accept`。App 在当前
`ExecutionFrame` 内：

1. 按 CandidateId 取得冻结的 acceptance；
2. 重新校验 View、Content、revision、selection 和 edit ranges；
3. 将主 edit、additional edits 和 snippet 初始 edit 组成一个
   `TextChangeSet`；
4. 一次应用文本和 selection 映射；
5. 成功后关闭 session；
6. frame 提交后发送 source accepted 通知或 LSP command。

任一 edit 重叠、越界、UTF-16 非法或 stale 时，整个 accept 失败且不修改
Content、history、selection 或 source state。不得退化为插入 label。

### 10.2 insert 与 replace

引擎保留 insert range 和 replace range。用户命令决定采用 insert 还是 replace，
source 的显式 text edit 优先于 label/insert text 猜测。没有 edit 的简单来源
只能替换 session 建立时冻结的 query range，不能在接受时重新猜词边界。

### 10.3 additional edits

additional edits 与主 edit 必须同 revision、互不重叠，并由 core 的 batch edit
验证一次应用。首版只接受当前 Content 的额外 edit；跨文件 workspace edit
属于独立 roadmap，不能偷偷塞进 completion accept。

### 10.4 commit characters

用户输入 commit character 时：

- 当前 item 声明该字符才先接受；
- 接受与字符输入属于同一物理输入的同一 `ExecutionFrame`；
- 接受失败时字符是否继续输入由明确配置决定，默认保留普通输入；
- 未选 item 或字符不匹配时直接交给正常 Mode/input chain。

### 10.5 snippet

snippet 不是字符串替换的别名。支持 LSP snippet 前必须先建立原生 snippet
session，拥有 placeholder、linked edit、Tab traversal、cancel 和 history
语义。补全只负责用一次 acceptance 启动 snippet session。

首版不宣称 `snippetSupport: true`。在 M6 完成前，LSP capability 必须关闭
snippet，或只接受服务器明确返回的 PlainText。

### 10.6 接受后动作

provider accepted hook 和 LSP command 在文本 frame 成功提交后运行。它们不能
回滚已提交文本，也不能直接借用 App。需要宿主 mutation 时，必须产生新的
显式 command/operation，并接受新的 revision 校验。

## 11. LSP completion adapter

LSP 是 source adapter，不是补全内核。LSP 进程、document sync、capability
协商、JSON-RPC request/cancel 和 position encoding 属于独立 runtime；它把
协议结果转换为通用 CompletionBatch。

LSP 3.18 明确由 client 负责过滤和排序，允许 server 用 `filterText`、
`sortText` 调整；complete list 可用 `isIncomplete` 要求继续输入时重请求，
昂贵字段可通过 `completionItem/resolve` 延迟获取。
[LSP completion][lsp-completion]

### 11.1 必须支持的协议语义

- capability negotiation 和动态/静态 completion provider；
- manual、trigger character、incomplete retrigger；
- 多 client 独立 source identity 和确定性合并；
- `CompletionItem[]` 与 `CompletionList`；
- `itemDefaults` 展开；
- label details、kind、tags、preselect；
- `filterText`、`sortText`、`insertText`、`textEditText`；
- TextEdit 与 InsertReplaceEdit；
- PlainText 与完成 M6 后的 Snippet；
- insert text mode；
- additionalTextEdits、commitCharacters、command、data；
- resolve capability 和 `$ /cancelRequest`；
- UTF-8/UTF-16/UTF-32 position encoding 的显式转换。

### 11.2 不得过早宣告的 capability

Vell 只有在接受和测试完整语义后才向 server 宣告：

- snippet support；
- insert/replace support；
- commit character support；
- resolve properties；
- completion list item defaults；
- insert text mode。

错误宣告会让 server 合法返回客户端无法解释的数据，不能靠忽略字段掩盖。

### 11.3 多 server 合并

每个 LSP client 是独立 SourceKey。一个 server 超时、返回 incomplete 或 fault
不改变其他 server 的 batch。server 顺序只提供 source bias；相同 item 的
去重策略在统一引擎中按 semantic key 执行，并保留获胜 item 的接受 payload。

## 12. 故障、诊断与可观测性

### 12.1 故障隔离

- source callback 失败只 fault 该 SourceKey 的本次请求；
- 连续 fault 可按退避策略暂时禁用自动触发，manual 仍可探测；
- matcher 或 session invariant 失败属于宿主 bug，测试中 panic，生产中关闭
  session 并记录诊断；
- invalid batch、resolve 或 edit 是 provider contract error；
- TUI geometry 错误不得损坏 session 或 Content。

### 12.2 诊断数据

为每个 request 记录有界 ring buffer：

- trigger、query length 和 active source count；
- debounce、queue、provider、merge、match、top-K、render 各阶段耗时；
- 每 source item/byte 数、first/final result、timeout、cancel、fault；
- cache hit、full rescan、incremental rescan；
- stale result 丢弃原因；
- selected、resolved、accepted、accept rejected；
- session peak memory。

不得把用户源码、完整 label、documentation 或 provider data 默认写入日志。

### 12.3 用户可见诊断

增加只读命令，例如 `completion.status` 和 `completion.traceLast`，显示 active
sources、能力、延迟、超时和最近拒绝原因。命令读取 owned diagnostic
snapshot，不暴露内部 mutable engine。

## 13. 实施阶段

### M0：冻结语义、数据集与基线

目标：在写 popup 前定义可测 interface 和性能事实。

工作：

- 建立 `vell-completion` crate 和事件驱动 engine prototype；
- 固定术语、identity、session state、batch/resolve/accept invariants；
- 制作 1k、10k、100k ASCII/Unicode/长标识符 corpus；
- 记录空闲输入、普通插入、Mode chain 和 render 的现有延迟；
- 建立 matcher、top-K、batch conversion 和 memory benchmark；
- 用 in-memory fake source 覆盖乱序、取消、timeout 和 streaming；
- 更新 crate dependency 文档与 workspace 列表。

验收：

- engine 测试只通过 `transition/snapshot` interface；
- 基线可重复，文档记录机器与命令；
- 没有 TUI、V8、LSP 或 ContentStore 依赖进入 `vell-completion`；
- 性能预算有测量结果，不以主观“很快”代替。

### M1：session 生命周期与并发调度

目标：来源可以异步返回，旧结果永远不能安装。

工作：

- 在 Buffer Mode adapter 增加 completion source contract；
- 从 Mode chain 派生活跃 SourceKey；
- 在 ClientSession 接入 `CompletionEngine`；
- 建立 per-source debounce、task key、CancellationToken 和 AppMessage；
- 覆盖 edit、selection、focus、rebind、switch、close 的 cancel/retrigger；
- 增加结构化 source diagnostics。

验收：

- 输入 frame 不运行 source；
- 两个 fake source 可并行且快来源先安装；
- 旧 revision、旧 epoch、旧 Mode attachment 结果全部拒绝；
- View 关闭后没有 task、session 或 provider payload 泄漏。

### M2：最小可用补全

目标：单 cursor 下完成 manual/automatic buffer word completion。

工作：

- 实现 native buffer-word index source；
- 实现 Unicode-correct matcher、稳定 total order 和 top-K；
- 增加 `CompletionPresentation` 和 RenderQuery；
- TUI 绘制 menu、selection、match spans 和 loading state；
- 增加 next/previous/accept/cancel/manual trigger 命令；
- accept 简单 PlainText item，并进入一个 ExecutionFrame/history record。

验收：

- 一个 collapsed primary selection 可完整使用；
- 自动补全不改变正常字符、Backspace 或 Vim Mode 语义；
- 同一 Content 两个 View 的 session 独立；
- 所有 M0 性能预算通过；
- menu 在窄终端、滚动、宽字符和屏幕边缘正确裁剪。

### M3：增量匹配、缓存与排序质量

目标：大候选集在连续输入中保持稳定低延迟。

工作：

- 实现 query extension 增量筛选和 delete checkpoint；
- 通过 benchmark 选择/优化 fuzzy matcher；
- 加入 smart case、boundary、typo、match spans；
- 加入 source bias、`sortText`、session frecency 和可信 proximity；
- 实现有界去重、per-source/session caps 和 memory accounting；
- 用 replay corpus 建立排序 golden tests。

验收：

- 10k/100k budget 达标；
- 相同输入和 batch 与异步到达顺序无关；
- selected CandidateId 在合并后保持；
- 配置 revision 变化不会复用错误 cache；
- 没有逐项 V8 或 TUI callback。

### M4：第二来源、streaming 与 TypeScript 扩展

目标：用真实的第二种 adapter 证明 source seam，并开放安全脚本 contract。

工作：

- 实现 native path source，与 buffer-word source 共同验证 seam；
- 开放 replace/append streaming 和 incomplete forward/backward；
- 实现 fallback dependency graph；
- 在 `runtime/editor.d.ts` 增加 source definition；
- 建立独立 provider Worker、latest-only queue 和 request termination；
- V8 实现 owned batch conversion、bulk transform、resolve 和 accepted；
- 为 callback、batch、payload 和字符串设置独立预算；
- 增加 TypeScript 契约、module rollback 和 owner unload 测试。

验收：

- native 与 script source 使用同一 session、matcher 和 presentation；
- JavaScript comparator/predicate/render 不存在于公开 interface；
- script timeout/fault 不影响其他来源和输入；
- 插件卸载会取消 task 并移除自己的 batch；
- `pnpm typecheck` 覆盖示例和负向契约。

### M5：documentation、resolve 与丰富 item

目标：只为用户关注的 item 支付昂贵信息成本。

工作：

- 支持 selection debounce 后 lazy resolve；
- 对 resolve 使用独立 epoch/cancellation/cache；
- 显示 label details、kind、source、deprecated 和 documentation；
- 处理 resolve 前后允许变化的字段白名单；
- 支持 PlainText TextEdit、InsertReplaceEdit、additional edits 和
  commit characters；
- 扩展 acceptance stale/overlap/UTF-16 测试。

验收：

- 快速移动选择不会发起无界 resolve；
- 旧选择 resolve 不能覆盖当前 documentation；
- main/additional edits 一次成功或一次失败；
- commit character 与原字符处于同一输入 frame；
- render path 只读取 owned resolved presentation。

### M6：LSP completion 与 snippet

目标：完整而诚实地支持 LSP completion 语义。

工作：

- 建立可复用 LSP runtime、document sync、request/cancel 和编码转换；
- 实现 LSP completion source、capability negotiation 和多 server 合并；
- 覆盖 CompletionList、itemDefaults、isIncomplete 和 resolve；
- 建立原生 snippet session 与 placeholder/linked edit/navigation；
- 支持 insert text mode、snippet item 和接受后 LSP command；
- 只在各能力完成后逐项向 server 宣告。

验收：

- 使用至少 rust-analyzer、typescript-language-server 和 clangd 做集成测试；
- UTF-8/UTF-16、emoji、组合字符和非 BMP edit 正确；
- server cancel、退出、重启和慢响应不冻结输入；
- snippet 的一次接受只产生一个文本 history entry；
- LSP command 不绕过 command/operation 边界。

### M7：多 selection、ghost text 与体验完善

目标：在已有正确模型上增加高级交互，不改变 source interface。

工作：

- 定义多 selection compatibility 和 atomic accept；
- 只有所有 cursor range 与 item 语义兼容时显示/接受；
- 增加可选 ghost text presentation；
- 完成 page、scroll、documentation toggle 和 accessibility fallback；
- 持久 frecency 前完成隐私、版本、淘汰和损坏恢复设计；
- 增加用户配置和 `docs/scripting.md`。

验收：

- 任一 cursor stale 会拒绝整个多 selection accept；
- ghost text 不进入 Content、selection 或 history；
- 关闭 ghost text 不影响 menu/source；
- 持久统计损坏时安全退回空数据库。

### M8：压力、远程语义与完成门禁

目标：在大文件、慢 provider 和不同 Frontend 下关闭系统性风险。

工作：

- 大文件、100k item、连续输入、快速 focus/switch soak tests；
- task cancellation、Mode unload、LSP restart 和 shutdown leak tests；
- fuzz batch schema、edit ranges、snippet parser 和 state transitions；
- 为远程语义消息增加 completion presentation/request identity；
- 记录性能 flamegraph、heap peak 和回退阈值；
- 更新架构、脚本、用户配置和故障排查文档。

验收：

- 全部正确性与性能门禁通过；
- 本地 TUI 与 remote semantic adapter 对相同 snapshot 得到等价菜单；
- 取消或关闭后没有后台结果复活 UI；
- 文档只描述已经支持的 capability。

## 14. 测试矩阵

### `vell-completion`

- state transition、epoch、乱序和取消；
- replace/append/incomplete/fallback；
- Unicode、smart case、typo、match spans；
- deterministic sort、top-K、dedupe、selection stability；
- incremental/full rescan 等价性；
- item/byte/memory limits；
- acceptance 生成和 stale rejection；
- corpus benchmark 与相对性能门禁。

### `vell-mode`

- source 随 attachment 安装、重排和卸载；
- Content-bound capability 与 View-only 拒绝；
- state/context snapshot、fault phase 和 owner；
- source 不能返回宿主 operation 或借用对象。

### `vell-app`

- post-frame trigger 与输入不阻塞；
- task/debounce/timeout/cancel；
- View/Content/revision/selection/source identity 门禁；
- split、focus、rebind、switch、close、undo/redo、paste；
- 同 Content 多 View session 隔离；
- accept 的 Content、selection、history 和 rollback；
- lazy resolve 与 accepted notification 顺序。

### `vell-protocol` 与 remote

- owned presentation 和结构化 query error；
- match spans、selected id、documentation 和 loading 状态；
- serialization limits、unknown enum 和 revision/request identity；
- remote request/response 的 stale 与 error 语义。

### `vell-tui`

- 上下 placement、窄/矮 Rect 和 terminal edge；
- Unicode cell width、tab、horizontal/vertical scroll；
- menu selection、match face、deprecated 和 source columns；
- documentation、scroll indicator、clear/repaint；
- overlay 不改变 viewport、Scene 或 focus。

### `vell-plugin-v8` 与 `runtime`

- TypeScript source schema 与声明一致；
- batch/payload/string/callback budgets；
- module rollback、unload、timeout 和 fault isolation；
- bulk transform、resolve、accepted；
- 禁止逐项 filter/comparator/render callback；
- Worker/index 与有界 request context。

### LSP integration

- capability matrix；
- trigger kinds、multiple clients、incomplete list；
- item defaults、resolve、insert/replace、additional edits；
- commit characters、snippet、command；
- position encoding 和 invalid server payload；
- cancellation、timeout、restart 和 stale response。

## 15. 关键不变量

- source 不在物理按键 frame 中执行；
- 匹配、排序、top-K 和 menu materialization 只有一套 Rust 实现；
- ModeResolver 仍是 source attachment 顺序的唯一语言/行为 owner；
- completion session 按 View 隔离，provider index 才可以按 Content 共享；
- 每个异步结果都经过 revision、epoch、source 和 sequence 校验；
- 慢来源不能阻止快来源显示；
- source 不直接写 Content、View、history、Scene 或 Frontend；
- accept 只能通过 typed operation 和 `ExecutionFrame` 修改文本；
- popup 是派生呈现，不是 Content、View、Pane 或 Scene node；
- render path 不调用 Mode、V8、LSP、Worker 或 matcher；
- TypeScript 不参与逐候选热路径；
- capability 只有在完整实现并测试后才能对 LSP 宣告；
- 所有集合、字符串、payload、task、时间和内存都有上限。

## 16. 完成标准

- buffer words、第二 native source、TypeScript source 和 LSP source 共用同一
  source interface；
- manual/automatic、streaming、fallback、resolve、accept 和 cancel 可组合；
- 10k/100k 性能预算和回退门禁稳定通过；
- stale、取消、关闭、rebind 和 Mode unload 结果无法复活 session；
- PlainText、insert/replace、additional edits、commit characters 与 snippet
  有原子 history 语义；
- TUI 和 remote semantic path 只消费 owned presentation；
- `cargo fmt --all -- --check` 通过；
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` 通过；
- `cargo test --workspace --all-features` 通过；
- `cargo doc --workspace --all-features --no-deps` 通过；
- `pnpm typecheck` 通过；
- Markdown 行长、相对链接和文档中的 capability 声明通过检查。

## 17. 一手参考资料

- [Neovim insert completion][nvim-insert]
- [Neovim LSP completion][nvim-lsp]
- [Emacs completion in buffers][emacs-capf]
- [Emacs programmed completion][emacs-programmed]
- [blink.cmp architecture][blink-architecture]
- [blink.cmp sources][blink-sources]
- [blink.cmp source interface][blink-source]
- [blink.cmp fuzzy matcher][blink-fuzzy]
- [LSP 3.18 completion][lsp-completion]

更细的上游实现与源码定位见
[补全架构一手资料调研](../research/completion-architecture.md)。

[nvim-insert]: https://neovim.io/doc/user/insert.html#ins-completion
[nvim-lsp]: https://neovim.io/doc/user/lsp.html#lsp-completion
[emacs-capf]:
https://www.gnu.org/s/emacs/manual/html_node/elisp/Completion-in-Buffers.html

[emacs-programmed]:
https://www.gnu.org/s/emacs/manual/html_node/elisp/Programmed-Completion.html
[blink-architecture]: https://cmp.saghen.dev/development/architecture
[blink-sources]: https://cmp.saghen.dev/configuration/sources
[blink-source]: https://cmp.saghen.dev/development/source-boilerplate
[blink-fuzzy]: https://cmp.saghen.dev/configuration/fuzzy
[lsp-completion]: https://microsoft.github.io/language-server-protocol/
