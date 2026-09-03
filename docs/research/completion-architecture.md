# 补全架构一手资料调研

本文为 Vell 补全 roadmap 提供事实依据。调研日期为 2026-08-13。
只使用上游官方手册、规范、仓库文档与源码，不使用博客、评测或第三方
解读。

## 1. 版本与结论边界

- Neovim 部分以当前在线帮助和官方仓库 `master` 的
  `vim.lsp.completion` 为准。在线帮助可能包含尚未进入旧稳定版的行为。
- Emacs 部分以当前 GNU 手册和官方镜像 `master` 的 Eglot 源码为准。
- blink.cmp 部分以稳定版 v1.10.2 文档为主。官方 README 明确提示 v2
  仍在开发并有破坏性变更，因此不把 v2 的接口形状作为稳定前提。
- LSP 语义以 3.18 completion 规范为准。
- 文中“建议”和“推论”是面向 Vell 的设计判断；其余陈述均可由紧邻的
  一手资料链接核对。

## 2. 直接结论

Vell 不应照搬三者之一，而应组合它们各自最强的部分：

1. 采用 Emacs 的“提供者与策略分离”：提供者只描述范围、候选和元数据，
   匹配、排序、呈现、接受策略可以独立替换。
2. 采用 blink.cmp 的显式四段管线：触发、sources、fuzzy、render，并让
   source 契约原生支持流式回调、取消、超时、延迟 resolve 和 fallback。
3. 采用 Neovim LSP 实现中的请求生命周期：新请求取消旧请求，结果安装前
   校验编辑上下文，`isIncomplete` 才重查，文档 resolve 独立去抖。
4. 把过滤和排序放入 Rust 热路径，候选保持稳定 ID 和紧凑索引；插件只在
   source、元数据、批量变换和声明式呈现等受控接缝上运行。
5. 接受候选不是“插入一个字符串”，而是一次可回滚的编辑事务：主编辑、
   snippet 和 `additionalTextEdits` 必须原子提交；外部命令和接受后钩子只在
   提交后运行，并以 revision 或 generation 防止过期异步结果落地。

Emacs 值得学习的是协议的宽度，不是它的热路径执行模型。官方手册明确警告
CAPF 可能被频繁调用，并建议用函数型 collection 延迟昂贵候选生成；基础
API 的 `try-completion` 又会比较 collection 中的候选。这些是一手资料可
支持的性能风险，而不是“Emacs 在所有场景都慢”的证明。
[Emacs buffer completion][emacs-buffer]、
[Emacs basic completion][emacs-basic]

## 3. Neovim

### 3.1 source 与候选抽象

Neovim 内建补全有两层 source 形态：

- `complete` 选项枚举当前 buffer、其他 window、dictionary、tags 等内建
  来源；自动补全按配置顺序收集，并给靠前来源更高时间优先级。
- `completefunc`、`omnifunc` 和 `thesaurusfunc` 是函数接缝。函数先返回
  起始列，再返回候选列表或带 `words`、`refresh` 的字典。

候选可以携带 `word`、`abbr`、`menu`、`info`、`kind`、去重标志、
`user_data`、`preselect` 和 `commit_chars`。这使“插入文本”“过滤文本”
“展示信息”和“接受行为”不必完全等同。
[Neovim insert completion][nvim-insert]

这套 API 易接入单个函数，却不是 blink.cmp 那样统一的多 source 生命周期
协议。内建来源、用户函数和 LSP adapter 的配置与取消方式并不相同。

### 3.2 异步、取消和去抖

通用 complete-function 的慢任务模型是协作式的：函数可以边搜索边调用
`complete_add()`，并周期调用 `complete_check()` 让用户输入中止搜索。
函数也可返回 `-2`，保持补全状态以便异步加入候选。
[Neovim insert completion][nvim-insert]

官方 LSP adapter 则有完整的请求生命周期：

- 每次触发前取消 `Context.pending_requests`，并对每个 LSP client 保存
  request ID；取消函数调用各 client 的 `cancel_request`。
- trigger character 使用 25 ms timer 合并连续触发。
- incomplete list 的重查延迟根据最近请求间隔与移动平均 RTT 自适应计算。
- `completionItem/resolve` 有独立 timer、RTT 平均与取消路径；响应落地前
  复查 buffer、Insert mode、popup 可见性和当前选中项。
- 离开 Insert mode、显式取消或新触发都会清理 timer 与 pending request。

这些行为见官方 `runtime/lua/vim/lsp/completion.lua` 中的 `Context`、
`request`、`CompletionResolver`、`trigger` 与 `on_insert_char_pre`。
[Neovim LSP completion source][nvim-lsp-source]

### 3.3 本地过滤、排序和缓存

Neovim popup 在继续输入时会缩小已有匹配集合；complete-function 可用
`refresh = "always"` 要求前导文本变化时重查。内建自动补全还对 source
使用递减 timeout，并允许给 source 设置候选上限。
[Neovim insert completion][nvim-insert]

LSP adapter 的行为更具体：

- 普通前缀匹配使用 `filterText` 或 `label`；开启 `completeopt=fuzzy` 后用
  fuzzy score 过滤。
- 默认按 LSP `sortText`，没有时按 `label`；完整列表可叠加 fuzzy score，
  incomplete 列表不按 fuzzy score 重排。
- 多 server 结果合并后可由用户 `cmp` comparator 重排。
- 仅当服务端返回 `isIncomplete` 时，继续输入才用
  `TriggerForIncompleteCompletions` 重查；否则沿用 popup 中已有结果。
- 新响应替换同一 client 的旧项，同时保留尚未响应 client 的仍匹配项。

源码中没有一个可供所有 source 共用的通用缓存契约；LSP 的复用主要依赖
当前补全 session 与 LSP 的 `isIncomplete` 语义。
[Neovim LSP completion source][nvim-lsp-source]

### 3.4 LSP 语义与接受

`vim.lsp.completion.enable()` 把多个 client 接入原生 popup，并提供
`convert`、合并 comparator、commit character 开关。它处理
`itemDefaults`、`filterText`、`sortText`、snippet、insert/replace edit、
`preselect`、deprecated tag、commit characters 和 lazy resolve。
[Neovim LSP help][nvim-lsp]

接受后，adapter 可能清除 popup 已插入的临时 word，再应用 snippet、
`additionalTextEdits` 和 LSP command。如果 accept 时还需 resolve，它用
buffer `changedtick` 拒绝在 buffer 已变化后安装响应。这个校验很重要，
但源码没有把全部副作用描述为一个跨步骤原子事务。
[Neovim LSP completion source][nvim-lsp-source]

## 4. Emacs

### 4.1 CAPF 与 programmed completion 的扩展性

`completion-at-point-functions` 是 abnormal hook。每个 CAPF 返回
`(start end collection . props)`；第一个负责的 CAPF 获胜，
`:exclusive no` 可在无匹配时继续下一个。
[Emacs buffer completion][emacs-buffer]

`collection` 可以是 list、obarray、hash table 或函数。函数型 programmed
completion 会收到 query、predicate 与操作标志，能够分别实现
`try-completion`、`all-completions`、精确测试、边界和 metadata。
表包装器还能做 case-fold、predicate、merge、in-turn、subvert 和 quoting。
[Emacs programmed completion][emacs-programmed]、
[Emacs basic completion][emacs-basic]

匹配策略由 `completion-styles` 与 `completion-styles-alist` 注入；类别可
覆盖 styles、cycle 和 sort。metadata/extra properties 还能提供 category、
annotation、affixation、group、display/cycle sort 和 exit function。
因此 source、匹配策略、展示附加信息和接受后行为彼此可组合。
[Emacs completion variables][emacs-variables]

这正是 Emacs 最值得 Vell 继承的部分：协议不是只返回字符串列表，而是允许
惰性 collection、按操作分派和丰富 metadata。不过 CAPF 默认是“首个负责者”
而不是并行多 source merge；多源合并要由 completion table 或上层包完成。

### 4.2 一手资料能确认的性能热点

官方手册直接给出三条重要约束：

1. CAPF 应快速返回，因为它可能从 `post-command-hook` 被频繁调用。
2. 昂贵候选不应在 CAPF 返回时立刻构造，应返回函数型 collection 延迟生成。
3. source 不应预过滤；过滤由调用方按 completion style 完成。

[Emacs buffer completion][emacs-buffer]

基础 `try-completion` 的文档说明，它会把输入与 completion table 中允许的
候选比较；`all-completions` 则需要返回全部匹配项。复杂 style 若建立在已
物化的大候选集上，就会把扫描、分配和排序留在交互热路径。
[Emacs basic completion][emacs-basic]

Eglot 当前源码展示了具体成本：

- 首次需要候选时同步调用 `jsonrpc-request` 包装器；请求允许
  `cancel-on-input` 并向 server 发送 `$/cancelRequest`。
- response 被完整 map 为带 text property 的 proxy 字符串列表。
- Eglot 的 flex style 逐字符扫描每个 proxy；`all-completions` 还复制
  proxy 字符串后再次过滤。
- 展示排序用 `cl-sort` 按 `sortText` 排完整列表。
- resolve 使用 hash cache；只有 `isIncomplete` 明确为 false 的列表才进入
  completion-session cache。

这些实现位于 `eglot-completion-at-point` 与 `eglot--request`。
[Eglot source][eglot-source]

因此可支持的结论是：Emacs 的扩展协议很宽，但当前 CAPF/Eglot 路径把
provider 调用、候选物化、Lisp 级过滤与排序组合在交互线程上；取消和
session cache 能减轻延迟，却没有把它变成原生流式、generation-safe 的
多 provider 调度器。这是 Vell 应避免的结构性风险。

### 4.3 Eglot 的 LSP 映射和接受

Eglot 把 LSP 完成映射为普通 CAPF，而不是另造专用 UI。completion table
提供 Eglot category、display sort、annotation，并为 Company 暴露 kind、
deprecated、doc signature 和 doc buffer 等属性。
[Eglot source][eglot-source]

若服务端返回完整列表，Eglot 在同一 completion session 内缓存候选、
resolve hash、原始范围、原始位置和原始文本；`isIncomplete` 列表不会这样
复用。resolve 只在文档或接受确实需要时请求，并支持输入取消。
[Eglot source][eglot-source]

接受通过 CAPF `:exit-function` 完成。Eglot 必要时先恢复获得 LSP edit 时的
原始文本，再应用 `TextEdit` 或 `InsertReplaceEdit`；snippet 使用可选的
Yasnippet adapter，随后应用 `additionalTextEdits`，最后发送 didChange。
这说明一个通用补全框架必须保留 source 原始 item，而不能在过滤阶段把它
降格为纯字符串。
[Eglot source][eglot-source]

## 5. blink.cmp v1

### 5.1 管线与 source 契约

官方架构文档把系统拆为四段：`trigger -> sources -> fuzzy -> render`。
trigger 构造 query、cursor 与 Tree-sitter 上下文；sources 统一完成候选、
trigger characters、resolve 与 cancellation；fuzzy 通过 Rust/Lua FFI 同时
过滤和排序；windows 负责 menu、documentation 与 signature 渲染。
[blink architecture][blink-architecture]

稳定版 source contract 直接采用 LSP `CompletionItem` 形状，并定义：

- `new`、可选 `enabled` 和 `get_trigger_characters`；
- `get_completions(ctx, callback)`，callback 首次发布、后续追加，支持流式；
- response 的 forward/backward incomplete 标志，分别决定增加或删除字符
  后是否重查；
- 返回 cancellation function；
- 可选 `resolve(item, callback)` 延迟补文档与 additional edits；
- 可选 `execute` 自定义接受，并可复用默认实现。

source 文档明确要求 source 不按 keyword 预过滤，过滤由 blink 统一完成；
若缓存并复用 item，因框架会修改 item，source 必须先 deep-copy。
[blink source boilerplate][blink-source]

### 5.2 调度、fallback 与故障隔离

每个 provider 都能动态配置 `enabled`、`async`、`timeout_ms`、
`transform_items`、`max_items`、`min_keyword_length`、fallback、
`score_offset` 和函数 override。`async` provider 不阻塞先到结果展示；同步
provider 超时后也被视为异步。fallback 只在上游无结果时启用，并能表达
多个上游共同控制一个 fallback。
[blink sources][blink-sources]

main snapshot 的 source queue 还展示了比统一 debounce 更精细的背压：

- 同一 context 同时最多运行一个请求；忙时 queued slot 只保留最新 context；
- 新 context 可先发布缓存列表，再排队真正请求；
- 异步 response 发布前比较 context ID，过期结果不会进入 completion UI；
- selection 的 lazy resolve 单独使用 50 ms debounce，并取消旧 resolve。

对应实现位于 `lua/blink/cmp/sources/lib/queue.lua`、
`sources/lib/init.lua`、`completion/init.lua` 与
`completion/prefetch.lua`。
[blink pinned source snapshot][blink-main]

这里最值得借鉴的不是 Lua 配置语法，而是调度语义进入统一 provider 层：
插件无需各自重造 timeout、fallback、streaming 与 cancellation。

### 5.3 过滤、排序与缓存

blink 的 Rust fuzzy 阶段统一做过滤和排序。官方文档描述的排序信号包括
fuzzy score、frecency、proximity、source/item `score_offset`，以及可配置的
`sortText` fallback；匹配结果还返回字符索引用于 UI 高亮。
[blink architecture][blink-architecture]、
[blink reference][blink-reference]

官方 README 声明每次按键更新在单核异步执行，并给出 0.5--4 ms 的项目方
自报范围；架构页对“约 6 倍 FZF”同时标了尚待补 benchmark。roadmap 不应把
这两项当作 Vell 验收基准，只应把“Rust 热路径、Lua 冷路径”当作设计证据。
[blink README][blink-readme]、
[blink architecture][blink-architecture]

缓存分散在不同生命周期：

- source manager 依据 forward/backward incomplete 决定复用还是重查；
- source 可自行缓存 item，但必须按契约复制；
- fuzzy 层按 provider 缓存 FFI haystack；只有 Lua item table 身份变化时才
  重建 Rust 侧 provider item table；
- LSP source 按 client 分别缓存，只有该 client 的 `isIncomplete` 为 true
  才随输入重查，多 client session 不必全部重发；
- snippets 有 items cache 选项；
- buffer source 按 `changedtick` 缓存每个 buffer 的词表，并设置同步、异步、
  跳过和总字符数阈值；
- frecency 使用持久数据库，不等同于候选 response cache。

[blink source boilerplate][blink-source]、
[blink reference][blink-reference]、
[blink changelog][blink-changelog]

上述实现细节位于 pinned snapshot 的 `fuzzy/init.lua`、
`fuzzy/rust/lib.rs`、`sources/lsp/cache.lua` 和
`sources/buffer/init.lua`。buffer source 自己注明：buffer 改动后当前仍会
重扫整个 buffer，而不是仅更新变化行。因此它是缓存加分级阈值，不是完整的
增量索引。
[blink pinned source snapshot][blink-main]

因此 Vell 不应只有一个不分语义的全局 LRU。至少要区分 provider response、
当前 session 的 filtered view、lazy resolve、buffer word index 和持久接受
历史，并为每层规定 invalidation key。

### 5.4 UI 与接受

blink 的 menu 是可组合 grid：column 由 component 构成，component 分别
提供 text 与 highlight；highlight 只对屏幕可见 item 调用。documentation、
ghost text、signature、selection 与 auto-insert 都有独立策略。
[blink completion UI][blink-completion]

接受配置可创建独立 undo point、写 dot-repeat、给 resolve 设置 timeout，
并按 kind 或异步 semantic token 决定 auto-brackets。auto-insert 只是预览，
取消会撤销预览；source `execute` 可以在主 edit 后扩展行为。
[blink reference][blink-reference]、
[blink source boilerplate][blink-source]

accept 实现会先隐藏列表并立即 resolve，默认最多等待 100 ms；随后计算主
edit/insert-replace range，再走 source `execute` 或默认实现。preview 保存
逆 edit，选择变化、取消或正式接受前都可撤回。
[blink pinned source snapshot][blink-main]

blink 的官方 LSP tracker 仍列出部分 commit-character、preselect、
insert/replace 与 additional-edit 问题。因此它证明了优秀的管线设计，但不应
被当成 LSP completion 语义完整性的唯一基线。
[blink LSP tracker][blink-lsp-tracker]

可扩展 `execute` 很有用，但 Vell 的脚本不能直接取得可变 Buffer。更安全的
等价物是让 provider 返回 typed accept plan，由 app 在同一
`ExecutionFrame` 校验 revision、准备 edits、一次提交或整体回滚。

## 6. LSP completion 的共同语义底线

LSP 3.18 规范明确提出：

- client 通常负责过滤和排序，server 用 `filterText`、`sortText` 施加提示；
- 为了速度，用户继续输入时 client 应能过滤已收到列表；只有
  `CompletionList.isIncomplete` 才要求服务端重算；
- expensive documentation/detail 可经 `completionItem/resolve` 延迟获取；
- insert text/label 与 text edit 的过滤边界语义不同；
- client/server 通过 snippet、commit characters、insert/replace edit、
  lazy-resolve properties 和 item defaults 能力协商。

[LSP official site][lsp-site]

三套实现都证明：LSP 是一种 provider，不应成为核心 completion model。
核心应容纳更轻的 buffer/path/snippet source，也要完整保留 LSP item 的
filter/sort/resolve/accept 语义。

## 7. 面向 Vell roadmap 的设计推论

以下是从上述事实推导出的建议，不是对上游实现的陈述。

### 7.1 必须稳定的核心 seam

核心至少需要这些稳定类型：

- `CompletionContext`：content/view、revision、cursor/selection、query range、
  language/mode、trigger kind、typed snapshot handle；
- `CompletionProvider`：capabilities、trigger、request、cancel、resolve；
- `CompletionBatch`：session/generation、items、replace-or-append、
  incomplete-forward/backward；
- `CompletionItem`：稳定 item ID、provider ID、label/filter/sort/insert、kind、
  score hints、opaque owned provider data；
- `CompletionAcceptPlan`：main edit、additional edits、snippet、command 和
  provider operations 的有序 typed plan；
- `CompletionPresentation`：只读 rows、selection、ghost text、docs 与状态。

### 7.2 性能预算应约束算法与调用次数

不要把一个“总耗时”数字写成唯一验收项。分别量化：

- 输入到首批本地候选可见的 p50/p95；
- 每按键 Rust filter/rank 的 p50/p95 与 candidate count 曲线；
- 主线程每帧允许执行的脚本 callback 数与时间；
- 过期 response 丢弃率、取消传播延迟、resolve 命中率；
- buffer index 增量更新成本与内存上限；
- accept transaction 的提交与回滚延迟。

候选上限只能保护 UI，不能替代 provider 侧索引、增量过滤和 top-k；否则
大 collection 仍会在截断前付出扫描与排序成本。

### 7.3 扩展性边界

应向 TypeScript 插件开放：

- provider 注册、动态 enable、trigger characters；
- owned snapshot 请求和 streaming batch sink；
- 有界 bulk transform、score offset 和声明式 sort key；
- lazy resolve 与 typed accept operations；
- menu component 数据与 face，不开放直接 TUI/window 控制。

不应开放：可变 Buffer、V8 handle 持久化、逐候选脚本 comparator、绕过
generation/revision 的结果安装、直接提交 additional edits，或在 render
pull 路径调用 provider/V8。

## 8. 一手资料索引

[nvim-insert]: https://neovim.io/doc/user/insert.html
[nvim-lsp]: https://neovim.io/doc/user/lsp.html
[nvim-lsp-source]:
https://github.com/neovim/neovim/blob/master/runtime/lua/vim/lsp/completion.lua

[emacs-buffer]:
https://www.gnu.org/s/emacs/manual/html_node/elisp/Completion-in-Buffers.html

[emacs-basic]:
https://www.gnu.org/s/emacs/manual/html_node/elisp/Basic-Completion.html

[emacs-programmed]:
https://www.gnu.org/s/emacs/manual/html_node/elisp/Programmed-Completion.html

[emacs-variables]:
https://www.gnu.org/s/emacs/manual/html_node/elisp/Completion-Variables.html

[eglot-source]:
https://github.com/emacs-mirror/emacs/blob/master/lisp/progmodes/eglot.el

[blink-readme]: https://github.com/Saghen/blink.cmp
[blink-architecture]: https://cmp.saghen.dev/development/architecture
[blink-sources]: https://cmp.saghen.dev/configuration/sources
[blink-source]: https://cmp.saghen.dev/development/source-boilerplate
[blink-reference]: https://cmp.saghen.dev/configuration/reference
[blink-completion]: https://cmp.saghen.dev/configuration/completion
[blink-lsp-tracker]: https://cmp.saghen.dev/development/lsp-tracker
[blink-changelog]:
https://github.com/Saghen/blink.cmp/blob/main/CHANGELOG.md

[blink-main]: https://github.com/Saghen/blink.cmp/tree/0e63407

[lsp-site]: https://microsoft.github.io/language-server-protocol/
