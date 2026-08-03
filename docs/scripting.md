# TypeScript 脚本

**状态：** 当前插件作者指南

**更新日期：** 2026-08-02

宿主架构与信任边界见
[`TypeScript 脚本架构`](design/typescript-scripting-architecture.md)。

编辑器在创建初始 Content 和 View 之前，按照 manifest 中的 `order`
加载 `runtime/plugins/*/plugin.json` 指定的内建插件。Rust 只注册由此得到的
通用 Mode 定义；它不会按名称选择插件，也不实现插件行为。

内建插件加载完成后，编辑器可以再加载一个可选的用户 `config.ts`。可以通过
`VELL_CONFIG` 显式指定文件，也可以使用各平台的默认路径：

- Windows：`%APPDATA%\vell\config.ts`
- Linux 和 macOS：`$XDG_CONFIG_HOME/vell/config.ts`
- 主目录 fallback：`$HOME/.config/vell/config.ts`

内建插件加载失败会阻止启动，因为这意味着编辑器安装不完整。可选用户配置
加载失败只会报告 warning；该配置产生的部分定义会被回滚，编辑器继续使用
内建 Mode。

编辑器和 TypeScript 工具应使用
[editor.d.ts](../runtime/editor.d.ts)。它是公开 schema 的唯一真相源，并以
`TYPESCRIPT_DECLARATIONS` 的形式内嵌在 `vell-plugin-v8` 中；
[commands.generated.d.ts](../runtime/commands.generated.d.ts) 是描述原生
命令签名的伴随文件。CI 会根据这两份声明检查内建插件和迁移示例的类型。

执行路径只转译 TypeScript，不做类型检查。类型检查由独立的命令类型环境提供，
它只服务于命令签名推导和声明发布，见[动态类型](#动态类型)。

Rust 测试和 headless 工具可以在不创建终端的情况下编译并加载源码字符串：

```rust
let loaded = vell_plugin_v8::load_typescript_modes(
    "file:///test.ts",
    source,
)?;
let modes = loaded.modes;
let backgrounds = loaded.backgrounds;
```

结果只暴露通用 `Mode`、`ModeBackground` 和 `CommandEntry`；
V8 类型不会跨越 crate 边界。
根二进制通过 `load_user_configuration()` 原子取得 Mode、后台运行时、Theme
和 Face override，再用 `prepare_commands()` 安装原生命令视图并取回命令，
最后构建 App。内建配置的测试或 headless 入口可使用
`load_default_configuration()`。

## 选择 Theme 与覆盖 Face

用户配置可以选择内建 Theme，并按属性覆盖 named Face：

```ts
editor.theme.use("catppuccin-mocha");

editor.faces.override("syntax.comment", {
  italic: false,
});

editor.faces.override(
  "ui.editor",
  { foreground: { reset: true } },
  { theme: "catppuccin-latte" },
);
```

不带 `theme` option 的覆盖作用于所有 Theme；带 option 的覆盖只在对应 Theme
活动时生效。覆盖按属性合成，`false` 是显式值，`{ reset: true }` 恢复当前
presentation root 的对应属性。命令行 `--theme` 优先于 config 选择。

Theme 选择、Face override 和 Mode 定义属于同一个启动 draft。模块执行失败
时三者一起回滚，不会发布部分配置。

## 注册命令

普通函数定义不会自动成为命令。显式注册后，函数才获得稳定的 `CommandId`，
并进入 `editor.commands` 命名空间：

```ts
function increment(value: number): number {
  return value + 1;
}

async function saveAndReport(): Promise<void> {
  await save();
}

editor.commands.register(increment);
editor.commands.register("math.increment", increment);
editor.commands.register("save", saveAndReport);
```

`register(namedFunction)` 使用函数名作为 ID，`register(id, callback)` 使用
显式 ID。ID 由一个或多个点分 TypeScript 标识符组成，并按点构造命名空间：

```ts
editor.commands.math.increment(1);
```

`register` 返回传入的 callable，因此赋值时仍保留完整推断。重复 ID 会替换
当前实现，包括替换 Rust 原生命令；已绑定该 ID 的按键随之调用新实现。

宿主只在裸名称当前没有被占用时，为它安装 global fallback，因此词法绑定和
用户自己的 global 始终优先：

```ts
save();                  // 没有同名绑定时解析为正式命令
editor.commands.save();  // 始终是正式命令
```

Rust 原生命令与 TypeScript 命令有完全相同的调用体验。当前原生命令的签名见
[commands.generated.d.ts](../runtime/commands.generated.d.ts)。其中
`invokeMode("mode.command", arguments?)` 是命令进入 Mode command 的唯一
入口。

## 快捷命令与 `:`

快捷命令是文本入口，最终指向正式命令：

```ts
editor.commands.shortcut("q", () => quit());

editor.commands.shortcut("wq", async () => {
  await save();
  quit();
});
```

handler 只接收一个原始可选参数，类型为 `string | undefined`。分派器移除
名称与参数之间的分隔空白：没有非空白参数时以零个参数调用，否则把剩余文本
作为一个字符串传入。宿主不拆分引号、flag、range 或位置参数。

传入未注册的回调时，宿主为它创建私有命令 identity。私有 identity 不进入
公开命令补全，但仍使用正常的命令预算、错误和 execution frame。

`:` 支持三条路径：

```text
:save()                 单个正式命令调用
:wq                     注册快捷命令
:ts const value = 1     普通 TypeScript
```

函数形式必须是一条以 `editor.commands` 中命令为根的调用表达式；参数本身
仍是普通 TypeScript 表达式，可以调用普通函数和嵌套命令：

```text
:switchBuffer(newBuffer())
:math.increment(41)
```

顶层声明或多条语句会被拒绝，并提示改用 `:ts`。`:` 的输入状态和按键处理
属于 Vim 插件；解析与执行属于通用命令服务，Rust 层没有 Ex parser。

## TypeScript 求值

`:ts <source>` 求值参数。`:ts` 不带参数时求值当前 buffer：非空 selection
优先，否则执行光标所在的完整顶层语句或声明。多行声明按语法节点整体执行；
无法得到完整节点时报告语法错误，不猜测物理行。

交互求值、buffer 求值和直接运行的 TypeScript 文件共享一个持久 global
script environment，普通函数、变量和闭包按 JavaScript global script 语义
跨执行保留。插件与 `config.ts` 继续使用 ES module 作用域，module-local
绑定不会泄漏到该环境。

持久 script 不支持 top-level `await`，静态 `import` 只属于 module 路径；
script 使用动态 `import()`。异步入口写成普通函数：

```ts
async function main(): Promise<void> {
  await save();
}

main();
```

## 命令的结果、异常与异步

同步命令立即返回真实结果。嵌套命令共享最外层 execution frame，因此后续
调用可以观察前序修改：

```ts
const id = newBuffer();
switchBuffer(id);
```

未捕获异常回滚该同步段的 Content、View、input、history、Mode draft 和
prepared effect。以下状态不参与宿主回滚：JavaScript heap、global 绑定、
闭包状态、命令与快捷命令注册，以及已发布的类型声明。

真正异步的命令返回标准 `Promise<T>`。每个实际 `await` 提交当前 frame，
恢复 continuation 时创建新 frame，并继续绑定命令启动时的 View 与 Content。
等待期间切换焦点不会改变目标；目标关闭或 revision 失效时 Promise reject。

`save()` 在实际保存完成时 resolve，在冲突或 IO 失败时 reject。组合保存与
退出必须显式排序：

```ts
async function writeQuit(): Promise<void> {
  await save();
  quit();
}
```

外部入口自动等待最终返回的 Promise。脚本既没有返回、也没有 `await` 的
Promise 不会被追踪或自动等待。

交互入口保留最终结果，但当前没有通用消息区，因此不显示返回值；键位调用
同样忽略返回值。异常继续通过现有诊断和状态栏路径呈现。

## 动态类型

注册成功后，宿主使用内嵌的官方 TypeScript compiler 推导 handler 的参数、
返回值和 Promise 类型，并更新虚拟 `commands.generated.d.ts`。下一次交互
输入、buffer 求值或脚本立即看到该命令的真实类型；重定义会原子替换旧声明。

compiler 运行在独立 isolate 中，用户脚本无法访问它。它只提供类型数据，
不拥有运行时注册表；实际成功的注册才会发布声明。无法静态定位的动态注册
退化为安全的 `unknown` 签名，不伪造具体类型。compiler 出错时只禁用类型
服务，执行与命令注册继续工作。

compiler bundle 随源码管理，构建不调用网络、Node 或 pnpm。来源、license
与升级流程见 [vendored TypeScript compiler][bundle]。

[bundle]: ../crates/vell-plugin-v8/vendor/typescript/README.md

## 定义 Mode

```ts
editor.modes.define({
  name: "pairs",
  on: {
    buffer: {
      state: () => ({ inserted: 0 }),
      viewState: () => ({ enabled: true }),
      commands: {
        quote(context) {
          if (!context.viewState.enabled) return context.pass();
          context.state.inserted++;
          context.edit.insert('""');
          context.cursor.moveLeft();
        },
      },
      keys: { '"': "quote" },
    },
  },
});
```

每个 `(Mode, Content)` 只有一份 Content state。每个 `(Mode, View)`
只有一份 View state。两者都只能包含与 JSON 兼容的结构化数据。callback
返回后，宿主会复制经过验证的值。

Mode 按 attachment 顺序接收输入。command 正常返回表示已经处理该输入。
只有 `return context.pass()` 才会在当前 operation 执行后继续传递给下一个
Mode。可选的 `input(context)` callback 会在 `context.arguments` 中以
类型化 `EditorKeyEvent` 接收每个未映射的原始按键；只有简单 keymap 的
Mode 不需要该 callback。

Command 使用稳定的限定名称，例如 `pairs.quote`。其他 command 可以调用
`context.commands.invoke("pairs.quote")` 暂存该 command。嵌套 command
与当前 command 共享 transaction，但其返回值不会替换调用方的
`void | Pass` 决策。

Mode command 与 [`editor.commands`](#注册命令) 中的正式命令是两套命名空间，
不会互相合并。正式命令通过原生 `invokeMode("pairs.quote")` 进入 Mode
command；宿主在当前同步段结束后按顺序执行它，因此 Mode callback 不会在
命令执行途中重入。

## 原生原语

Rust 在 `context.cursor`、`context.edit`、`context.history`、
`context.viewport`、`context.commands` 和 `context.app` 下暴露
类型化函数。脚本直接调用这些函数；operation 名称不会序列化为字符串。
动态 Mode 和 action 名称仍使用字符串，因为这些命名空间由插件定义。

Viewport 原语包括按 pane 大小滚动和 cursor 对齐。`alignTop()`、
`alignCenter()` 和 `alignBottom()` 会变成延迟执行的 viewport effect；
它们不会移动文本 cursor。

Buffer context 通过 `resourceName`、`resourcePath`、`backingState`、
`dirty`、`saveState` 和 `textMetrics` 暴露彼此独立的只读事实。
状态栏呈现不经过独立 context：buffer mode 在
`viewPolicy.statusBar` 中定义带可选 Face 的 `left`、`center` 和 `right`
分段，app 把目标 buffer view 的 policy 组装为状态栏呈现。

`context.app` 除保存和退出外，还提供 `closePane()`、
`splitHorizontal()`、`splitVertical()` 与四个 `focus*()` 原语。pane close、
split 和 focus 与 viewport 一样，只在整个 execution frame 成功后发布。
`closePane()` 关闭当前 pane；关闭最后一个可聚焦 pane 时退出应用。
每个 execution frame 最多产生一个 split、close 或 focus；topology 原语
不能与 viewport 原语在同一 frame 中混用。nested command 和 callback 也
属于调用方的 frame；违反约束时整个 frame 回滚。

原语调用会把类型化 Rust operation 追加到当前 callback。只有 callback
及其返回状态通过验证后，App 才会按顺序执行这些 operation。如果 callback
失败，已暂存的 operation 都不会执行。callback 结束后，之前保留的 context
不能再调用原语。

例如：

```ts
context.history.begin();
context.cursor.moveWordForward(2);
context.edit.deleteToLineEnd();
context.history.commit();
```

## 编辑 Content

`context.edit.insert()` 和相对于 cursor 的文本函数使用现有的延迟编辑路径。
绝对位置的 edit batch 使用从零开始的 UTF-16 坐标：

```ts
context.edit.applyEdits([{
  range: {
    start: { line: 0, character: 1 },
    end: { line: 0, character: 3 },
  },
  text: "replacement",
}]);
```

该 batch 绑定到当前 callback 捕获的 Content snapshot。adapter 会拒绝
相互重叠的 range、位于 surrogate pair 中间的位置，以及超出该 snapshot
的 batch。selection 协调、history、undo 和 rollback 仍由 App executor
统一负责。

## Face 与 decoration

Mode 可以定义插件私有 `faces`，包括显式继承与无 Theme 时的 fallback：

```ts
faces: {
  "plugin.todo.warning": {
    inherits: ["diagnostic.warning"],
    fallback: { bold: true, underlineStyle: "curl" },
  },
},
```

command callback 可以产生 Session、Content 或 View scope 的局部 remap：

```ts
const token = context.faces.addRelative(
  "plugin.todo.warning",
  ["diagnostic.warning", { dim: true }],
  "view",
);
context.viewState.warningFaceToken = token;

// 后续 callback：
context.faces.removeRelative(context.viewState.warningFaceToken);
```

`addRelative` 返回的 token 只允许所属 Mode 删除。callback 或 execution
frame 失败时 remap 不会发布；View 关闭或 Mode detach 时宿主自动清理。

Mode 通过 `contentDecorations` 或 `viewDecorations` 发布 FaceName。每个
decoration snapshot 都携带 Content revision 和 UTF-16 range。渲染只读取
缓存的 Rust snapshot，不会调用 V8。

文本变化时，缓存的 Content decoration 会先随该 change 映射，直到新的异步
snapshot 到达。这样既能避免高亮短暂消失，也能保持 revision 安全。

`viewState.viewPolicy` 可以设置 cursor style、cursor domain、selection
shape 和具名 selection face。

## 后台 Worker

插件可以在顶层创建 module Worker。Worker 不属于 Mode，也不需要
`analysis` 声明。构造参数必须是 `new URL(...)` 产生的 URL 对象；vell 不接受
缺少插件来源 identity 的裸字符串：

```ts
const worker = new Worker(
  new URL("./worker.ts", import.meta.url),
  { type: "module" },
);

worker.onmessage = (event: MessageEvent<HighlightResult>) => {
  const { contentId, revision, spans } = event.data;
  editor.writeDecorations(contentId, revision, spans);
};
```

主线程和 Worker 通过 `postMessage`、`onmessage` 或
`addEventListener("message", ...)` 交换 JSON-compatible owned data。
Worker 入口是 ES module，可使用静态或动态相对 import：

```ts
self.onmessage = async (event: MessageEvent<HighlightRequest>) => {
  const parser = await import("./parser.ts");
  self.postMessage(await parser.highlight(event.data));
};
```

`editor.writeDecorations(contentId, revision, spans)` 是 revision-safe sink。
宿主只安装仍与目标 Content 当前 revision 匹配的结果；过期 Worker 结果会被
丢弃。Worker 只计算并 `postMessage`，sink 必须在主 isolate 的消息回调中调用。

需要取消时，可把 `AbortSignal` 传给构造器，或调用
`worker.terminate()`。`self.close()` 从 Worker 内结束自身。Worker 内也可
创建 Worker；硬限制为每插件 8 个、全局 32 个、最多 4 层 Worker。

Worker 资源只读，并限制在插件根目录。相对路径、绝对路径和 `file:` URL
最终都必须位于该根目录；父目录穿越会被拒绝。宿主不提供网络、timer、DOM、
Node API、共享内存或 worker-to-worker 通道。

Worker 模块加载或语法错误通过异步 `error` 事件报告。URL/options 校验和配额
错误可由构造器同步抛出。未捕获异常、超时或 heap exhaustion 会终止对应
Worker 并产生 `ErrorEvent`，不会终止主 isolate。

## 模块与信任边界

用户配置支持在配置目录内通过静态或动态相对路径导入 `.ts` 和 `.js`。
bare package、远程 URL、CommonJS、top-level await，以及越出配置目录的
import 都会被拒绝。

内建 Worker 脚本和二进制资源在构建时嵌入。文件系统用户配置也可以从自己的
配置目录创建 Worker；入口及其 module graph 不能越出该目录。

## Windows 构建说明

仓库固定了 rusty_v8 使用的 bindings，因此 Cargo registry 和 target 目录
位于不同磁盘时，也不要求 Windows symlink 权限。首次构建仍会下载
rusty_v8 的预编译静态库。
