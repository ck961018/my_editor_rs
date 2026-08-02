# 命令系统 Roadmap

**状态：** 计划中（设计已确认，尚未实现）

**更新日期：** 2026-08-02

## 1. 目标

本路线图把命令提升为独立于 Mode 的一等运行时能力，并提供统一的 Rust、
TypeScript 和 `:` 调用体验。

最终使用方式如下：

```ts
function formatAndSave(): Promise<void> {
  formatDocument()
  return save()
}

editor.commands.register(formatAndSave)

editor.commands.shortcut("wq", async () => {
  await save()
  quit()
})
```

```text
:formatAndSave()
:wq
:ts function helper() { return currentBufferId() }
```

交付完成后必须满足：

- Rust 原生命令与 TS 注册命令具有相同的函数调用体验；
- `editor.commands` 是正式命令的权威命名空间；
- 普通 TS 可直接调用命令，也可显式访问完整 `editor` API；
- `:` 支持单个命令调用、注册快捷命令和 `ts` 求值；
- TS 脚本、交互求值和 buffer 求值共享持久全局环境；
- 同步命令在一个 `ExecutionFrame` 中组合并返回真实结果；
- 异步命令使用标准 Promise，`await` 是 frame 提交边界；
- 动态注册立即更新持久 TypeScript 类型环境；
- App 继续不依赖 V8，Mode callback 的现有执行模型保持不变。

本路线图遵守以下当前架构文档：

- [Core 与 workspace 依赖方向][core-deps]；
- [Command 与执行所有权][execution-ownership]；
- [TypeScript 脚本架构][typescript-architecture]；
- [可组合 Mode 架构][mode-architecture]。

实现命令系统时，需要同步更新其中已经被新能力扩展的“当前实现”描述。

[core-deps]: ../design/core-dependency-direction.md
[execution-ownership]: ../design/command-execution-ownership.md
[typescript-architecture]: ../design/typescript-scripting-architecture.md
[mode-architecture]: ../design/composable-mode-architecture.md

## 2. 已确认的产品语义

### 2.1 正式命令

普通函数定义不会自动注册命令：

```ts
function increment(value: number): number {
  return value + 1
}
```

此时 `increment()` 只是普通 TS 函数。显式注册后，它才获得稳定
`CommandId`：

```ts
editor.commands.register(increment)
editor.commands.register("math.increment", increment)
editor.commands.register("save", customSave)
```

注册规则为：

- `register(namedFunction)` 使用函数名作为 ID；
- `register(id, function)` 使用显式 ID；
- ID 由一个或多个点分 TS 标识符组成；
- 重复 ID 替换当前实现，包括 Rust 原生命令；
- `register` 返回传入的 callable，保留局部类型推断；
- 注册结果进入 `editor.commands`，不是普通全局函数表。

命令命名空间按 ID 构成树：

```ts
editor.commands.math.increment(1)
```

普通 TS 中，词法绑定和普通全局绑定优先。找不到裸名称时，再回退到
`editor.commands`：

```ts
save()
editor.commands.save()
```

`editor.commands.save()` 始终明确调用正式命令。命令名与普通局部函数同名时，
裸调用遵循标准 TS 词法作用域。

### 2.2 快捷命令

快捷命令是文本入口，最终指向正式命令：

```ts
editor.commands.shortcut("q", quit)

editor.commands.shortcut("wq", async () => {
  await save()
  quit()
})
```

传入未注册回调时，运行时为它创建私有命令 identity。私有 identity 不进入
公开 TS 命令补全，但仍使用正常的命令预算、错误和执行 frame。

快捷命令只接收一个原始可选参数：

```ts
type ShortcutHandler = (argument: string | undefined) => unknown
```

分派器移除名称与参数之间的分隔空白。没有非空白参数时以零个参数调用；
否则把剩余文本作为一个字符串调用。系统不拆分引号、flag、range 或位置参数。

### 2.3 `:` 输入

`:` 支持三条明确路径：

```text
:save()                 单个正式命令调用
:wq                     注册快捷命令
:ts const value = 1     普通 TS script
```

函数形式必须是一条以 `editor.commands` 中命令为根的调用表达式：

```text
:openFile(projectRoot() + "/README.md")
:switchBuffer(newBuffer())
```

参数仍是普通 TS 表达式。这条限制用于分派和诊断，不是安全沙箱。
顶层声明或多条语句必须使用 `ts`：

```text
:const value = 1        拒绝，并提示使用 :ts
:save(); quit()         拒绝，并提示使用 :ts
:ts const value = 1     接受
```

`:` 的输入状态和按键处理继续属于 Vim 插件。Vim 插件只提交原始文本，
不再维护独立的命令执行器。通用命令服务负责解析和执行。

### 2.4 TS 求值

`ts` 快捷命令有两种行为：

```text
:ts <source>    执行 source
:ts             执行当前 buffer 中的 TS
```

buffer 求值按以下顺序选择源码：

1. 有非空 selection 时执行 selection；
2. 否则执行光标所在的完整顶层语句或声明；
3. 多行声明按语法节点整体执行；
4. 无法得到完整节点时报告语法错误，不猜测物理行。

交互求值、buffer 求值和直接运行的 TS 文件共享一个持久 global script
environment。普通函数、变量和闭包按 JavaScript global script 语义保留。
现有插件与配置继续使用 ES module 作用域。

持久 script 不支持 top-level `await`。异步入口使用普通函数：

```ts
async function main(): Promise<void> {
  await save()
}

main()
```

外部入口会自动等待最后返回的 Promise。

### 2.5 返回值、异常和异步

同步命令立即返回。嵌套命令调用共享最外层 `ExecutionFrame`，后续调用可观察
前序 provisional 状态。未捕获异常回滚该同步段的 Content、View、input、
history、Mode draft 和 prepared effect。

以下状态不参与宿主 rollback：

- JavaScript heap；
- global script binding；
- closure state；
- 命令与快捷命令注册；
- 增量 TypeScript 类型环境中的已发布定义。

真正异步的命令返回标准 `Promise<T>`。每个实际 `await` 提交当前 frame，
恢复 continuation 时创建新 frame。恢复后的 frame 继续绑定命令启动时的
Client、View 和 Content；目标已关闭或 revision 失效时 reject，而不是改用
当前焦点。

`save()` 返回实际保存完成的 `Promise<void>`。组合保存与退出必须显式排序：

```ts
async function writeQuit(): Promise<void> {
  await save()
  quit()
}
```

不追踪或自动等待脚本没有返回、也没有 `await` 的任意 Promise。

交互入口保留最终结果，但当前没有通用消息区，因此第一版不显示结果。
键位调用也忽略最终结果。异常继续使用现有诊断和可见错误路径。

## 3. 非目标

本路线图不包含：

- Vim Ex 的 range、bang、bar、缩写和统一参数语法；
- command palette、命令历史或新的通用命令输入 UI；
- 返回值消息区；
- global script 的 top-level `await` 转换；
- 自动等待所有未消费 Promise 的非标准 structured concurrency；
- 把 Mode-local command 合并进全局命令表；
- 改变 Mode callback 的 operation 收集与 draft 模型；
- 允许 TS 保存可变 App、Content、View 或 V8 host object；
- 把主 isolate 变成不受信任代码的安全沙箱；
- 在 Rust 中实现自定义 TypeScript 类型检查器。

类型环境会为未来 diagnostics 和补全提供基础，但本路线图不建设对应 UI。

## 4. 当前基础与缺口

可以直接复用的能力：

- `vell-mode::Command` 输入 envelope 与 typed `OperationRequest`；
- `vell-app::ExecutionFrame`、checkpoint、prepared effect 和预算；
- `ModeRegistry` 的 native/script adapter 擦除模式；
- `ScriptHost` 的持久主 isolate、module map、watchdog 和 microtask pump；
- `deno_ast` 的 TypeScript parse、span 和 transpile 能力；
- 当前 Vim `:` prompt、状态栏错误呈现和输入测试；
- 保存任务、revision 校验和 Worker completion 回流。

当前缺口：

- `Command` 只是输入 action envelope，不是全局 callable registry；
- Mode command 依赖 Mode attachment，不能充当独立命令；
- V8 primitive 只收集 operation，不能同步返回 mutation 结果；
- `ExecutionFrame` 不提供脚本可用的 scoped synchronous host；
- 当前 Vim 插件硬编码 `executeCommand()` parser；
- `execute_typescript` 没有持久交互 source 与 command namespace；
- `deno_ast` 只转译，不提供完整 TypeScript type checker；
- 保存完成没有与 V8 Promise resolver 建立关联。

不要为了新系统重命名现有 `Command` enum。新类型使用 `CommandId`、
`CommandInvocation`、`CommandEntry` 和 `CommandRegistry`，避免无关重构。

## 5. 目标架构

```text
key / Vim : / explicit API / TS
-> CommandInvocation or ExecuteCommandLine
-> CommandRegistry
-> NativeCommandAdapter or ScriptCommandAdapter
-> scoped CommandHost
-> typed request / query
-> app target resolver
-> current ExecutionFrame
-> owned result or Promise completion
```

TypeScript 路径额外包含：

```text
source
├── TypeEnvironment: check / infer / update virtual declarations
└── ScriptHost: transpile / execute / retain JS state
        └── editor.commands proxy
                └── scoped CommandHost
```

### 5.1 Crate 所有权

| Crate | 新职责 |
| --- | --- |
| `vell-mode` | 命令 ID、entry、registry、adapter 和 host contract |
| `vell-app` | 目标解析、同步执行、frame、异步 continuation 和结果 |
| `vell-plugin-v8` | TS registry adapter、求值器、Promise 和类型环境 |
| `runtime/` | Vim prompt、内建快捷命令和 TS 使用示例 |
| 根二进制 | 组装 native registry、ScriptHost、App 和 background owner |

`vell-protocol` 不承担本地命令执行。只有将来远程前端确实需要枚举或调用命令
时，才把最小 DTO 下沉到 protocol。

### 5.2 语言中立契约

`vell-mode` 增加：

- `CommandId`：校验点分 TS 标识符；
- `CommandInvocation`：ID、owned arguments 和调用来源；
- `CommandEntry`：ID、adapter identity 和最小诊断元数据；
- `CommandRegistry`：注册、替换、查找和稳定迭代；
- `CommandAdapter`：native 与 script 的统一调用接口；
- `CommandHost`：typed mutation、query、nested invoke 和 async task seam；
- `CommandError` 与完成状态。

跨 Rust/V8 边界的参数和结果使用 JSON-compatible owned value，并为
ContentId、ViewId 等稳定 ID 提供明确 DTO。完全发生在同一 V8 isolate 内的
TS-to-TS 调用保留普通 JavaScript value，不强制序列化。

Registry 不保存 V8 类型。`ScriptCommandAdapter` 只暴露 opaque callback
identity 和语言中立结果。

### 5.3 Scoped synchronous host

命令执行不能复用 Mode primitive collector。App 为一次同步执行段构造
`CommandHost` facade，并只在 V8 调用期间安装：

```text
install scoped host
-> call script
-> nested native/TS commands use the same host and frame
-> collect result or pending Promise
-> clear scoped host
```

Host 只暴露 typed request 和 owned query，不借出 App、Kernel、Session、
Content 或 View。若 rusty_v8 callback 要求不可表达的 scoped lifetime，
非 owning pointer 必须限制在一个经过审计的 bridge 模块中，并由 RAII guard
在成功、异常、timeout 和 termination 路径全部清除。

命令调用和 operation 共享 frame 预算。递归命令链另设深度预算，防止 TS、
native 和 shortcut 互相递归耗尽栈。

### 5.4 Definition state 与 rollback

命令和快捷命令注册属于 ScriptHost definition state，不属于文本事务。
注册调用立即更新当前 V8 命令视图，并通过 typed registration event 更新
`CommandRegistry` 和 `TypeEnvironment`。

即使同一 TS 执行随后抛错，已经完成的注册仍然保留。Host mutation 仍只能通过
typed request 进入 `ExecutionFrame`；definition event 不能携带 Content 或
View mutation。

## 6. 里程碑

### M1：语言中立命令契约

**目标：** 在没有 V8 和 UI 的情况下注册并调用 Rust 命令。

交付：

- 在 `vell-mode` 实现 `CommandId`、entry、registry 和错误类型；
- 支持确定性的替换语义和稳定迭代；
- 定义 native/script 共用的 adapter 与 host trait；
- 为 nested invocation、owned argument/result 和 async completion 留下最小
  contract；
- 给现有输入 `Command` envelope 增加 registered invocation 路径；
- 不迁移或删除现有 Mode-local command。

验收：

- 合法与非法点分 ID；
- native 注册、调用和返回值；
- 同 ID 替换后旧 key binding 调用新实现；
- 不存在命令、参数错误和递归预算；
- registry 与 V8、app、TUI 无依赖。

### M2：App 同步命令执行

**目标：** 让命令在当前 `ExecutionFrame` 内立即执行 typed request。

交付：

- `Kernel` 持有 `CommandRegistry`；
- app 实现 scoped `CommandHost` facade；
- nested command 调用共享最外层 frame；
- typed query 返回 owned snapshot 或 ID；
- outcome-bearing operation 可在后续命令中使用；
- 为需要立即返回 ID 的 lifecycle 操作建立最小 provisional journal；
- definition event 与 host rollback journal 明确分离；
- 把第一批原生 buffer、save、history、view 和 app command 注册为函数。

不要建立第二个 ContentStore 或通用 shadow App。仅为真实需要立即观察的
operation 增加 provisional 数据；其他外部 effect 继续在成功后发布。

验收：

- `const id = newBuffer(); switchBuffer(id)` 可组合；
- 前序 edit 可被后序 query 观察；
- nested command 任一步失败时同步段整体回滚；
- JS definition state 不进入 host rollback；
- prepared save、quit 和 topology effect 失败时不发布；
- 现有 Mode 与输入 frame 测试保持通过。

### M3：V8 命令 adapter 与 `editor.commands`

**目标：** 在主 isolate 中统一调用 native 与 TS 命令。

交付：

- `editor.commands.register` 两个重载；
- `editor.commands.shortcut` 与私有 callback identity；
- 根据 `CommandId` 构造嵌套 namespace proxy；
- 为 native 命令安装 callable wrapper；
- 为未被词法绑定遮蔽的命令安装 global fallback；
- TS command callback registry 与重定义；
- scoped host 安装、清除、异常转换和 watchdog 接线；
- `runtime/editor.d.ts` 与 Rust schema 同步。

验收：

- native 与 TS 命令使用同一种函数调用；
- `editor.commands.save()` 绕过同名普通函数；
- lexical binding 优先于裸命令 fallback；
- TS 覆盖 native 后，已有 ID invocation 使用新实现；
- TS-to-TS 返回 object、closure 或 Promise 时不被无谓 JSON 化；
- callback 结束后保存的 host wrapper 无法访问旧 frame；
- timeout、heap termination 和异常路径都清除 scoped host。

### M4：持久 TS script evaluator

**目标：** 提供接近编辑器内 REPL 的持久 global script 体验。

交付：

- 在现有主 `ScriptHost` 中增加 global script evaluator；
- 为每次交互、buffer 和文件执行分配稳定 source identity；
- 普通 global function、variable 和 closure 跨执行保留；
- 插件/config module graph 继续保持 module scope；
- 直接运行 TS 文件，共享 global script environment；
- 静态 import 仍走 module loader，script 使用动态 `import()`；
- 明确拒绝 global script top-level `await`；
- 外层识别最终 Promise，但异步调度在 M6 完成。

验收：

- 一个输入定义的普通函数可被后续 `:ts` 调用；
- 未注册普通函数不会出现在 `editor.commands`；
- 显式注册后可通过 ID 调用；
- module-local binding 不泄漏到 global script；
- 重复执行、语法错误和运行时异常不会损坏主 isolate；
- 现有插件加载、Worker 和 Mode callback 行为不变。

### M5：持久增量 TypeScript 类型环境

**目标：** 动态注册后，后续交互输入和脚本立即看到真实命令类型。

交付：

- 随 Vell 嵌入固定版本的官方 TypeScript compiler bundle；
- compiler 在独立 V8 isolate 中运行，不能被用户脚本访问；
- 建立包含 `editor.d.ts`、module source、global history 和生成声明的
  virtual project；
- 注册调用携带 source identity 与 span；
- 使用 checker 推导 handler 参数、返回值和 Promise 类型；
- 维护虚拟 `commands.generated.d.ts`；
- 重定义命令时原子替换旧声明；
- 无法静态定位的动态函数退化为安全 `unknown` 签名；
- Cargo 构建不调用网络、Node 或 pnpm；bundle 和 license 随源码管理；
- 提供独立的 compiler bundle 更新流程和版本一致性测试。

类型服务只提供 language-service 数据，不拥有运行时 registry。实际成功的
注册事件是声明发布的唯一来源。

验收：

- `register(namedFunction)` 推导完整签名；
- 显式 ID 与 inline arrow 推导完整签名；
- `:ts` 注册后，下一个输入立即获得该命令类型；
- buffer 求值注册后，其他 TS buffer 可查询相同类型；
- 重定义同步更新 bare global 与 `editor.commands` 类型；
- 动态 ID 和不可定位 callback 不伪造具体类型；
- compiler isolate fault 不破坏执行 isolate 或命令 registry；
- release 构建不依赖机器上的 `tsserver`。

### M6：Promise continuation 与异步 frame

**目标：** 支持真实异步结果，同时不让 frame 跨 await 常驻。

交付：

- 外部命令入口自动等待最终 Promise；
- pending Promise 暂停 V8 continuation 并提交当前 frame；
- completion 回流后用固定目标上下文创建新 frame；
- 保存任务与 Promise resolver 建立稳定 correlation；
- `save()` 在实际成功时 resolve，在冲突或 IO 失败时 reject；
- buffer close、目标 revision 变化、取消和 quit 清理 continuation；
- continuation 继续受 watchdog、heap 和 operation budget 限制；
- 不自动等待未返回且未 `await` 的 Promise。

验收：

- `:save()` 无需显式 `await`，但等待真实保存完成；
- `await save(); quit()` 不会在写入完成前退出；
- await 前的 frame 已提交，await 后异常只回滚新 frame；
- 等待期间切换焦点不会改变 continuation 目标；
- 关闭原目标会 reject，不会修改新的当前 buffer；
- stale save completion 不 resolve 错误的 invocation；
- 同步命令不因异步基础设施变成 Promise。

### M7：命令行与 Vim 接线

**目标：** 用通用命令系统替换 Vim 插件内的命令执行器。

交付：

- 增加语言中立 `ExecuteCommandLine` 请求；
- 函数形式只接受一个根 command call；
- 非函数形式按快捷命令名称分派；
- `ts` 快捷命令接入 global evaluator；
- selection 优先、否则提取当前顶层 TS 节点；
- Vim 插件注册 `q`、`w`、`wq`、buffer 与现有编辑快捷命令；
- 把现有 substitute 和 `set` 语义迁到 Vim 注册的 TS shortcut handler；
- 删除 Vim `executeCommand()` 中的硬编码执行分支；
- 错误继续通过现有 status bar/prompt 路径呈现；
- 结果显示保持关闭，等待未来消息区。

Vim 插件可以保留只用于兼容现有 Vim 输入拼写的薄 normalization，但不能再
直接执行 buffer、app 或 search operation。

验收：

- `:save()` 调用正式命令；
- `:q`、`:w` 和 `:wq` 调用注册快捷命令；
- `:ts` 执行参数或当前 buffer 语句；
- 普通 TS 声明不能伪装成 command call；
- shortcut 参数按 `string | undefined` 传递；
- 参数表达式可以调用普通 TS 和 nested command；
- Vim prompt Escape、Backspace、Enter 和错误恢复不回归；
- Rust 层没有 Vim、`wq` 或 Ex parser 分支。

### M8：文档、迁移与硬化

**目标：** 固定公共 contract，并删除临时兼容路径。

交付：

- 更新 `docs/design/command-execution-ownership.md`；
- 更新 `docs/design/typescript-scripting-architecture.md`；
- 更新 `docs/design/core-dependency-direction.md`；
- 更新 `docs/scripting.md`；
- 补充 native/TS command、shortcut、求值、async 和类型环境示例；
- 删除旧的独立-command-deferred 描述；
- 检查 runtime schema、Rust schema 和生成 declaration 一致性；
- 记录 compiler bundle 来源、license 和升级流程；
- 完成预算、故障恢复和跨 crate 依赖审计。

## 7. 依赖顺序

```text
M1 language-neutral registry
└── M2 app synchronous execution
    └── M3 V8 command adapter
        ├── M4 persistent evaluator
        │   └── M5 incremental TypeEnvironment ──┐
        └── M6 Promise continuation ─────────────┤
                                                └── M7 command line and Vim
                                                    └── M8 docs and hardening
```

M5 必须在交互命令系统宣布完成前交付。不能把动态类型环境降级为未来优化。
M6 可以在 M4 与 M5 的基础设施稳定后并行设计，但最终接线依赖两者。

## 8. 测试策略

### 8.1 `vell-mode`

- ID validation、namespace tree 和 replacement；
- registry iteration 与 lookup；
- native/script adapter 擦除；
- recursion 与 value budget；
- shortcut identity 和 typed error。

### 8.2 `vell-app`

- 顶层与 nested command 共用 frame；
- operation 后立即 query；
- provisional lifecycle result；
- Content、selection、history 和 input rollback；
- definition state 不随 host frame 回滚；
- await 前后 frame 分段；
- target pinning、revision 和取消；
- save Promise 与实际 completion correlation。

### 8.3 `vell-plugin-v8`

- native/TS callable parity；
- namespace proxy 与 lexical fallback；
- register、replace 和 shortcut；
- global script persistence 与 module isolation；
- scoped host 生命周期、timeout 和 termination；
- Promise auto-await 与 continuation resume；
- statement span 提取和 UTF-16 位置；
- TypeEnvironment 增量更新、推导、fault isolation 和 bundle version。

### 8.4 `runtime/`

- `runtime/editor.d.ts` contract；
- Vim prompt 到通用命令行请求；
- 内建快捷命令；
- 当前 Vim command 行为迁移；
- 交互注册后的生成声明 typecheck；
- 直接 TS 脚本和 buffer eval 示例。

## 9. 完成定义

以下场景全部通过时，命令系统完成：

```ts
const id = newBuffer()
switchBuffer(id)

function double(value: number): number {
  return value * 2
}

editor.commands.register("math.double", double)
editor.commands.shortcut("double", value => {
  if (value === undefined) throw new TypeError("value required")
  return math.double(Number(value))
})
```

- TS 能同步调用 native 命令并消费返回值；
- TS command 能调用 native 或其他 TS command；
- `:math.double(21)` 只从 `editor.commands` 解析根命令；
- `:double 21` 通过 shortcut 传入原始字符串；
- `:ts double(21)` 使用普通 TS 词法解析；
- buffer 当前语句执行后，全局定义立即可用于后续求值；
- 普通函数只有显式注册后才能通过命令入口调用；
- 动态命令签名立即进入增量类型环境；
- 同步错误回滚 host mutation，但不回滚 JS definition state；
- async save 的成功和失败通过 Promise 准确返回；
- Vim Mode 只拥有 `:` 交互，不拥有通用命令执行器；
- Mode callback、render path、Worker 和 crate 依赖边界没有回归。

## 10. 最终验证门槛

每个里程碑先运行受影响 crate 和 TypeScript contract 测试。M8 运行完整门槛：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
pnpm typecheck
cargo metadata --no-deps
cargo tree -p vell-app -e normal
```

最终还必须确认：

- `vell-app` 普通依赖不含 V8、Taffy 或 crossterm；
- `vell-plugin-v8` 公共 API 不泄漏 V8 类型；
- `vell-core` 不依赖 async runtime、Mode、Frontend 或终端；
- Mode callback 仍只返回 ordered typed operation；
- compiler bundle 可离线构建，并包含正确 license；
- `runtime/editor.d.ts`、Rust schema 和动态生成声明保持一致；
- Markdown 行宽、相对链接和 `git diff --check` 通过。
