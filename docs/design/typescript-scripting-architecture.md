# TypeScript 脚本架构

**状态：** 当前实现

**更新日期：** 2026-07-22

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

`vell-app` 的普通依赖不含 V8。根二进制先调用
`vell_plugin_v8::load_user_modes()`，再把 `Vec<Box<dyn Mode>>` 注入
`App::with_modes`。V8 类型不跨出 `vell-plugin-v8` 的公共边界。

## 2. 加载与所有权

构建脚本把 `runtime/plugins/` 的清单、TypeScript、worker 和资源嵌入
`vell-plugin-v8`。启动时：

1. 枚举内嵌 `plugin.json`；
2. 按 manifest `order` 稳定加载入口；
3. 在同一 `ScriptHost` 中收集 Mode definition；
4. 加载可选用户 `config.ts`；
5. 把每个 definition 包装为 `ScriptMode`；
6. 将通用 Mode 交给 App bootstrap。

所有 `ScriptMode` 通过 `Rc<RefCell<ScriptHost>>` 共享主 isolate、context、
module map、callback registry 和 diagnostics。Mode definition 进入
`ModeRegistry` 后，host 的生命周期由这些 adapter 保持；App 不直接保存或
识别 ScriptHost。

内建插件失败表示安装损坏，会阻止启动。可选用户配置失败会输出 warning，
回滚该模块新增的 definition，并继续使用内建 Mode。

## 3. 配置发现

用户配置只从以下位置加载：

- `VELL_CONFIG` 指定的文件；
- Windows：`%APPDATA%\vell\config.ts`；
- Linux/macOS：`$XDG_CONFIG_HOME/vell/config.ts`；
- fallback：`$HOME/.config/vell/config.ts`。

编辑器不会自动执行当前工作目录或所打开项目中的脚本。

用户配置支持 `.ts`、`.js` 与配置目录内的静态相对 import。以下能力被
拒绝：

- URL 与裸 package specifier；
- CommonJS `require`；
- dynamic import 和 top-level await；
- 越出配置根目录的路径；
- Node、Deno、网络、timer、子进程和任意异步文件 API。

## 4. 公开 schema

`runtime/editor.d.ts` 是公开 TypeScript schema 的唯一真相源，并通过
`TYPESCRIPT_DECLARATIONS` 内嵌到 Rust API。CI 对声明、内建插件和迁移示例
运行严格类型检查。

`PLUGIN_API_VERSION` 当前为 2。v2 使用 ContentKind adapter：

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

Buffer 与 StatusBar adapter 获得不同的静态 context。Buffer context 暴露
资源名、路径、载体状态、脏状态、保存结果和文本统计；StatusBar view
context 还暴露
目标 View 与 Content ID，并可通过 `viewPolicy.statusBar` 定制左、中、右分段
及 Face。StatusBar 不暴露 cursor、text edit 或 background analysis。

v1 `content/view/actions/keys` schema 只作为兼容 parser 存在。每个 host 最多
产生一条结构化弃用诊断；`V1_REMOVAL_VERSION` 为 `0.3.0`。兼容层不会改变
Rust Mode contract。

## 5. ScriptMode 与状态

脚本定义中的 callback 保存在 V8 callback registry。`ScriptMode` 只保存稳定
callback identity、静态 keymap/adapter 元数据和共享 host，不保存可变 App、
Content 或 View 引用。

正式脚本状态只有：

```text
state:     每 (ModeId, ContentId) 一份
viewState: 每 (ModeId, ViewId) 一份
```

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

v2 command 正常返回 `void` 表示 Stop；只有 `return ctx.pass()` 继续到
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

## 8. Presentation

脚本可以定义 named Face、content decoration、view decoration 和 View policy。
callback 返回的数据转换为 owned Rust presentation layer。

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

## 9. 命名后台 analysis

Buffer adapter 可以声明多个命名 analysis：

```ts
analysis: {
  syntax: {
    worker: "worker.ts",
    snapshot: "text",
    input(ctx) {
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

analysis 名称映射为宿主内部 job slot。`input` 是纯函数，其返回 message
同时是依赖签名。`snapshot: "text"` 让宿主在线程边界把稳定文本快照加入
message；普通 UI callback 不复制全文到 V8。

宿主为请求分配单调 generation，并捕获 Content revision 与 input epoch：

- 同 slot 的新 message 或 `void` 取消旧请求；
- 一次 poll 先计算所有 slot 的签名，再发布替换；
- stale revision、epoch、message 或 generation 不进入 `apply`；
- `apply` 使用短生命周期 Mode draft；
- 当前 slot 接受 post-apply signature，避免自身 state 形成反馈循环；
- 不同 slot 的结果和 decoration cache 彼此隔离。

Worker 使用独立 isolate 和线程，只能读取嵌入插件目录的只读资源。它没有网络、
timer、Node API 或任意文件访问。Promise 通过受控 microtask pump 完成，并响应
cancellation 与执行预算。

## 10. 预算与恢复

当前默认限制：

- 普通 callback 2 秒，module startup 5 秒；
- worker request 30 秒；
- isolate heap 128 MiB，另保留 16 MiB 终止恢复余量；
- 单个脚本或 module 4 MiB，module graph 16 MiB；
- 普通 JSON state/result 4 MiB，结构化输入 32 MiB；
- 单 callback 最多 255 个 operation；
- 单次 presentation 最多 100,000 个 decoration。

主 isolate 的 watchdog 在线程中持有 `IsolateHandle`。超时或 heap pressure
触发 V8 termination；termination 传播出 scope 后，RAII 清理 watchdog，
恢复 terminate 状态与 heap limit。只有 runtime 可安全恢复时才继续调用。

所有大小、超时、转换和 presentation 检查都发生在发布 state、operation 或
cache 之前。

## 11. 故障隔离

主动 input/command callback 错误映射为 Mode fault，并使当前 execution frame
失败。App 恢复 Content、View、input 与 history checkpoint，丢弃 operation 和
Mode draft，但事件循环继续。

被动 content-change、presentation、state factory 或 analysis apply 失败时，只
fault 对应 attachment。基础文本编辑、其他 Mode 与渲染继续工作；诊断包含
Mode、callback phase 和 message。

主 isolate 由全部 ScriptMode 共享，因此这些限制不是恶意代码的进程级隔离。
在需要自动运行不受信任插件前，必须重新评估 isolate 或进程边界。

## 12. 物理模块

```text
vell-plugin-v8::script
├── mod          façade、加载、共享运行时类型
├── host         isolate、context、definition 与 callback registry
├── invocation   调用、microtask、watchdog 与 heap 恢复
├── mode_adapter ScriptMode、状态与后台 job 接线
├── module       本地 ES module graph 与 TypeScript 转译
├── bridge       Rust、JSON 与 V8 值转换
├── schema       v1/v2 definition 解析
├── primitives   callback-scoped native function
└── worker       后台 isolate、资源、取消与 Promise
```

依赖方向保持：

```text
vell-plugin-v8 -> vell-mode + vell-core + vell-protocol
vell-app       -X-> vell-plugin-v8
vell-tui       -X-> vell-plugin-v8
```

脚本作者使用的 API 见 [`docs/scripting.md`](../scripting.md)。
