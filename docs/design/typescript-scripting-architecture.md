# TypeScript 脚本架构

**状态：** 当前脚本子系统设计；总架构见
[`editor-kernel-architecture.md`](editor-kernel-architecture.md)
**日期：** 2026-07-21

## 0. 实现补充

当前实现已经完成以下边界；这些结论取代本文后续仍保留的过渡期描述：

- 默认功能也通过 `runtime/plugins/*/plugin.json` 加载，不在 Rust registry
  中注册具体 Mode；
- Rust 生产代码不识别 Vim、Tree-sitter 或具体语言名称；
- Vim 输入行为和语法高亮均由内嵌 TypeScript 插件定义；
- Script Mode 可定义 Face、decoration、view policy 和原始输入 action；
- Content 分析使用持久的独立 V8 worker isolate；
- Tree-sitter worker 按 Content 保留旧语法树，并以最小文本差异执行增量解析；
- worker 只接收结构化消息和插件目录内的只读内嵌资源；
- worker Promise 由显式 microtask checkpoint 驱动，并受取消和超时约束；
- 后台任务复用通用 `(ModeId, ContentId, slot)` 调度和最新版本合并；
- 新版本后台任务会取消仍在运行的旧版本，不执行过期的完整分析；
- v2 Buffer adapter 通过命名 `analysis` 声明 worker、输入快照和结果应用，
  task slot 与 revision 由宿主管理；
- `snapshot: "text"` 在 worker 线程构造全文消息，UI 线程只捕获低成本
  TextSnapshot；
- 渲染只读取 Rust 侧缓存的 presentation snapshot，不同步进入 V8；
- decoration 缓存按可见行裁剪后再交给 TUI；
- 文本变化期间旧 decoration 会经过位置映射，直到新 revision 安装。
- 输入和显式 action 通过 callback-scoped native functions 调用 Rust 原语；
- 原语直接暂存 `OperationRequest`，不使用字符串命令或 `ScriptEffect` DTO；
- callback 结束后旧 context 的原语函数失效，不能持有宿主引用；
- callback 或返回值验证失败时丢弃本次暂存操作，不进入 app executor；
- `ModeName`、action 名和 Face 名仍是字符串，因为它们属于插件扩展命名空间。
- v2 定义通过 `on.buffer` 和 `on.statusBar` 生成 canonical
  Mode-Content adapter，不支持的 adapter 不会挂载到对应 Content；
- v2 以 `state`、`viewState`、`commands` 和 `keys` 为基础组织，
  `void` 表示处理，`ctx.pass()` 表示继续 Mode chain；
- Buffer 和 StatusBar 拥有独立的 TypeScript context 类型与运行时原语
  集，StatusBar 不安装 `edit`、`cursor` 或文本原语；
- v1 `content/view/actions/keys` schema 暂时作为兼容入口，但与 v2
  共用 `ScriptMode`、Mode state、typed operation 和 execution frame。
- 内建 Vim 与 Tree-sitter 已迁移到 v2；有效 v1 用户定义每个宿主只输出一次
  deprecation 诊断，删除 parser 留给单独版本决策。
- v2 的静态 context 收窄由 `runtime/type-tests/tsconfig.json` 固化；使用
  `tsc.cmd --noEmit -p runtime/type-tests/tsconfig.json` 验证负向类型用例。
- `changed` 当前只属于 Buffer adapter；StatusBar 是派生 Content，没有独立的
  `ContentChange` 通知源，因此 schema 和类型定义均不暴露该 hook。

主 ScriptHost 仍在 UI 线程同步执行输入和 action callback。只有显式声明的
后台 worker 用于解析等 CPU 密集工作，因此没有引入通用 Web、Node 或 Deno
运行时。

## 1. 定位

TypeScript 是动态定义 Mode 的语言前端，不建立第二套编辑器内核。

第一版参考 RSVim 的 `rusty_v8` runtime：长期存活的 isolate/context、
ES module loader、TypeScript 转译、显式 microtask checkpoint 和统一异常处理。
编辑器绑定不照搬 RSVim 直接持有 buffer、UI tree 等共享对象的方式，而是映射到
现有 Mode、Command、operation 和事务边界。

```text
config.ts / local modules
-> ScriptHost (rusty_v8)
-> ScriptMode adapter
-> ModeResult / OperationRequest
-> app command executor
-> ContentStore / View / history transaction
```

## 2. 目标

第一版必须支持：

- 加载并转译用户 `config.ts` 和本地 TypeScript/JavaScript ES modules；
- 动态注册多个可共存的 Script Mode；
- 为每个 Script Mode 定义 content state、view state 和两类 action；
- 按既有 ModeChain 顺序处理输入并返回 Continue/Stop；
- 使用脚本 keymap 和 key sequence 调用 action；
- 读取 Content 快照、revision、document status 和 View selections；
- 通过 typed operation 动态修改 selection 和 Content 文本；
- 使用既有事务、undo/redo、selection 变换和 ContentChange 通知；
- 定义 Face、decoration 和 View policy；
- 通过 ModeCommand 调用其他 native 或 script Mode；
- 提供 `.d.ts`、TypeScript source map 和可定位的错误信息；
- 限制单次 callback 的执行时间和 V8 heap。

## 3. 非目标

第一版不包含：

- npm、远程 URL import 或 Node/Deno 兼容层；
- 网络、子进程和任意异步文件 API；
- 通用 Web/WinterTC runtime；
- project-local 脚本自动信任和执行；
- 插件包管理器、依赖解析器或版本求解；
- inspector、debug adapter 或 REPL；
- 热重载时迁移旧 Mode state；
- 对整个 V8 heap 或模块闭包状态进行事务 checkpoint；
- callback 中直接借出可变 Buffer、View、ContentStore 或 App。

## 4. 运行时选择

第一版直接使用 `rusty_v8`，不依赖 `deno_core`。

原因是 Script Mode callback 必须同步完成，且编辑器已经拥有 Tokio 主循环、
Command executor、后台 Mode job 和事务框架。第一版只需要本地静态 module、
callback registry 和少量宿主绑定，不需要第二套通用异步资源系统。

TypeScript 由与所选 V8 版本兼容的成熟 SWC 版本转译。依赖版本必须作为一个
经过构建验证的集合固定，不能分别盲目升级。

如果未来需要 dynamic import、top-level await、timer、异步文件、网络或进程，
应先重新评估 `deno_core`，而不是继续扩张自制 event loop。

## 5. 所有权与线程

`ScriptHost` 由 app runtime 在主线程持有。所有 Script Mode definition 共享同一
host，但 content/view state 仍由现有 Mode stores 按各自作用域持有。

```text
App runtime
├── Kernel
│   ├── ModeRegistry
│   │   ├── native Mode
│   │   └── ScriptMode -> shared ScriptHost
│   └── ModeContentStore
├── ClientSession
│   └── ModeViewStore
└── ScriptHost
    ├── Isolate
    ├── Context
    ├── ModuleMap
    └── CallbackRegistry
```

同步 callback 期间 app 本来就必须等待 ModeResult，独立 VM 线程不会改善输入
响应，反而要求额外的序列化和请求通道。因此第一版不建立 VM worker thread。

超时 watchdog 在 Tokio worker 上持有 `IsolateHandle`。callback 到期时终止 V8
执行；callback 正常返回时取消 watchdog。

## 6. 初始化顺序

```text
初始化 V8 platform
-> 创建 isolate/context
-> 安装内置 editor API
-> 定位并转译用户 config.ts
-> 递归加载本地静态 ES modules
-> 执行 config，收集 Mode 定义和 attachment policy
-> 注册 native + script Mode
-> 创建 Content/View 和初始 ModeChain
-> 进入 app event loop
```

配置入口只从用户配置目录或显式 CLI/环境覆盖路径加载。不得自动执行工作目录
或所打开项目中的脚本。

配置顶层运行时尚未创建编辑 Content，因此只负责注册定义、Face 和 attachment
规则，不直接修改 Content。文本修改由后续 input/action callback 产生。

## 7. Script Mode 定义

脚本定义与 native Mode 共享一个后端契约。v2 以 Content adapter
为行为边界：

```text
name
adapter state factory
adapter view-state factory
mode-local commands
adapter input handler
adapter keymap
content change callbacks
faces
decorations
view policy
attachment policy
```

TypeScript API 保持 command-first：

```ts
editor.modes.define({
  name: "pairs",
  on: {
    buffer: {
      state: () => ({ enabled: true }),
      viewState: () => ({ inserted: 0 }),
      commands: {
        quote(ctx) {
          if (!ctx.state.enabled) return ctx.pass();
          ctx.viewState.inserted++;
          ctx.edit.insert('\"\"');
          ctx.cursor.moveLeft();
        },
      },
      keys: {
        '\"': "quote",
      },
    },
  },
});
```

定义中的 callback 保存在 V8 CallbackRegistry。`ScriptMode` 只保存稳定的
callback identity 和静态 keymap/presentation metadata，不保存宿主可变引用。

## 8. 状态语义

正式 Script Mode 状态只有：

```text
state:     每 (ModeId, ContentId) 一份
viewState: 每 (ModeId, ViewId) 一份
```

它们必须是可结构化复制的数据：

```ts
type ScriptData =
  | null
  | boolean
  | number
  | string
  | ScriptData[]
  | { [key: string]: ScriptData };
```

函数、Promise、V8 handle、循环引用和宿主对象不能存入 Mode state。callback 前
checkpoint 这两份状态；callback 和后续 operation 成功后安装新值，执行帧失败时由
现有 Mode state snapshot 恢复。

模块级变量和闭包状态遵循普通 JavaScript 语义，长期存在但不属于编辑器事务。
脚本抛错前已经发生的纯 JavaScript 全局状态变化不回滚。这与文件句柄、缓存等
runtime 外部资源一致，不尝试为整个 V8 heap 制造伪事务。

## 9. Callback 调用边界

callback 获得一次调用期间有效的 context 和可变的 Script Mode state 草稿。
context 包含 owned/共享快照和 callback-scoped native functions。脚本可以保存
普通快照值，但旧 context 中的原语函数在 callback 结束后必须拒绝调用。

```text
Rust Mode state checkpoint
-> 建立 V8 callback scope
-> 暴露快照、state draft 和 native functions
-> 调用 JS function
-> 提取 flow、新 state、presentation snapshot 和暂存操作
-> 验证全部返回值
-> 退出 V8 scope
-> app 按顺序执行 OperationRequest
```

V8 callback 内不得重新进入 app command executor。
`context.commands.invoke("mode.command")` 只暂存类型化
`ModeCommand`，等 callback scope 退出后再由 app 深度优先执行。

`onContentChanged` 等被动 callback 只能更新所属 Mode state，不能返回编辑或
其他宿主 operation。需要修改文本的行为必须由 input 或显式 action 触发，避免隐式
递归编辑循环。

## 10. Callback 结果和原语

v2 command 正常返回 `void` 表示已处理；只有 `return ctx.pass()`
会继续后续 Mode。boolean 和旧 `ModeActionResult.continue` 不是 v2 流向
语义。编辑操作不放在返回值中，而是调用 Buffer context 的
`cursor`、`edit`、`history`、`viewport`、`commands` 和 `app`
原语。StatusBar context 只安装其合法能力。

v1 callback 暂时仍使用 `handled()`、`forward()` 和原有的结果对象，
兼容语义由脚本 parser 边界吸收，不进入 Rust Mode contract。

每个 native function 立即校验参数，并把 `OperationRequest` 追加到本次 callback
的 Rust 暂存区。adapter 必须在执行任何宿主操作前完整验证返回值和 state。
错误参数、非法坐标、冲突编辑和超出预算都使 callback 失败，并丢弃全部暂存
操作，不允许部分执行。

## 11. Content 读取

脚本读取的是 callback 开始时的 Content 快照：

- revision；
- document status；
-行数和文本长度；
-按范围读取文本；
-位置与 offset 转换；
-当前 View selections（只有 view/input callback 可用）。

不在每次 callback 时把全文转成一个 JavaScript string。快照由 Rust 持有，脚本
通过窄范围查询读取所需内容；只有请求的文本跨 V8 边界。

需要分析全文的命名 analysis 声明 `snapshot: "text"`。宿主捕获
TextSnapshot，并在 worker 线程上把正文加入消息的 `text` 字段，避免阻塞输入
线程。analysis 名称映射到内部 slot；宿主为请求分配单调 generation，并捕获
Content revision 和输入 Mode state。TypeScript callback 不接触 slot、
generation 或取消队列。

analysis `input` 是纯函数，只捕获 Content 元数据和深只读 Mode state；其返回
message 同时作为输入签名，意外状态修改不会发布。宿主在一次 poll 中先计算并
发布所有 slot 的签名，再提交全部取消或替换请求，关闭跨 slot stale 窗口。只有
revision、input epoch 和 message 仍匹配的当前 generation 可以进入 `apply`。

`apply` 复用 Mode draft，验证完成后原子发布 state 和独立 decoration layer。
当前 slot 的 post-apply input 被视为该结果的一部分，不会自触发循环；其他 slot
仅在自己的 input message 变化时重跑。完成结果按内部 slot 路由，不能覆盖其他
analysis 的缓存。

## 12. 脚本坐标

TypeScript API 使用独立的 UTF-16 `line/character` 坐标，符合 JavaScript string、
LSP 和常见 TypeScript 编辑器 API 的习惯。

```ts
interface Position {
  line: number;
  character: number;
}

interface Range {
  start: Position;
  end: Position;
}
```

adapter 将它转换成内部字符 offset。位置不能落在 surrogate pair 中间。脚本坐标
类型不得复用内部字符列 `TextPoint` 或 Tree-sitter UTF-8 byte point。

## 13. View-relative 编辑

input/view action 可以调用基于当前 selections 的原语，例如：

```ts
ctx.edit.insert("hello")
ctx.edit.deleteBackward(1)
ctx.cursor.moveLeft(1)
```

这些原语在 Rust 中直接构造现有 `EditCommand`，并在实际执行时依据当时的 View
selections 解析。多个有序操作可以观察前一个操作已成功产生的状态。

## 14. Content edit batch

view action 可以对所属 Content 暂存绝对范围编辑：

```ts
ctx.text.applyEdits([
  {
    range: {
      start: { line: 1, character: 4 },
      end: { line: 1, character: 7 },
    },
    text: "replacement",
  },
])
```

语义为：

- 一个 batch 的全部范围基于同一份 callback 输入快照；
- batch 隐式绑定本次 callback 开始时捕获的 Content snapshot；
-范围经过 UTF-16 到内部字符 offset 的严格转换；
-多个范围不得重叠或产生歧义；
- adapter 一次性构造 `TextChangeSet::from_edits`；
-一个 batch 映射为一个 `ContentAction::Text`；
-全文替换只是覆盖完整文档范围，不增加专用 mutation API；
-第一版只能修改当前 Script Mode 所附着的 Content。

ContentAction 继续由 app 执行，因此自动获得：

-所有关联 View 的 selection 变换；
- Content revision 更新；
- ContentChange 通知；
- execution frame 恢复；
- history transaction、undo 和 redo。

## 15. 呈现

渲染查询不能每帧同步进入 V8。脚本 callback 更新 owned presentation snapshot，
`ScriptMode::decorations` 和 `view_policy` 只读取 Rust 侧缓存。

第一版允许脚本定义：

- named Face 的默认值；
-按文本范围排序的 decoration spans；
- cursor style/domain；
- selection shape/face。

可见范围查询继续由 app 裁剪缓存，TUI 不知道 decoration 来自脚本还是 native
Mode。

## 16. 模块系统

第一版支持：

- `.ts` 和 `.js`；
-相对或用户配置目录内的绝对本地 import；
-静态 ES module graph；
- `import.meta.filename`、`dirname` 和 `url`；
- module compile/evaluation cache；
- TypeScript source map。

第一版拒绝：

- `http:`、`https:` 和裸 npm specifier；
-配置目录之外的隐式搜索；
- dynamic import；
- CommonJS `require`。

内置 editor API 作为预注册 module 或只读 global namespace 暴露。实现时选择
其中最小的一种，不同时维护两套公共入口。

## 17. 错误与资源限制

错误至少包含：

-配置或 module 路径；
- TypeScript 原始行列；
- callback/mode/action 名；
-异常 message 和 stack；
-超时、heap limit、转换或 operation 验证失败类别。

主动 callback 错误映射为 `ModeError`，交给当前 `ExecutionFrame` 丢弃 Mode
draft，并恢复本次输入的 Content、View、input 和 transaction checkpoint。
被动 callback 错误使用 attachment fault isolation，不阻止基础文本编辑。

V8 exception 不自动销毁整个 runtime。timeout 终止传播完成后，runtime 只有在
V8 允许恢复执行时才继续使用，否则禁用脚本层并保留 native 编辑能力。

## 18. 分层

不新增通用 `script` trait 或多语言 VM abstraction。第一版只有一个 runtime，
直接实现最小边界：

```text
app::script
├── runtime        rusty_v8 platform/isolate/context
├── module         loader、module map、TypeScript transpile
├── value          ScriptData 和窄 V8 转换
├── primitives     callback-scoped native functions 和操作暂存
└── mode           ScriptMode adapter 和 presentation 映射
```

依赖保持：

```text
app::script -> app::mode + core + protocol
core        -> protocol/std
tui         -> frontend + terminal + protocol
```

`core`、Content、View、frontend 和 TUI 不依赖 V8，也不识别 Script Mode。

## 19. 测试

第一版至少覆盖：

- TypeScript 转译并执行配置；
-本地静态 module import 和缓存；
-脚本注册 Mode 并附加到初始 View；
-输入 Continue/Stop 与后续 native Mode 顺序；
- content/view state 的作用域和 checkpoint；
- selection-based insert；
- Unicode UTF-16 range 转换；
-多范围 Content edit、selection 变换和 undo；
- stale revision、重叠 range 和非法返回值整体失败；
-跨 ModeCommand；
-脚本错误恢复编辑器状态；
-超时不会永久阻塞 native 编辑；
-渲染只读取缓存，不在 pull query 中调用 V8。

## 20. 实现顺序

1. 固定并验证 V8/SWC 依赖集合和 MSRV；
2. 实现 ScriptHost、内置 API 安装和 TypeScript 单文件执行；
3. 加入本地静态 module graph 和 config discovery；
4. 实现 Script Mode 注册、state 和 input/action callback；
5. 映射 view edit、Content edit batch 和 ModeCommand；
6. 加入 presentation snapshot、错误映射和资源限制；
7. 提供 `.d.ts` 和用户配置示例。

每一步都必须保持脚本 Vim、Tree-sitter、事务、保存、viewport 和 TUI 行为，
并通过项目要求的测试、clippy、fmt 和空白检查。
