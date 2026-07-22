# TypeScript 脚本

**状态：** 当前插件作者指南

**更新日期：** 2026-07-22

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
`TYPESCRIPT_DECLARATIONS` 的形式内嵌在 `vell-plugin-v8` 中。CI 会根据
该文件检查内建插件和迁移示例的类型。运行时会转译 TypeScript，但不会执行
类型检查。

Rust 测试和 headless 工具可以在不创建终端的情况下编译并加载源码字符串：

```rust
let loaded = vell_plugin_v8::load_typescript_modes(
    "file:///test.ts",
    source,
)?;
assert!(loaded.diagnostics.is_empty());
let modes = loaded.modes;
```

结果只暴露通用 `Mode` 对象和结构化诊断；V8 类型不会跨越 crate 边界。
`PLUGIN_API_VERSION` 标识当前 schema 版本。根二进制通过
`load_user_modes()` 合并内嵌插件与可选用户配置，然后再构建 App。

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
StatusBar view context 还提供
`targetViewId` 和 `targetContentId`。状态栏 Mode 可以在
`viewPolicy.statusBar` 中定义带可选 Face 的 `left`、`center` 和 `right`
分段。

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

Mode 可以定义具名 `faces`，并发布 `contentDecorations` 或
`viewDecorations`。每个 decoration snapshot 都携带 Content revision 和
UTF-16 range。渲染只读取缓存的 Rust snapshot，不会调用 V8。

文本变化时，缓存的 Content decoration 会先随该 change 映射，直到新的异步
snapshot 到达。这样既能避免高亮短暂消失，也能保持 revision 安全。

`viewState.viewPolicy` 可以设置 cursor style、cursor domain、selection
shape 和具名 selection face。

## 后台 worker

内建插件可以指定一个持久的 `worker.ts`：

```ts
editor.worker.onMessage(async (message) => {
  const bytes = editor.resources.readBinary("vendor/parser.wasm");
  return await analyze(bytes, message);
});
```

Worker 资源是只读的，并限制在插件目录中。宿主不提供绝对路径、父目录穿越、
网络访问、timer 或 Node API。

高级 Buffer adapter 可以独立于普通 command 和 input 声明具名后台 analysis：

```ts
analysis: {
  syntax: {
    worker: "worker.ts",
    snapshot: "text",
    input(ctx) {
      if (ctx.state.language === null) return;
      return { language: ctx.state.language, revision: ctx.revision };
    },
    apply(ctx) {
      return {
        contentDecorations: {
          revision: ctx.revision,
          spans: ctx.arguments.spans,
        },
      };
    },
  },
}
```

Analysis 名称是稳定的任务 identity。`input` 必须是纯函数；其返回值同时
也是依赖签名。宿主在发布任何 replacement 前轮询全部具名 analysis，分配
单调递增的 generation，并捕获 Content revision 和 input epoch。message
变化或返回 `void` 会取消已被取代的任务；过期结果不会进入 `apply`；
`apply` 在事务化 Mode state 上运行。当前 analysis 会接受自己在 apply
之后的新签名，避免 state 更新不断触发自身；其他 analysis 只在各自的
message 变化时重新运行。

`snapshot: "text"` 会在 UI 线程之外把当前文档文本加入 worker message；
`input` 必须返回不含 `text` 字段的对象。多个具名 analysis 各自维护独立
的 decoration cache layer。

Worker 可以返回 Promise。worker isolate 会驱动 V8 microtask、响应编辑器
取消，并拒绝超过执行预算的请求。主 ScriptHost 对 input 和 command callback
仍然同步执行，但 watchdog 会限制每次调用，并在超时或 heap pressure 时终止
V8。只有 invocation 成功后，经过验证的 state、operation 和 presentation
数据才会发布。

独立 command 仍有意保持为延后功能。目前没有 command palette 或非 Mode
调用入口，因此 `context.commands.invoke()` 只解析已注册的 Mode-local
限定 command，不维护第二套全局脚本 action 表。

## 迁移 v1 Mode

迁移窗口内，用户配置仍可使用 v1 `content/view/actions/keys` schema。
即使定义了多个 v1 Mode，一个已配置的 host 也只会产生一次弃用 warning。
parser 会把它们适配到与 v2 相同的已注册 Mode 和 execution frame。

普通 Buffer Mode 的迁移是机械性的：

- 将 `content.create` 移到 `on.buffer.state`；
- 将 `view.create` 移到 `on.buffer.viewState`；
- 将 `actions` 重命名为 `on.buffer.commands`，并把 `keys` 移到旁边；
- 将 `contentState` 重命名为 `state`，将 `text` 原语改为 `edit`；
- 用 `pass()` 替换 `forward()`，并删除 `handled()` 返回值。

内建 Vim 和 Tree-sitter 插件使用 v2，因此不会经过兼容 parser。
[已检查的迁移示例](../runtime/examples/v1-migration.ts) 同时由 TypeScript
编译器和 Rust host 测试执行。

v1 在 0.1.x 中已弃用，在 0.2.x 中仍可使用并产生一次结构化 warning，
在 0.3.0 中将被删除。删除 v1 前，已检查的迁移示例和所有内建插件必须继续
使用 v2。公开的 `V1_REMOVAL_VERSION` 常量和 contract test 会确保 warning、
声明与发布策略保持一致。

## 模块与信任边界

用户配置支持在配置目录内通过静态相对路径导入 `.ts` 和 `.js`。
bare package、URL、CommonJS、dynamic import、top-level await，以及越出
配置目录的 import 都会被拒绝。

内建 worker 脚本和二进制资源在构建时嵌入。当前还不支持来自文件系统的
用户 worker。

## Windows 构建说明

仓库固定了 rusty_v8 使用的 bindings，因此 Cargo registry 和 target 目录
位于不同磁盘时，也不要求 Windows symlink 权限。首次构建仍会下载
rusty_v8 的预编译静态库。
