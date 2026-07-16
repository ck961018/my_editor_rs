# Editor Evolution Roadmap

**状态：** 长期演进方向
**更新日期：** 2026-07-16

## 1. 路线图原则

本文件只描述演进顺序、触发条件和架构目标，不是可直接执行的任务清单。每个阶段在进入实施
前仍需单独编写 design spec；跨层或高风险改动再配套 implementation plan。

演进遵循以下原则：

- 先稳定机制和数据边界，再扩展功能数量；
- App 只协调通用状态，不感知 Vim、Emacs 或用户 Mode 的私有语法；
- 脚本是可直接调用 host API 的控制面，Rust 是机制和核心数据的数据面；
- 不采用 Wasm 沙箱作为目标脚本模型；
- builtin Mode 最终应能使用与用户脚本相同的注册和输入接缝；
- Content 保持静态闭合，直到脚本确实需要注册新 Content 类型；
- 远程、多客户端、增量布局等能力按真实使用场景或性能数据触发，不提前搭空框架。

## 2. 已完成的架构基线

以下能力是后续路线的既有前提，不再作为 roadmap 项目重复推进：

- `Kernel / ClientSession / View / Content` 所有权分离；
- `SpaceId / ViewId / ContentId` 身份分离；
- View 独立持有 `ModeInstance`、`ContentViewState` 和 revision；
- 静态 `Content` + 唯一 `ContentStore` + 语义 `ContentCommand`；
- Scene 模型与协议快照分离，布局和 viewport 归 TUI；
- owned pull query、View presentation 和远程 request/response 语义数据；
- revision-aware 原子保存与在途保存合并；
- 泛型 keymap trie、多活动层虚拟匹配、Leader 定义期展开；
- 通用 `Ready/Awaiting` 输入 context、timeout、replay 与取消语义；
- 用 `gg`、`f/F`、count 和最小 `dd` 验证 Vim 私有 Awaiting。

## 3. 阶段一：输入配置与可观察性

**目标：** 让现有输入机制成为用户可配置、可解释的稳定能力，而不是只有 builtin keymap 能
使用的内部结构。

优先内容：

- 定义持久化 keymap 配置格式和加载/校验错误；
- 支持用户设置 Leader、全局默认 timeout，以及 prefix 的 `Duration/Never` 覆盖；
- 配置变更时重建相关 keymap，不在运行时 trie 中保留 Leader alias；
- 暴露当前 prefix 的多层 continuation 并实现 which-key；
- 提供按键冲突、shadowing、无效 action 和不可达绑定的诊断；
- 保持一个 binding 对应一个结构化 RHS，组合逻辑进入 action 实现而不是 matcher；
- 保持 mode 优先于 global、长候选等待和 action-before-replay 的既有语义。

本阶段不顺带实现递归 remap、宏语言或完整 Mode stack。只有真实配置需要按 Content、项目或
临时 scope 叠加时，才扩展活动层来源和失效策略。

## 4. 阶段二：编辑事务与 Motion/Range 语义

**目标：** 在继续扩展 Vim operator 之前建立可组合、可撤销的编辑语义，避免把 Vim grammar
泄漏到 Buffer 或不断增加一次性 `EditCommand`。

优先内容：

- 定义编辑 transaction/change set 及其 selection 变换；
- 实现 undo/redo，并明确保存 revision 与 undo revision 的关系；
- 设计通用 Motion 结果、Range 和 shape（charwise、linewise，blockwise 按需求后置）；
- 让 operator 组合 Motion/Range 后产生通用编辑事务；
- 定义多 selection 下的重叠编辑、顺序、光标归并和失败原子性；
- 保持 Buffer 只执行编辑语义，不知道 `d`、`c`、count 或寄存器前缀。

只有第二个消费者证明抽象有用时，才把 Motion/Range 提升为更广的公共协议；初期可以留在
core 编辑领域内。

## 5. 阶段三：完整化 Vim Mode

**目标：** 在不改变上游通用输入接口的前提下，让 Vim Mode 从验证切片演进为可日常使用的
模态编辑实现。

建议顺序：

1. 完成常用 motion、`0/$/^`、word、paragraph、`t/T`、`;`/`,`；
2. 基于 Motion/Range 实现 `d/c/y` 与 operator 前后 count；
3. 增加 Visual/Visual Line，并与 selection 模型对齐；
4. 引入 register、put、named register 和 black-hole register；
5. 增加 repeat、macro、search 和 command-line 所需的独立状态；
6. 补齐 undo/redo、文本对象和可配置行为。

Count、operator、register、字符参数等始终留在 `VimModeState` 或其内部子状态机。若状态增长，
优先拆分 Vim 内部 grammar/context，不向 `InputStatus` 增加 Vim 专属变体。

## 6. 阶段四：稳定可扩展的 Mode Host API

**触发条件：** 开始接入第二个真正独立的 Mode，或准备选择脚本 runtime。

**目标：** 把当前原生 `Mode` trait 背后的能力整理为语言无关、可注册和可诊断的 host 契约。

需要明确：

- Mode 定义、实例、action、keymap 和 input context 的生命周期；
- View 切换、focus 变化、配置重载和脚本卸载时的 cancel/cleanup；
- host 可提供的查询、命令提交、状态消息和调度能力；
- action/context handle 的代际或失效规则，避免悬空引用；
- 错误隔离、调用栈诊断和脚本回调中的重入规则；
- 多个 Mode/context 真正需要组合时的层级、优先级与 `Pass` 传播。

不要为了假想的 major/minor mode 预先固定枚举或无用字段。组合模型应由至少两个真实 Mode 的
需求共同验证。

## 7. 阶段五：直接交互式脚本运行时

**目标：** 提供接近 Emacs 自由度、采用类似 rsvim 直接交互模型的内嵌脚本扩展，同时保持
Rust 核心边界清晰。

目标模型：

```text
脚本控制面
  注册 Mode / action / keymap / Awaiting context
  查询 View / Content / selection / editor state
  提交结构化 Command 或编辑 transaction
  订阅生命周期与变更事件

Rust 数据面
  文本存储、selection、transaction、undo、Scene、布局协议
  输入协调、ID/revision、任务与资源生命周期
```

runtime 选型时优先评估：直接 host object 交互、可调试性、增量/热重载、调用开销、生态和 Rust
嵌入质量；不以 Wasm 沙箱或跨语言稳定 ABI 为先决目标。

建议先让脚本注册一个完整 Mode 和全局 prefix context，再逐步迁移非应急 builtin Mode 到同一
公开 API。Rust 可以保留最小安全模式和核心机制，避免脚本配置损坏后编辑器无法启动。

权限、安全和资源限制以本地可信配置为默认场景设计；若以后引入第三方不可信插件，再另行
增加隔离层，不让该问题阻塞第一版直接交互式脚本。

## 8. 阶段六：文本显示模型

**触发条件：** 开始正确支持 tab、全角字符、组合字符、emoji 或软换行。

**目标：** 保持 `TextOffset -> TextPoint -> DisplayPoint` 分层，新增可验证的 DisplayMap，而不把
终端 cell 或 GUI pixel 写回 selection。

主要内容包括 grapheme 边界、Unicode width、tab stop、软换行、水平/垂直 viewport、命中测试
和光标形状。TUI 与未来 GUI 可以共享逻辑显示规则，但各自保留最终 cell/pixel 映射。

## 9. 阶段七：远程 Frontend 与多 ClientSession

**触发条件：** 有第一个真实的进程外 Frontend 或需要多个客户端同时查看同一 Content。

实施顺序：

1. 根据部署场景选择序列化和 transport；
2. 为 Scene 定义 snapshot/delta 和首次同步；
3. 接入已有 request/response、capability、revision 和 invalidation 语义；
4. 增加连接、断线恢复、背压和过期响应处理；
5. 将单 `ClientSession` 提升为 session registry，并明确共享 Content 的并发编辑规则；
6. 仅在并发真实出现后引入必要的 `Arc`、锁、actor 或消息所有权。

远程 Frontend 仍采用 pull 模型，不传输 Taffy 节点、Canvas 指令或后端对象借用。

## 10. 阶段八：按测量结果优化布局与渲染

**触发条件：** profiling 证明完整布局重建、重复 query 或整帧 paint 成为瓶颈。

可能方向：

- 让 Scene mutation 产生结构化 diff，并增量维护 Taffy 节点；
- 分离 layout revision、presentation revision 与 content revision；
- 缓存稳定的 ViewData、TextPoint 或可见行，但所有缓存都携带对应 revision；
- 生成 dirty region 或渲染 diff，减少终端输出；
- 对大型文档、多个 View 和频繁 mode/focus 切换建立基准。

当前 `TaffyEngine` 已避免在 scene revision 不变时重复建树。没有测量证据前，不用复杂的增量
同步替换这一简单缓存。

## 11. 明确不提前实施的事项

- 不因脚本 Mode 把 `Content` 立即改为动态 trait object 或通用插件容器；
- 不把 Vim count/operator/register 提升为 App 或协议层概念；
- 不为假想组合提前固定 major/minor Mode 枚举；
- 不在远程 Frontend 出现前选择 transport、serde 格式或认证协议；
- 不在多客户端写入冲突出现前设计 CRDT/OT；
- 不在 profiling 前实现 Scene diff、增量 Taffy tree 或全套渲染缓存；
- 不把未来脚本运行时设计成 Wasm 沙箱兼容层。
