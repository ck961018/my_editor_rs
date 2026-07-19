# TypeScript 脚本架构

**状态：** 已确认
**日期：** 2026-07-19

## 1. 定位

TypeScript 是动态定义 Mode 的语言前端，不建立第二套编辑器内核。

第一版参考 RSVim 的 `rusty_v8` runtime：长期存活的 isolate/context、
ES module loader、TypeScript 转译、显式 microtask checkpoint 和统一异常处理。
编辑器绑定不照搬 RSVim 直接持有 buffer、UI tree 等共享对象的方式，而是映射到
现有 Mode、Command、effect 和事务边界。

```text
config.ts / local modules
-> ScriptHost (rusty_v8)
-> ScriptMode adapter
-> ModeResult / ModeEffect
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
- 通过 typed effect 动态修改 selection 和 Content 文本；
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

脚本定义与 native Mode 共享一个后端契约：

```text
name
content state factory
view state factory
content actions
view actions
input handler
keymap
content change callbacks
faces
decorations
view policy
attachment policy
```

TypeScript API 的具体命名在实现时保持小而一致。语义示例：

```ts
editor.modes.define({
  name: "pairs",
  content: {
    create: () => ({ enabled: true }),
  },
  view: {
    create: () => ({ inserted: 0 }),
  },
  actions: {
    quote(ctx) {
      ctx.viewState.inserted++;
      return ctx.stop([
        ctx.view.insertText('\"\"'),
        ctx.view.moveLeft(1),
      ]);
    },
  },
  keys: {
    '\"': "quote",
  },
});
```

定义中的 callback 保存在 V8 CallbackRegistry。`ScriptMode` 只保存稳定的
callback identity 和静态 keymap/presentation metadata，不保存宿主可变引用。

## 8. 状态语义

正式 Script Mode 状态只有：

```text
contentState: 每 (ModeId, ContentId) 一份
viewState:    每 (ModeId, ViewId) 一份
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
checkpoint 这两份状态；callback 和后续 effect 成功后安装新值，执行帧失败时由
现有 Mode state snapshot 恢复。

模块级变量和闭包状态遵循普通 JavaScript 语义，长期存在但不属于编辑器事务。
脚本抛错前已经发生的纯 JavaScript 全局状态变化不回滚。这与文件句柄、缓存等
runtime 外部资源一致，不尝试为整个 V8 heap 制造伪事务。

## 9. Callback 调用边界

callback 获得一次调用期间有效的只读 context 和可变的 Script Mode state 草稿。
context 可以读取 owned/共享快照，但不得保存宿主引用。

```text
Rust Mode state checkpoint
-> 建立 V8 callback scope
-> 暴露只读 context 和 state draft
-> 调用 JS function
-> 提取 ScriptResult、新 state 和 presentation snapshot
-> 验证全部返回值
-> 退出 V8 scope
-> app 按顺序执行 ModeEffect
```

V8 callback 内不得重新进入 app command executor。跨 Mode 操作只返回
ModeCommand effect，等 callback scope 退出后再由 app 深度优先执行。

`onContentChanged` 等被动 callback 只能更新所属 Mode state，不能返回编辑或
其他宿主 effect。需要修改文本的行为必须由 input 或显式 action 触发，避免隐式
递归编辑循环。

## 10. Script Result

Script callback 返回的数据映射为：

```text
ScriptResult
├── flow: continue | stop
├── effects: ScriptEffect[]
├── contentState
├── viewState
└── presentation snapshot（按 callback 类型可选）
```

adapter 必须在执行任何宿主 effect 前完整验证返回值。未知 effect、错误参数、
非法坐标、冲突编辑和超出预算都使当前 callback 失败，不允许部分执行。

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

input/view action 可以返回基于当前 selections 的编辑，例如：

```ts
ctx.view.insertText("hello")
ctx.view.deleteBackward(1)
ctx.view.moveLeft(1)
```

这些操作映射成现有 `EditCommand`，并在 effect 实际执行时依据当时的 View
selections 解析。多个有序 effect 可以观察前一个 effect 已成功产生的状态。

## 14. Content edit batch

content/view action 都可以对所属 Content 返回绝对范围编辑：

```ts
ctx.content.applyEdits({
  revision: ctx.content.revision,
  edits: [
    {
      range: {
        start: { line: 1, character: 4 },
        end: { line: 1, character: 7 },
      },
      text: "replacement",
    },
  ],
})
```

语义为：

- 一个 batch 的全部范围基于同一份 callback 输入快照；
- batch 必须携带该快照 revision；
- revision 不匹配时整体失败；
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
-超时、heap limit、转换或 effect 验证失败类别。

主动 callback 错误映射为 `ModeError`，交给现有 InputExecutionFrame 恢复本次
编辑器状态。被动 callback 错误使用现有 attachment fault isolation，不阻止基础
文本编辑。

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
└── mode           ScriptMode adapter 和 ScriptEffect 映射
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

每一步都必须保持 native Vim、Tree-sitter、事务、保存、viewport 和 TUI 行为，
并通过项目要求的测试、clippy、fmt 和空白检查。
