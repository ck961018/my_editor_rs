# TypeScript 脚本架构

**状态：** 当前实现

**更新日期：** 2026-08-11

## 1. 定位

`vell-plugin-v8` 是 TypeScript 到通用 Mode contract 的具体 adapter。它使用
`rusty_v8` 执行脚本，使用 `deno_ast` 转译 TypeScript，但不建立第二套
编辑器内核。

```text
embedded plugins / optional config.ts
-> ScriptHost
-> ScriptMode
-> vell-mode contract
-> vell-app operation executor
-> Content / View / history / presentation
```

命令是第二条入口。它同样以 `vell-mode` 契约结束，不绕开执行帧：

```text
source
├── TypeEnvironment  独立 isolate 中的 checker 与生成声明
└── ScriptHost       转译、执行、保留 JS 状态
        └── editor.commands proxy
                └── scoped CommandHost
                        └── vell-app ExecutionFrame
```

`vell-app` 的普通依赖不含 V8。根二进制先调用
`vell_plugin_v8::load_user_configuration()`，再把 Mode、View extension、
owned `CompoundViewDefinition`、后台运行时、ThemeName 和纯 protocol Face
override DTO，以及 owned `EditorOptions` 注入 App。`prepare_commands()`
在同一步安装 native 命令的 TypeScript 视图，并返回语言中立的
`CommandEntry` 列表。V8 类型不跨出 `vell-plugin-v8` 的公共边界。

## 2. 加载与所有权

构建脚本把 `runtime/plugins/` 的清单、TypeScript、worker 和资源嵌入
`vell-plugin-v8`。启动时：

1. 枚举内嵌 `plugin.json`；
2. 按 manifest `order` 稳定加载入口；
3. 在同一 `ScriptHost` draft 中收集 Mode、View definition、View extension、
   `EditorOptions` 和视觉配置；
4. 加载可选用户 `config.ts`；
5. 把 Mode definition 包装为 `ScriptMode`，把 View definition 转成 owned
   `CompoundViewDefinition`；
6. 将通用 Mode、View definition、View extension 与视觉 DTO 交给 App
   bootstrap。

所有 `ScriptMode` 和一个独立 `ScriptBackground` 通过
`Rc<RefCell<ScriptHost>>` 共享主 isolate、context、module map、callback
registry 和 diagnostics。Mode definition 进入 `ModeRegistry`；后台运行时进入
App 的 background owner 列表，因此没有 Mode 的插件也能持续泵送 Worker。
App 不识别具体 ScriptHost 类型。

内建插件失败表示安装损坏，会阻止启动。可选用户配置失败会输出 warning，
原子回滚该模块新增的 Mode、View definition、View extension、命令、Theme
选择、Face override 和 `EditorOptions`，并继续使用内建配置。

`editor.configure` 是模块加载期的 typed 配置入口。当前只开放 session 级
BufferView gutter 默认值；字段级调用会合并进 `ScriptConfigurationDraft`，
宽度在 V8 边界严格校验为 1 到 16 的整数。Mode action、View extension
callback、交互求值和 Worker 都不能修改该启动配置。

App bootstrap 先把 `CompoundViewDefinition` 注册到 Kernel 的唯一
`ViewDefinitionRegistry`，再创建初始 session 并安装 extension。definition
只携带 binding schema、两个子 View recipe 和布局方向，不携带 V8 callback。
卸载时，活动 View、Mode attachment rule 或 extension 仍引用该 definition
都会使操作失败；通过检查后才按 owner 一次移除。

## 3. 配置发现

用户配置只从以下位置加载：

- `VELL_CONFIG` 指定的文件；
- Windows：`%APPDATA%\vell\config.ts`；
- Linux/macOS：`$XDG_CONFIG_HOME/vell/config.ts`；
- fallback：`$HOME/.config/vell/config.ts`。

编辑器不会自动执行当前工作目录或所打开项目中的脚本。

用户配置支持 `.ts`、`.js`，以及配置目录内的静态和动态相对 import。
以下能力被拒绝：

- URL 与裸 package specifier；
- CommonJS `require`；
- top-level await；
- 越出配置根目录的路径；
- Node、Deno、网络、timer、子进程和任意异步文件 API。

## 4. 公开 schema

`runtime/editor.d.ts` 是公开 TypeScript schema 的唯一真相源，并通过
`TYPESCRIPT_DECLARATIONS` 内嵌到 Rust API。`runtime/commands.generated.d.ts`
是它的生成伴随文件，描述当前 native 命令签名；它由 Rust schema 决定，并由
contract test 保证与 `NATIVE_COMMAND_IDS` 一致。CI 对声明、内建插件和迁移
示例运行严格类型检查。

adapter schema 使用 ContentKind：

```ts
editor.modes.define({
  name: "pairs",
  on: {
    buffer: {
      state: () => ({ inserted: 0 }),
      viewState: () => ({ enabled: true }),
      commands: {
        quote(ctx) {
          if (!ctx.viewState.enabled) return ctx.pass();
          ctx.state.inserted++;
          ctx.edit.insert('""');
          ctx.cursor.moveLeft();
        },
      },
      keys: { '"': "quote" },
    },
  },
});
```

Buffer adapter 获得静态 context。Buffer context 暴露
资源名、路径、载体状态、脏状态、保存结果和文本统计。
状态栏呈现不经过 adapter：buffer mode 通过
`viewState.viewPolicy.statusBar` 定制左、中、右分段及 Face，
app 在 render query 层按状态栏 Pane 组装。Buffer context 不暴露
Pane 或 Space 标识。

View API 有两个不同深度的入口：

- `editor.views.extend(target, definition)` 为已有 View definition 增加
  host-supported Pane。render callback 只接收 owned snapshot，只返回受限
  presentation；缓存完成后 TUI 不再进入 V8。
- `editor.views.define(definition)` 注册完整复合 View recipe。schema 在 V8
  内解析后立刻转成 owned `CompoundViewDefinition`；App 只看到 binding、
  子 View 映射和布局方向，不看到脚本 factory 或布局树。

两种注册都只允许出现在模块加载 draft 中，并按模块 owner 原子发布或回滚。

## 5. ScriptMode 与状态

脚本定义中的 callback 保存在 V8 callback registry。`ScriptMode` 只保存稳定
callback identity、静态 keymap/adapter 元数据、attachment rule 和共享
host，不保存可变 App、Content 或 View 引用。schema 把 `attach.view`、
可选 `attach.binding` 和 `attach.languages` 转成通用
`ModeAttachmentRule`；省略 `attach` 时使用 BufferView `document` 的兼容
默认值。分类与具体 View 的 attachment 决策留在 app 的
`ContentClassifier` 和 `ModeResolver`，V8 adapter 不重复实现。

正式脚本状态只有：

```text
state:     每 (ModeId, ContentId) 一份
viewState: 每 (ModeId, ViewId) 一份
```

省略 `attach.binding` 的 View-only Mode 使用 `on.view`，只实例化一份 View
`state`，不会运行 content `state` factory，也不会在 `ModeContentStore` 中
创建占位项。它适合只依赖 View definition 的父级行为；Content-bound
`on.buffer` adapter 仍使用完整的 `state + viewState` 对。

状态必须是 JSON-compatible owned data：null、boolean、number、string、
array 和普通 object。函数、Promise、V8 handle、循环引用、host object 与
非有限数值不能进入持久 Mode state。

每次 callback 读取当前 Mode draft，并在返回时完整提取和验证新 state。宿主
operation 成功后才提交 draft。callback、返回值或后续 operation 失败时，
draft 被丢弃。JavaScript module global 与闭包状态遵循 V8 语义，不参与宿主
rollback。

## 6. Callback 边界

```text
create Mode draft
-> build callback-scoped context
-> call V8 function
-> collect flow, state, operations and presentation
-> validate all output
-> leave V8 scope
-> app executes OperationRequest in order
-> frame success publishes state
```

Context 中的 native function 只在当前 invocation 有效。保留旧 context 并在
callback 结束后调用会被拒绝。

V8 callback 不重入 app executor。`ctx.commands.invoke("mode.command")` 只把
typed Mode invocation 暂存到结果；app 在 scope 退出后深度优先执行。

command 正常返回 `void` 表示 Stop；只有 `return ctx.pass()` 继续到
下一 Mode。普通返回值不承载 mutation。

## 7. Native primitives

Buffer adapter 按能力安装：

- `ctx.cursor`：selection/cursor movement；
- `ctx.edit`：selection-relative edit 与绝对 edit batch；
- `ctx.history`：transaction、undo 和 redo；
- `ctx.viewport`：滚动与 cursor alignment；
- `ctx.commands`：限定 Mode command；
- `ctx.app`：受限的 App operation。

每次调用立即校验参数，并追加 typed `OperationRequest`。单 callback 的
operation 上限来自 `vell-mode` 的共享常量，不能与 App frame 上限漂移。

绝对 edit batch 使用零起点 UTF-16 `line/character`，并绑定 callback 开始时
捕获的 Content snapshot。adapter 拒绝：

- 落在 surrogate pair 中间的位置；
- 越界、倒序或互相重叠的 range；
- stale snapshot；
- 超过结构化输入或 operation 预算的结果。

合法 batch 一次转换为 `TextChangeSet`，由 App 统一获得 history、selection
映射、undo/redo 与 rollback。

## 8. 命令与 `editor.commands`

命令与 Mode 无关。`editor.commands` 是正式命令的权威命名空间：

```ts
editor.commands.register(formatDocument)
editor.commands.register("math.increment", increment)
editor.commands.shortcut("wq", async () => {
  await content.save()
  quit()
})
```

`register` 返回传入的 callable，因此局部类型推断不丢失。ID 是一个或多个点分
TypeScript 标识符，按点构造嵌套 namespace object。同 ID 重新注册原子替换
namespace 叶子、bare global fallback 和宿主 `CommandRegistry` 中的实现，
并保留该节点下已注册的子命令。`$commandLine`、`$script`、`register` 和
`shortcut` 是保留根，不能被脚本占用。

宿主为每个 native 命令安装 callable wrapper，使 native 与 TS 命令具有相同的
函数调用体验。bare global fallback 只在该名称当前是 undefined、或仍是宿主
自己安装的 fallback 时才写入，因此词法绑定和用户自定义 global 优先；
`editor.commands.content.save()` 始终明确调用正式命令。

`shortcut` 只接收一个原始可选参数 `string | undefined`。传入未注册回调时，
宿主保留私有 callback identity：它不进入公开命令补全，但仍使用正常的命令
预算、错误和执行 frame。

调用期间，`ScriptCommandAdapter` 通过 RAII guard 安装 scoped host。guard 在
成功、异常、timeout 和 termination 路径都会清除，因此 callback 结束后保存的
context 或 wrapper 无法访问旧 frame。同一 isolate 内的 TS-to-TS 调用保留普通
JavaScript value；只有跨 Rust 边界的参数与结果才转换为 owned JSON。

命令注册属于 definition state。它立即更新 V8 命令视图、宿主 registry 和类型
环境，即使同一次执行随后抛错也不回滚。

## 9. 持久 global script evaluator

除 module 之外，`ScriptHost` 还提供一个持久 global script environment。交互
求值、buffer 求值和直接运行的 TS 文件共享同一份 global 绑定：

```text
$script.evaluate
├── Interactive  一次 : 或 API 输入
├── Buffer       buffer 中的 selection 或顶层节点
└── File         直接运行的 TS 文件
```

普通 global function、variable 和 closure 按 JavaScript global script 语义跨
执行保留；未注册的普通函数不会出现在 `editor.commands`。插件与用户配置继续
使用 ES module 作用域，module-local 绑定不泄漏到 global script。

global script 不支持 top-level `await`，静态 `import` 也只属于 module 路径；
script 使用动态 `import()`。异步入口写成普通函数并调用它。外部入口自动等待
最终返回的 Promise，但不追踪脚本既没有返回、也没有 `await` 的 Promise。

语法错误和运行时异常只失败当次求值，不损坏主 isolate 或已保留的 global。

## 10. 增量 TypeScript 类型环境

`TypeEnvironment` 在**独立 isolate** 中运行官方 TypeScript compiler bundle。
用户脚本无法访问该 isolate；它只提供 language-service 数据，不拥有运行时
registry。实际成功的注册事件是声明发布的唯一来源。

虚拟 project 由内嵌 `editor.d.ts`、必要的 `lib.*.d.ts`、module source、
global script history 和生成的 `commands.generated.d.ts` 组成。注册调用携带
source identity 与 span，checker 据此推导 handler 参数、返回值和 Promise
类型，并原子替换被重定义命令的旧声明。无法静态定位的动态注册退化为安全的
`unknown` 签名，不伪造具体类型。

compiler isolate 有独立的 heap 与超时预算。它发生 fault 时被禁用并记录原因，
执行 isolate 与命令 registry 继续正常工作。

bundle 随源码管理，Cargo 只读取已 check-in 的文件；构建不调用网络、Node 或
pnpm。来源、license 与升级流程见 [vendored TypeScript compiler][bundle]。

[bundle]: ../../crates/vell-plugin-v8/vendor/typescript/README.md

## 11. 命令行分类与分派

`ExecuteCommandLine` 到达固定服务命令 `$commandLine.execute`。分类只发生在
这里，Rust app 层没有 Vim 或 Ex parser 分支：

```text
trim source
├── 单条以已注册命令为根的调用表达式 -> 正式命令调用
├── 首个 token 是已注册 shortcut      -> shortcut(tail)
├── 首个 token 是 ts                  -> global script 求值
└── 其他                              -> 结构化错误
```

函数形式必须是**一条**以 `editor.commands` 中命令为根的调用表达式。裸名称
形式在求值前被重写为显式 `editor.commands.` 前缀，因此参数表达式仍是普通
TypeScript，可以调用普通函数和 nested command，但根名称只从命令表解析。
顶层声明或多条语句被拒绝，并提示改用 `ts`。

shortcut 分派移除名称与参数之间的分隔空白：没有非空白参数时以零个参数调用，
否则把剩余文本作为一个字符串传入。系统不拆分引号、flag、range 或位置参数。

`ts` 带参数时求值该参数；不带参数时求值当前 buffer：非空 selection 优先，
否则取光标所在的完整顶层语句或声明。无法得到完整节点时报告语法错误，不猜测
物理行。位置换算使用字符索引，覆盖非 ASCII 源码。

## 12. Presentation

Content-bound `on.buffer` 可以定义 named Face、content decoration、view
decoration 和 View policy。callback 返回的数据转换为 owned Rust
presentation layer。View-only `on.view` 的可见区域由 View extension 呈现。

render path 不进入 V8：

```text
Script callback
-> Rust presentation snapshot
-> PresentationLayerStore
-> AppQuery visible-range clipping
-> RenderQuery
-> SceneRenderer
```

Content decoration 带 Content revision。文本变化后，旧 decoration 可先通过
`ContentChange` 映射，直到新的异步 snapshot 安装，避免空白高亮帧。

## 13. Worker 平台原语

`Worker` 是 ScriptHost 级平台能力，不属于 Mode definition。插件顶层可以创建
module Worker，主 isolate 定期泵送其消息：

```ts
const worker = new Worker(
  new URL("./worker.ts", import.meta.url),
  { type: "module" },
);
worker.onmessage = (event) => {
  const { contentId, revision, spans } = event.data;
  editor.writeDecorations(contentId, revision, spans);
};
```

每个 Worker 使用独立 OS 线程和 V8 isolate。主线程与 Worker 只传递
JSON-compatible owned data；通信是主线程与各 Worker 之间的星形拓扑。
Worker 可静态或动态 import 同一插件根目录内的 module，也可在配额和深度限制
内嵌套创建 Worker。

`ScriptHost` 持有 Worker registry、配额和 decoration sink。Mode adapter
只调用脚本 callback，不负责 Worker 生命周期或消息轮询。事件循环每个 worker
poll tick 调用 ScriptHost pump；即使插件没有定义 Mode，顶层 Worker 也会继续
接收消息。

`editor.writeDecorations(contentId, revision, spans)` 在主 isolate 接收结果。
它以 `(ContentId, revision)` 校验 live Content，过期结果不会进入 presentation。
这层安全不依赖 Mode job slot、generation 或 analysis apply。

Worker module graph 和资源读取都限制在插件根目录。它没有网络、timer、DOM、
Node API、共享内存或任意文件访问。Promise 通过受控 microtask pump 完成；
取消、超时、heap exhaustion 和未捕获异常只终止对应 Worker。

## 14. 预算与恢复

当前默认限制：

- 普通 callback 2 秒，module startup 5 秒；
- worker request 30 秒；
- compiler isolate startup 10 秒，单次查询 2 秒，heap 256 MiB；
- isolate heap 128 MiB，另保留 16 MiB 终止恢复余量；
- 单个脚本或 module 4 MiB，module graph 16 MiB；
- 普通 JSON state/result 4 MiB，结构化输入 32 MiB；
- 单 callback 最多 255 个 operation；
- 注册命令最多 256 层嵌套调用；
- 单次 presentation 最多 100,000 个 decoration。

命令调用和 operation 共享 `ExecutionFrame` 预算，因此 TypeScript 命令无法
通过嵌套调用绕开单 frame 的 operation 上限。

主 isolate 的 watchdog 在线程中持有 `IsolateHandle`。超时或 heap pressure
触发 V8 termination；termination 传播出 scope 后，RAII 清理 watchdog，
恢复 terminate 状态与 heap limit。只有 runtime 可安全恢复时才继续调用。
resume 的 Promise continuation 走同一条 watchdog 与 heap 恢复路径。

所有大小、超时、转换和 presentation 检查都发生在发布 state、operation 或
cache 之前。

## 15. 故障隔离

主动 input/command callback 错误映射为 Mode fault，并使当前 execution frame
失败。App 恢复 Content、View、input 与 history checkpoint，丢弃 operation 和
Mode draft，但事件循环继续。

被动 content-change、presentation 或 state factory 失败时，只 fault 对应
attachment。Worker 错误通过自身 `error` 事件隔离。基础文本编辑、其他 Mode
与渲染继续工作；诊断包含 Mode、callback phase 和 message。

命令错误映射为 `CommandError`，并按来源呈现：命令行输入进入现有 status bar
与诊断路径，键位调用忽略返回值但仍报告异常。同步失败回滚该 frame 的宿主
mutation，但不回滚 JavaScript heap、global binding、closure、命令与快捷命令
注册，以及已发布的类型声明。compiler isolate fault 只禁用类型服务。

主 isolate 由全部 ScriptMode 共享，因此这些限制不是恶意代码的进程级隔离。
在需要自动运行不受信任插件前，必须重新评估 isolate 或进程边界。

## 16. 物理模块

```text
vell-plugin-v8::script
├── mod              façade、加载、共享运行时类型
├── host             isolate、context、definition 与 callback registry
├── invocation       调用、microtask、watchdog 与 heap 恢复
├── mode_adapter     ScriptMode、状态与 presentation 接线
├── module           本地 ES module graph 与 TypeScript 转译
├── bridge           Rust、JSON 与 V8 值转换
├── schema           definition 解析
├── primitives       callback-scoped native function
├── commands         editor.commands、scoped host、adapter 与 Promise
├── global_script    持久 global script environment
├── command_line     命令行分类、shortcut 分派与 buffer 求值
├── type_environment compiler isolate 与生成声明
└── worker           后台 isolate、资源、取消与 Promise
```

依赖方向保持：

```text
vell-plugin-v8 -> vell-mode + vell-core + vell-protocol
vell-app       -X-> vell-plugin-v8
vell-tui       -X-> vell-plugin-v8
```

脚本作者使用的 API 见 [`docs/scripting.md`](../scripting.md)。
