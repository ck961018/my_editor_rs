# Worker 平台原语设计

- 状态:已实现
- 日期:2026-07-27
- 主题:把 vell 的线程能力从 Mode 的 `analysis` 子字段提升为
  标准 Web Worker 平台原语

## 背景与问题

vell 当前通往后台线程的门只有 `on.buffer.analysis`,而
`analysis` 是 Buffer content 的子概念。一个想跑 LSP、搜索索引、
git blame、格式化器的插件作者,只要他的任务不正好是
"按 buffer 变更重算装饰",就完全没有线程入口。StatusBar 插件、
theme 插件、纯后台索引器同理。

此外,当前 worker 接口与 Web Worker 标准差距很大:

- `editor.worker.onMessage` 是全局单点、单次 request→response、
  无 request 身份、无标准取消。
- worker.ts 走单文件 transpile,不能用 `import`/`export`,大型
  worker(如 tree-sitter 355 行)无法拆分复用。
- 构造面非标准,作者无法用迁移自浏览器/Deno 的现成
  worker.ts。

## 目标

- 任何插件、任何位置都能拉起后台线程,不再限于 Mode 的
  `analysis`。
- worker 通信面对齐 Web Worker 核心:`postMessage`、
  `addEventListener("message")`、`MessageEvent`;`AbortSignal` 是
  vell 的生命周期扩展。
- worker.ts 是 ES module,支持 `import`/`export`/`import type` 跨文件
  复用。
- 构造面用标准 `new Worker(url, { type: "module" })`。
- 删除 `analysis`、`editor.worker.onMessage` 等 Mode-bound 表面。
- analysis 的安全精华(revision 校验)抽成通用 revision-safe sink,
  不绑 Mode。

## 非目标

- 不实现 Transferable / SharedArrayBuffer / Atomics(星型通信,
  clone 够用)。
- 不实现 MessageChannel / MessagePort(worker 间经主转发)。
- 不实现 BroadcastChannel。
- 不实现 worker 池或调度器(等真实瓶颈再加)。
- 不实现 graceful shutdown 协议(编辑器场景不需要)。
- 不实现 URL scheme 路由(无 `https://`、`blob:`),只支持最终仍在
  插件根目录内的相对路径、绝对路径和 `file:` URL。
- 不保留 v1/v2 兼容层(无包袱,字段推倒重来)。

## 总体架构

三层分离:

1. **平台原语(新)** —— 标准 Web Worker,全局可用,worker 可嵌套
   spawn。
2. **revision-safe sink(新)** —— analysis 的 revision 校验抽成
   通用平台 sink,任何 worker 结果可调。
3. **Mode 回调(不变)** —— 仍在 `ExecutionFrame` 内写 draft,
   不再是进线程的唯一入口。

analysis 从"唯一线程门 + 安全层"降级为:安全层搬到平台 sink,
线程门由 `new Worker` 取代。`analysis` 字段删除。

核心解耦:通往线程的门从"只有 buffer Mode 的 analysis"变成
"全局 `new Worker`"。status bar 插件、theme 插件、纯后台索引器
都能直接拉线程,不再假装自己是 buffer 分析。

## 平台原语:标准 Web Worker 表面

### 作者侧 API(主线程)

```ts
const worker = new Worker(
  new URL("./parser.ts", import.meta.url),
  { type: "module" },
);

worker.postMessage({ text });
worker.addEventListener("message", (e: MessageEvent<Spans>) => {
  // e.data = worker 回推的结果
});

// vell 生命周期扩展
const ctrl = new AbortController();
const worker2 = new Worker(
  new URL("./parser.ts", import.meta.url),
  { type: "module", signal: ctrl.signal },
);
ctrl.abort();

worker.terminate();
```

构造参数必须是 vell `URL` 构造器产生的对象。裸字符串没有可靠的插件来源
identity，会同步抛 `TypeError`。

### worker 侧 API(worker.ts 全局)

```ts
import { parse } from "./parser.ts";
import type { Span } from "./types.ts";

self.onmessage = (e: MessageEvent<string>) => {
  self.postMessage(parse(e.data));
};

// 嵌套 spawn(标准)
const child = new Worker(
  new URL("./helper.ts", import.meta.url),
  { type: "module" },
);
```

worker.ts 是标准 `DedicatedWorkerGlobalScope`:`self.onmessage`、
`self.postMessage`、`self.close()`。

### vell 的诚实约束(标准之内的收敛)

1. URL 只实现相对/绝对路径,不实现 scheme 路由。
   `new URL("./parser.ts", import.meta.url)` 解析到插件目录内文件。
   跨插件、跨目录、父目录穿越仍被 `resolve_path` 拒绝
   (现有边界校验)。
2. 无 `importScripts`(legacy CommonJS),只做 module worker。
3. 无 timer / fetch / Node API,保持现有沙盒。
4. 结构化克隆用现有 `json_to_v8` / `v8_to_json` 路径,暂不上
   Transferable / SharedArrayBuffer。

### 为什么比当前设计好

| 维度 | 当前 `editor.worker.onMessage` | 新 `new Worker` |
| --- | --- | --- |
| 入口 | 仅 Mode.analysis | 全局任何位置 |
| 通信 | 单向单次 | 双向多次 |
| 多 handler | 全局单点 | 每实例独立 |
| 取消 | 自定义 token | 标准 AbortSignal |
| worker 模块形态 | 单文件脚本 | ES module |
| 构造语法 | 非标准 | 标准 `new Worker` |
| 嵌套 | 不支持 | 支持 |

## revision-safe sink

### 动机

analysis 之前帮作者做的 revision 校验有真价值,不该让每个插件
作者重造,也不该绑死在 Mode job。它成为 ScriptHost 级平台 sink。

### 作者侧

```ts
declare const editor: {
  writeDecorations(
    contentId: number,
    revision: number,
    spans: TextDecorationSpan[],
  ): void;
};
```

作者在主 isolate 的 Worker message callback 中调用
`editor.writeDecorations`,宿主内部:

- identity 校验 —— `contentId` 明确目标 Content,不会因两个 Buffer
  revision 相同而串写。
- revision 校验 —— sink 只安装仍匹配 live Content revision 的结果,
  过期结果丢弃。
- 单一发布层 —— decoration buffer 由 ScriptHost 共享,各 ScriptMode
  presentation 只读取一次,不会重复合并。

### 扩展性

具名 sink 是加法式扩展:

```ts
declare const editor: {
  writeDecorations(contentId, revision, spans): void;
  // 未来按需加(不现在做)
  // writeDiagnostics(revision, diags): void;
  // statusBar(segments): void;
};
```

现在只实现 `writeDecorations`(tree-sitter 真实需求),其余等真有
插件需要再加。

### worker.ts 不感知 sink

worker 作者写标准 Worker,回 `postMessage`,安全层在主线程 sink
里。worker 不知道 Content revision 校验存在。

### 取消与过期结果

常驻任务只需创建一个 Worker,每次变化发送新 snapshot。旧任务即使晚返回,
revision-safe sink 也会丢弃结果:

```ts
const worker = new Worker(
  new URL("./parser.ts", import.meta.url),
  { type: "module" },
);
worker.onmessage = (event) => {
  const { contentId, revision, spans } = event.data;
  editor.writeDecorations(contentId, revision, spans);
};
```

需要结束整个 Worker 生命周期时,作者可调用 `terminate()` 或通过
`AbortController` 取消。平台不再提供 analysis generation 或自动 replacement。

## 嵌套 spawn、生命周期与错误模型

### 嵌套 spawn

worker 内部 `new Worker(url, { type: "module" })` 标准。vell 实现:
worker isolate 的全局也挂 `Worker` 构造器,spawn 时再起 OS 线程 +
新 isolate。

### 防爆炸:硬上限

| 限制 | 值 | 理由 |
| --- | --- | --- |
| 每插件总 worker 数 | 8 | 单插件典型 1-2,8 是 4x 垫 |
| 全局总 worker 数 | 32 | TUI 编辑器 32 条后台线程是 4x 估 |
| 嵌套深度 | 4 | 主→A→B→C→D 够所有真实场景 |
| 每 worker heap | 沿用 `SCRIPT_HEAP_LIMIT_BYTES` | 现有 |
| 每 worker 执行预算 | 沿用 `WORKER_TIMEOUT`(30s/请求) | 现有 |

超限 spawn 抛 `QuotaExceededError`(标准 DOMException),作者可
try/catch。

### 生命周期

| 事件 | 标准 | vell 实现 |
| --- | --- | --- |
| 构造 | `new Worker` 返回 | 校验后返回；后台线程加载 module |
| `worker.terminate()` | 主线程强杀 | 取消 token、移除句柄,线程退出后释放 isolate |
| `self.close()` | worker 自杀 | 主线程 pump 回收句柄,线程退出 |
| `AbortSignal` abort | vell 扩展 | 取消整个 Worker 生命周期 |
| 主线程退出 | n/a | 所有 worker 随进程退出,不做 graceful shutdown |

### 错误模型

worker 内未捕获异常:worker 触发 `error` 事件,主线程
`addEventListener("error")` 收到 `ErrorEvent`。异常不跨 isolate
传播,worker 死,主线程不死。worker 死后再 postMessage 抛
`InvalidStateError`。

超预算/heap:沿用现有 watchdog。超 `WORKER_TIMEOUT` 终止 isolate,
主线程收 `error` 事件(name=`TimeoutError`)。heap 超
`SCRIPT_HEAP_LIMIT_BYTES` 终止 isolate,主线程收 `error` 事件
(name=`ResourceExhausted`)。

构造器同步校验 URL、module-only options 和配额。Worker module 的
transpile、加载或 module graph 错误在线程启动后通过异步 `error` 事件报告,
与标准 Web Worker 的构造时序一致。

### 错误清单

| 场景 | 错误 | 标准性 |
| --- | --- | --- |
| 超线程配额 | `QuotaExceededError` | 标准 DOMException |
| 路径越界 | `TypeError` | 标准 |
| transpile/module 加载失败 | 异步 `error` 事件 | 标准 |
| worker 未捕获异常 | `error` 事件 | 标准 |
| 超时/heap | `error` 事件(name 指明) | vell 扩展(标准框架内) |
| 给 terminated worker postMessage | `InvalidStateError` | 标准 |

## 迁移影响

### 内建插件

只 tree-sitter 用 worker(`runtime/plugins/tree-sitter/`)。迁移后:

- `plugin.ts` 的 `analysis` 声明改为单个常驻 `new Worker` +
  `editor.writeDecorations`；revision sink 丢弃过期结果。
- `worker.ts` 的 `editor.worker.onMessage` 改为标准 `self.onmessage`。
- `worker.ts` 可拆成 `parser.ts`/`query.ts`/`worker.ts` 多文件
  (现在 355 行单文件)。

迁移代价:tree-sitter 作者显式发送 snapshot 和调用 sink。失去 analysis
的"自动轮询 input 签名",但
tree-sitter 的 `input`
逻辑(`if language === null return; return {...}`)搬到
`on.buffer.changed` 回调,等价。

### 删除的表面

- `analysis` 字段(Mode 定义里的)。
- `editor.worker.onMessage` 全局单点。
- `BackgroundAnalysisDefinition` / `BackgroundAnalysisInputContext` /
  `BackgroundAnalysisApplyContext` 整套类型。
- v1 `content.job` / `content.applyJob` / `worker`。
- `WorkerResponse` 泛型参数(类型从 worker 的 postMessage 推导)。

### 保留搬进平台的逻辑

- revision 校验(analysis 内部做的)→ 搬进
  `editor.writeDecorations` 实现。
- owned decoration 安装 → ScriptHost 共享 revision-gated buffer。

### 不保留的逻辑

- generation 单调递增 / 自动取消被取代任务。常驻 Worker 可依赖 sink
  丢弃 stale revision；需要结束 Worker 时使用 `AbortController`。

## 测试策略

### Rust 侧(`crates/vell-plugin-v8`)

现有测试
`worker.rs:381 worker_loads_embedded_resources_and_resolves_async_response`
要改(测的是旧 `editor.worker.onMessage` 单次模型)。新增:

| 测试 | 覆盖 |
| --- | --- |
| `worker_post_message_receives_in_worker` | 主→worker postMessage 到达 self.onmessage |
| `worker_post_message_receives_in_main` | worker→主 postMessage 到达 addEventListener("message") |
| `worker_aborts_on_signal` | AbortController.abort() 中断 worker |
| `worker_terminate_kills_isolate` | worker.terminate() 释放线程+isolate |
| `worker_nested_spawn_child` | worker 内 new Worker 起 child |
| `worker_quota_exceeded` | 超 8/32/4 限抛 QuotaExceededError |
| `worker_error_event_on_uncaught` | worker 抛异常→主收 error 事件 |
| `worker_timeout_error` | 超 30s→终止+error 事件 |
| `worker_heap_limit` | 超 heap→终止+error 事件 |
| `worker_es_module_imports` | worker.ts import ./parser.ts 多文件 |
| `worker_import_meta_url` | import.meta.url 返回正确 URL |
| `worker_dynamic_import` | `await import("./x.ts")` 解析 |
| `write_decorations_frame_safe` | sink 校验 Content identity 与 revision |
| `write_decorations_stale_revision` | 过期 revision 的 spans 被丢弃 |

### TypeScript 侧(`runtime/`)

`pnpm typecheck` 必过。新增类型契约测试(参考现有
`runtime/type-tests/v2-mode.ts`):

- `new Worker(new URL(..., import.meta.url))` 类型正确。
- `Worker` 构造器 options 类型(`type: "module"`, `signal`)。
- `editor.writeDecorations(contentId, revision, spans)` 签名。
- `AbortController` / `AbortSignal` 全局可用(lib 已含)。

### 迁移示例(`runtime/examples/`)

tree-sitter 迁移版作为 `runtime/examples/worker-platform.ts` 留作
参考,受 TS + Rust 测试双覆盖(AGENTS.md 要求迁移示例双覆盖)。

## 实现基础(已验证)

- vell 已用 `deno_ast::ModuleSpecifier`(URL 类型)做 module 身份,
  `v8::ScriptOrigin` 已挂 origin —— 暴露 `import.meta.url` 只需接
  `HostInitializeImportMetaObjectCallback`。
- `crates/vell-plugin-v8/src/script/module.rs` 已有完整 ES module
  解析(`load_module_tree` + `resolve_path` + 目录边界校验 + 缓存),
  worker 现在没用它只是因为走单文件
  `transpile_typescript`。改调用即可,rust 侧几乎零新增。
- `HostImportModuleDynamicallyCallback` 接现有 `resolve_module`,
  支持标准动态 `import()`。
- `worker.rs` 现有机械(std::thread + isolate + mpsc + watchdog)
  基本复用,主要改 mpsc 从单次扩为可多次双向。

## 实现顺序(粗,细化留给 writing-plans)

1. Rust:全局挂 `Worker` 构造器 + mpsc 双向化(`worker.rs`)。
2. Rust:`import.meta.url` 注入 + 动态 `import()` 回调
   (`module.rs` + host)。
3. Rust:配额表 + 错误事件分发。
4. Rust:`editor.writeDecorations` sink(从 analysis 搬安全层)。
5. TS:`editor.d.ts` 删 analysis/editor.worker,加 `Worker`/
   `AbortController`/`writeDecorations`。
6. 迁移 tree-sitter 插件。
7. 测试全过。
