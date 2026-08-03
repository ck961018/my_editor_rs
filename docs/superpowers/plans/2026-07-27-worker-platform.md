# Worker 平台原语 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 vell 的线程能力从 Mode 的 `analysis` 子字段提升为
标准 Web Worker 平台原语,任何插件、任何位置可用。

**Architecture:** 三层分离——全局标准 `new Worker` 平台原语、
通用 frame-safe `editor.writeDecorations` sink、Mode 回调不变。
删除 `analysis`、`editor.worker.onMessage` 等 Mode-bound 表面。
worker.ts 是 ES module,支持 `import`/`export`。支持嵌套 spawn,
配额上限防爆。

**Tech Stack:** Rust 2024(rusty_v8、deno_ast、tokio-util)、
TypeScript(`runtime/editor.d.ts`)、V8 isolate + std::thread。

## Global Constraints

- Rust 2024,MSRV 1.88。
- 内部 crate 依赖单向:
  vell-plugin-v8 -> vell-mode + vell-core + vell-protocol。
- vell-plugin-v8 不向公共接口泄漏 V8 类型。
- `runtime/editor.d.ts` 与 Rust schema 同步,改任一侧补契约测试。
- Markdown 所有行 ≤ 80 字符,LF。
- 跨 crate API 或执行边界改动默认跑完整测试 + Clippy。
- 测试命令:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace --all-features`
  - `pnpm typecheck`
- 修复 bug 时先加失败的回归测试,再改实现。
- 完成时说明实际运行的命令。

参考 spec:
`docs/superpowers/specs/2026-07-27-worker-platform-design.md`

---

## File Structure

### Rust 侧(`crates/vell-plugin-v8/src/script/`)

- `worker.rs` — **大改**。从单次 request→response 扩为可多次双向
  mpsc。删除 `editor.worker.onMessage` 安装,改挂全局 `Worker`
  构造器。删除 `worker_on_message`/`ScriptWorker::request` 单发
  接口,新增 `WorkerHandle`(postMessage/onmessage/terminate/
  AbortSignal)。保留 `resolve_asset_path`/`asset`/资源读。
- `module.rs` — **中改**。新增 `HostInitializeImportMetaObjectCallback`
  注入 `import.meta.url`。worker 的 transpile 从单文件
  `transpile_typescript` 改调 `load_module_tree`(已有)。
  新增 `HostImportModuleDynamicallyCallback` 支持动态 `import()`。
- `host.rs` — **小改**。主 isolate 的 `install_editor_api` 旁挂
  全局 `Worker` 构造器(让主线程能 `new Worker`)。
  `install_editor_api` 里加 `editor.writeDecorations` sink。
- `schema.rs` — **删改**。删除 `parse_analyses`/`parse_worker`
  (worker 字段)。`install_editor_api` 里删除 analysis 相关安装。
  Mode schema 里删 `analysis` 字段与 `BackgroundAnalysisDefinition`
  等类型。
- `mod.rs` — **小改**。删除 `ScriptAnalysisDefinition`、
  `ContentJob`/`applyJob` 相关。配额表常量定义在此。
- `mode_adapter.rs` — **小改**。删除 analysis 调度逻辑(input 轮询/
  generation/apply)。Mode 不再管 worker。

### 新增 Rust 文件

- `crates/vell-plugin-v8/src/script/worker_quota.rs` — **新增**。
  配额计数(`AtomicUsize`),每插件/全局/深度三限。

### TypeScript 侧(`runtime/`)

- `runtime/editor.d.ts` — **大改**。删除 `analysis` 字段、
  `BackgroundAnalysis*` 类型、`editor.worker.onMessage`、
  `ContentJob`/`applyJob`/`worker`、`WorkerResponse` 泛型。
  新增全局 `Worker` 构造器类型(标准 DOM lib 已含,只需声明
  `declare const Worker: ...` 或引用 lib)、`editor.writeDecorations`
  sink 签名。
- `runtime/plugins/tree-sitter/plugin.ts` — **迁移**。analysis 声明
  改为显式 `new Worker` + `AbortController` + `editor.writeDecorations`。
- `runtime/plugins/tree-sitter/worker.ts` — **迁移**。
  `editor.worker.onMessage` → `self.onmessage`。可选拆多文件。
- `runtime/type-tests/mode.ts` — **补**。Worker 构造/sink 类型契约。
- `runtime/examples/worker-platform.ts` — **新增**。迁移参考示例。

---

## Task 1: 配额计数器(无外部依赖,先落地)

**Files:**

- Create: `crates/vell-plugin-v8/src/script/worker_quota.rs`
- Modify: `crates/vell-plugin-v8/src/script/mod.rs`(pub mod 声明)
- Test: `crates/vell-plugin-v8/src/script/worker_quota.rs`(内联
  `#[cfg(test)] mod tests`)

**Interfaces:**

- Consumes: 无。
- Produces: `WorkerQuota` struct,方法 `try_acquire(plugin_id:
  &str) -> Result<QuotaHandle, QuotaError>`、
  `current_global() -> usize`、`QuotaError` enum
  (`PerPluginExceeded`/`GlobalExceeded`/`DepthExceeded`)。
  `QuotaHandle` Drop 时自动释放计数。

- [ ] **Step 1: Write the failing test**

```rust
// crates/vell-plugin-v8/src/script/worker_quota.rs 底部
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_acquire_succeeds_under_limit() {
        let quota = WorkerQuota::new(8, 32, 4);
        let h = quota.try_acquire("p1", 0).expect("under limit");
        assert_eq!(quota.current_global(), 1);
        drop(h);
        assert_eq!(quota.current_global(), 0);
    }

    #[test]
    fn try_acquire_fails_over_per_plugin() {
        let quota = WorkerQuota::new(2, 32, 4);
        let _h1 = quota.try_acquire("p1", 0).unwrap();
        let _h2 = quota.try_acquire("p1", 0).unwrap();
        let err = quota.try_acquire("p1", 0).unwrap_err();
        assert!(matches!(err, QuotaError::PerPluginExceeded));
    }

    #[test]
    fn try_acquire_fails_over_global() {
        let quota = WorkerQuota::new(100, 2, 4);
        let _h1 = quota.try_acquire("p1", 0).unwrap();
        let _h2 = quota.try_acquire("p2", 0).unwrap();
        let err = quota.try_acquire("p3", 0).unwrap_err();
        assert!(matches!(err, QuotaError::GlobalExceeded));
    }

    #[test]
    fn try_acquire_fails_over_depth() {
        let quota = WorkerQuota::new(100, 100, 2);
        let _h1 = quota.try_acquire("p1", 0).unwrap();
        let _h2 = quota.try_acquire("p1", 1).unwrap();
        let err = quota.try_acquire("p1", 2).unwrap_err();
        assert!(matches!(err, QuotaError::DepthExceeded));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vell-plugin-v8 worker_quota -- --nocapture`
Expected: FAIL with "cannot find type `WorkerQuota`"

- [ ] **Step 3: Write minimal implementation**

```rust
// crates/vell-plugin-v8/src/script/worker_quota.rs
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Debug)]
pub(super) enum QuotaError {
    PerPluginExceeded,
    GlobalExceeded,
    DepthExceeded,
}

pub(super) struct WorkerQuota {
    per_plugin: usize,
    global: usize,
    depth: usize,
    global_count: AtomicUsize,
    per_plugin_counts: Mutex<HashMap<String, usize>>,
}

pub(super) struct QuotaHandle {
    plugin_id: String,
    quota: &'static WorkerQuota,
}

impl Drop for QuotaHandle {
    fn drop(&mut self) {
        self.quota.global_count.fetch_sub(1, Ordering::Relaxed);
        let mut counts = self.quota.per_plugin_counts.lock().unwrap();
        if let Some(c) = counts.get_mut(&self.plugin_id) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                counts.remove(&self.plugin_id);
            }
        }
    }
}

impl WorkerQuota {
    pub(super) fn new(per_plugin: usize, global: usize, depth: usize) -> Self {
        Self {
            per_plugin,
            global,
            depth,
            global_count: AtomicUsize::new(0),
            per_plugin_counts: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn current_global(&self) -> usize {
        self.global_count.load(Ordering::Relaxed)
    }

    pub(super) fn try_acquire(
        &self,
        plugin_id: &str,
        current_depth: usize,
    ) -> Result<QuotaHandle, QuotaError> {
        if current_depth >= self.depth {
            return Err(QuotaError::DepthExceeded);
        }
        let mut counts = self.per_plugin_counts.lock().unwrap();
        let pc = counts.entry(plugin_id.to_owned()).or_insert(0);
        if *pc >= self.per_plugin {
            return Err(QuotaError::PerPluginExceeded);
        }
        if self.current_global() >= self.global {
            return Err(QuotaError::GlobalExceeded);
        }
        *pc += 1;
        self.global_count.fetch_add(1, Ordering::Relaxed);
        // 注意:静态生命周期这里需要用 leak 或用 Arc。
        // 见 Step 4 注释。
        todo!("handle static lifetime — see Step 4")
    }
}
```

- [ ] **Step 4: Fix lifetime, run test to verify pass**

`QuotaHandle` 引用 `&'static WorkerQuota` 要求 quota 是全局静态。
改为 `Arc<WorkerQuota>` 共享所有权:

```rust
// 修改:WorkerQuota 用 Arc 共享,QuotaHandle 持 Arc
use std::sync::Arc;

pub(super) struct QuotaHandle {
    plugin_id: String,
    quota: Arc<WorkerQuota>,
}

impl WorkerQuota {
    pub(super) fn try_acquire(
        self: &Arc<WorkerQuota>,
        plugin_id: &str,
        current_depth: usize,
    ) -> Result<QuotaHandle, QuotaError> {
        if current_depth >= self.depth {
            return Err(QuotaError::DepthExceeded);
        }
        let mut counts = self.per_plugin_counts.lock().unwrap();
        let pc = counts.entry(plugin_id.to_owned()).or_insert(0);
        if *pc >= self.per_plugin {
            return Err(QuotaError::PerPluginExceeded);
        }
        if self.current_global() >= self.global {
            return Err(QuotaError::GlobalExceeded);
        }
        *pc += 1;
        self.global_count.fetch_add(1, Ordering::Relaxed);
        Ok(QuotaHandle {
            plugin_id: plugin_id.to_owned(),
            quota: Arc::clone(self),
        })
    }
}
```

测试里改 `let quota = Arc::new(WorkerQuota::new(...))`,调
`quota.try_acquire(...)`。Drop 实现用 `Arc::clone` 持有。

Run: `cargo test -p vell-plugin-v8 worker_quota -- --nocapture`
Expected: PASS(4 tests)

- [ ] **Step 5: Add mod declaration + commit**

`crates/vell-plugin-v8/src/script/mod.rs` 加:

```rust
mod worker_quota;
```

```bash
cargo fmt --all -- --check
cargo clippy -p vell-plugin-v8 --all-targets -- -D warnings
git add crates/vell-plugin-v8/src/script/worker_quota.rs \
        crates/vell-plugin-v8/src/script/mod.rs
git commit -m "feat(vell-plugin-v8): add WorkerQuota counter for worker limits"
```

---

## Task 2: 主线程全局 Worker 构造器 + 双向 mpsc

**Files:**

- Modify: `crates/vell-plugin-v8/src/script/worker.rs`
  (删 `editor.worker.onMessage` 安装,改挂全局 `Worker` 构造器;
   mpsc 从单发扩为双向 `WorkerMessage` 通道)
- Modify: `crates/vell-plugin-v8/src/script/host.rs:61`
  (`install_editor_api` 旁挂全局 `Worker`)
- Test: `crates/vell-plugin-v8/src/script/worker.rs`(内联 tests)

**Interfaces:**

- Consumes: Task 1 的 `WorkerQuota`/`QuotaHandle`。
- Produces: `WorkerConstructArgs`(spec/entry/signal/depth)、
  `WorkerHandle`(postMessage/terminate/onmessage 通道)、
  全局 `Worker` 构造器函数 `worker_constructor`。

- [ ] **Step 1: Write the failing test**

```rust
// crates/vell-plugin-v8/src/script/worker.rs tests 模块
#[test]
fn worker_constructor_returns_handle_with_post_message() {
    let (host, _guard) = test_host();
    let result = host.evaluate(
        "const w = new Worker('./echo.ts', { type: 'module' });\
         typeof w.postMessage === 'function' && typeof w.terminate === 'function'",
    );
    assert_eq!(result, Some(true));
}
```

`echo.ts` = 嵌入测试资产,内容 `self.onmessage = e =>
self.postMessage(e.data);`。`test_host()` 是已有测试 helper(参考
现有 `worker_loads_embedded_resources...` 测试模式)。

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vell-plugin-v8 worker_constructor -- --nocapture`
Expected: FAIL with "Worker is not defined"

- [ ] **Step 3: Rewrite worker.rs mpsc + Worker constructor**

核心改动:

`WorkerRequest` 改为双向:

```rust
enum WorkerChannelMessage {
    ToWorker(serde_json::Value),
    FromWorker(serde_json::Value),
    Error(String),
    Terminated,
}
```

1. `ScriptWorker::start` 保留 std::thread + isolate,但 worker 全局
   挂 `self.onmessage`/`self.postMessage` 标准(而非
   `editor.worker.onMessage`)。
主线程挂全局 `Worker` 构造器:

```rust
// host.rs install_editor_api 内,或 worker.rs 新增 install_global_worker
fn install_global_worker(scope: &mut v8::PinScope<'_, '_>) {
    let global = scope.get_current_context().global(scope);
    let tmpl = v8::FunctionTemplate::new(scope, worker_constructor);
    let name = v8::String::new(scope, "Worker").unwrap();
    global.set(scope, name.into(), tmpl.get_function(scope).unwrap().into());
}
```

1. `worker_constructor` 回调解析 `new Worker(url, options)`,
   调 `ScriptWorker::start`,返回 `WorkerHandle` JS 对象(挂
   postMessage/terminate/addEventListener)。
2. worker isolate 全局挂 `self.onmessage` setter + `self.postMessage`
   - `self.close`(标准 DedicatedWorkerGlobalScope 子集)。

实现量大,参考现有 `run_worker`/`install_worker_api` 改写,保留
watchdog/heap limit/microtask 循环。`import.meta.url` 与动态
`import()` 留 Task 3,本任务 worker.ts 用单文件即可测。

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p vell-plugin-v8 worker_constructor -- --nocapture`
Expected: PASS

- [ ] **Step 5: Add bidirectional postMessage test**

```rust
#[test]
fn worker_post_message_roundtrip() {
    let (host, _guard) = test_host();
    host.evaluate(
        "let got;\
         const w = new Worker('./echo.ts', { type: 'module' });\
         w.addEventListener('message', e => { got = e.data; });\
         w.postMessage({ hello: 'world' });",
    );
    // pump microtasks + 等 worker 线程响应
    host.pump_worker_messages();
    assert_eq!(host.eval("got"), Some(serde_json::json!({"hello":"world"})));
}
```

实现 `pump_worker_messages`:主线程从 worker 的 `FromWorker` 通道
recv(带 timeout),dispatch 到注册的 message listener。

- [ ] **Step 6: Run + commit**

```bash
cargo test -p vell-plugin-v8 worker_ -- --nocapture
cargo fmt --all -- --check
cargo clippy -p vell-plugin-v8 --all-targets -- -D warnings
git add -A
git commit -m "feat(vell-plugin-v8): standard Web Worker constructor with bidirectional postMessage"
```

---

## Task 3: import.meta.url + 动态 import() + ES module worker

**Files:**

- Modify: `crates/vell-plugin-v8/src/script/module.rs`
  (新增 `HostInitializeImportMetaObjectCallback` 注入 url;
   `HostImportModuleDynamicallyCallback` 接 `resolve_module`)
- Modify: `crates/vell-plugin-v8/src/script/worker.rs`
  (worker transpile 从 `transpile_typescript` 改调
   `load_module_tree`)
- Test: `crates/vell-plugin-v8/src/script/module.rs`(tests)

**Interfaces:**

- Consumes: Task 2 的 Worker 构造器。
- Produces: `import.meta.url` 注入、标准动态 `import(specifier)`。

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn worker_import_meta_url_returns_file_url() {
    let (host, _guard) = test_host();
    let result = host.evaluate(
        "const w = new Worker(new URL('./meta.ts', import.meta.url),\
         { type: 'module' });",
    );
    // meta.ts: self.postMessage(import.meta.url)
    host.pump_worker_messages();
    let url = host.last_worker_message().unwrap();
    assert!(url.as_str().unwrap().starts_with("file:///"));
    assert!(url.as_str().unwrap().ends_with("/meta.ts"));
}

#[test]
fn worker_can_import_sibling_module() {
    let (host, _guard) = test_host();
    // uses.ts: import { x } from './helper.ts'; self.postMessage(x)
    host.evaluate(
        "const w = new Worker(new URL('./uses.ts', import.meta.url),\
         { type: 'module' });",
    );
    host.pump_worker_messages();
    assert_eq!(host.last_worker_message(), Some(json!(42)));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vell-plugin-v8 worker_import -- --nocapture`
Expected: FAIL(import.meta.url undefined / module 解析失败)

- [ ] **Step 3: Wire import.meta.url callback**

`module.rs` 新增:

```rust
pub(super) fn host_initialize_import_meta(
    context: v8::Local<v8::Context>,
    module: v8::Local<v8::Module>,
    meta: v8::Local<v8::Object>,
) {
    let scope = &mut v8::callback_scope!(unsafe context);
    let modules = scope.get_slot::<Rc<RefCell<ModuleMap>>>().cloned();
    if let Some(modules) = modules {
        let map = modules.borrow();
        let path = map.path_for(module.get_identity_hash().get(),
            &v8::Global::new(scope, module));
        if let Some(path) = path {
            let url = ModuleSpecifier::from_file_path(path).to_string();
            if let Some(key) = v8::String::new(scope, "url") {
                if let Some(val) = v8::String::new(scope, &url) {
                    meta.set(scope, key.into(), val.into());
                }
            }
        }
    }
}
```

在 isolate 创建时(`host.rs`/`worker.rs` 的 isolate setup)注册:

```rust
isolate.set_host_initialize_import_meta_object_callback(
    host_initialize_import_meta,
);
isolate.set_host_import_module_dynamically_callback(
    host_import_module_dynamically,
);
```

worker transpile 改:`worker.rs::ScriptWorker::start` 里

```rust
// 旧: let javascript = transpile_typescript(specifier, &source)?;
// 新:
let module = load_module_tree(scope, &path, &modules)?;  // 已有
```

(注意 worker isolate 也要设 slot `ModuleMap`,并注册
resolve_module callback——参考主 host 已有做法。)

- [ ] **Step 4: Wire dynamic import callback**

```rust
fn host_import_module_dynamically(
    context: v8::Local<v8::Context>,
    _referrer: v8::Local<v8::Module>,
    specifier: v8::Local<v8::String>,
    _attributes: v8::Local<v8::FixedArray>,
) -> v8::Local<v8::Promise> {
    let scope = &mut v8::callback_scope!(unsafe context);
    // 解析 specifier → load_module_tree → 返回 resolved promise
    // 参考 resolve_module,失败 reject promise
    // ...
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p vell-plugin-v8 worker_import -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
cargo fmt --all -- --check
cargo clippy -p vell-plugin-v8 --all-targets -- -D warnings
git add -A
git commit -m "feat(vell-plugin-v8): import.meta.url and dynamic import() for workers"
```

---

## Task 4: AbortSignal 取消 + terminate + 嵌套 spawn

**Files:**

- Modify: `crates/vell-plugin-v8/src/script/worker.rs`
  (Worker 构造器 options 加 `signal`;`terminate()`;嵌套
   spawn 传 current_depth+1)
- Test: `crates/vell-plugin-v8/src/script/worker.rs`(tests)

**Interfaces:**

- Consumes: Task 2/3 的 Worker 构造器、Task 1 的配额。
- Produces: 标准 `signal: AbortSignal` 选项、`worker.terminate()`、
  嵌套 `new Worker` 在 worker isolate 内可用。

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn worker_aborts_on_signal() {
    let (host, _guard) = test_host();
    host.evaluate(
        "const ac = new AbortController();\
         const w = new Worker('./long.ts', { type: 'module', signal: ac.signal });\
         ac.abort();",
    );
    // worker 应被中断;后续 postMessage 抛 InvalidStateError
    let err = host.eval("try { w.postMessage({}); } catch (e) { e.name }");
    assert_eq!(err, Some(json!("InvalidStateError")));
}

#[test]
fn worker_terminate_releases_resources() {
    let (host, _guard) = test_host();
    host.evaluate("const w = new Worker('./echo.ts',{type:'module'}); w.terminate();");
    // 配额应释放
    assert_eq!(host.quota().current_global(), 0);
}

#[test]
fn worker_nested_spawn_child() {
    let (host, _guard) = test_host();
    // parent.ts: const c = new Worker('./child.ts',{type:'module'});
    //            c.addEventListener('message', e => self.postMessage(e.data));
    host.evaluate("const w = new Worker('./parent.ts',{type:'module'}); w.postMessage('hi')");
    host.pump_worker_messages();
    assert_eq!(host.last_worker_message(), Some(json!("hi")));
}

#[test]
fn worker_quota_exceeded_throws() {
    let (host, _guard) = test_host();
    // spawn 9 个(per-plugin=8),第 9 个应抛 QuotaExceededError
    let err = host.eval(
        "try { for (let i=0;i<9;i++) new Worker('./echo.ts',{type:'module'}); }\
         catch(e){ e.name }",
    );
    assert_eq!(err, Some(json!("QuotaExceededError")));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vell-plugin-v8 worker_abort worker_terminate worker_nested worker_quota -- --nocapture`
Expected: FAIL(signal/terminate/nested/quota 均未实现)

- [ ] **Step 3: Implement AbortSignal, terminate, nested, quota**

1. `worker_constructor` 解析 options.signal:`AbortSignal` 实例,
   注册 abort handler → drop `WorkerHandle` 的 sender → worker
   线程 recv 返回 → 线程退出。
2. `WorkerHandle::terminate` 方法(挂 JS `terminate` 函数):
   同上 drop sender。
3. 嵌套:worker isolate 全局也挂 `Worker` 构造器(参考主线程)。
   `worker_constructor` 在 worker 内调用时 current_depth+1,调
   `WorkerQuota::try_acquire` 校验深度。
4. 配额:构造时 try_acquire,失败抛
   `DOMException("QuotaExceededError")`。构造成功持 `QuotaHandle`,
   terminate/abort/error 时 drop。

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vell-plugin-v8 worker_abort worker_terminate worker_nested worker_quota -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
cargo fmt --all -- --check
cargo clippy -p vell-plugin-v8 --all-targets -- -D warnings
git add -A
git commit -m "feat(vell-plugin-v8): AbortSignal, terminate, nested spawn, quota enforcement"
```

---

## Task 5: 错误事件分发

**Files:**

- Modify: `crates/vell-plugin-v8/src/script/worker.rs`
  (worker 未捕获异常 → 主线程 `error` 事件;超时/heap → error 事件)
- Test: `crates/vell-plugin-v8/src/script/worker.rs`(tests)

**Interfaces:**

- Consumes: Task 2-4。
- Produces: `ErrorEvent` 分发到 `addEventListener("error")`。

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn worker_error_event_on_uncaught_exception() {
    let (host, _guard) = test_host();
    // throw.ts: self.onmessage = () => { throw new Error("boom") }
    host.evaluate(
        "let err;\
         const w = new Worker('./throw.ts',{type:'module'});\
         w.addEventListener('error', e => { err = e.message });\
         w.postMessage({});",
    );
    host.pump_worker_messages();
    assert_eq!(host.eval("err"), Some(json!("boom")));
}

#[test]
fn worker_timeout_emits_error_event() {
    let (host, _guard) = test_host_with_timeout(Duration::from_millis(50));
    // loop.ts: self.onmessage = () => { while(true){} }
    host.evaluate(
        "let name;\
         const w = new Worker('./loop.ts',{type:'module'});\
         w.addEventListener('error', e => { name = e.name });\
         w.postMessage({});",
    );
    host.pump_worker_messages();
    assert_eq!(host.eval("name"), Some(json!("TimeoutError")));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vell-plugin-v8 worker_error worker_timeout -- --nocapture`
Expected: FAIL(error 事件未分发)

- [ ] **Step 3: Implement error event dispatch**

1. worker 线程:`run_worker` 的 `execute_request` 返回 Err 时,
   往 `FromWorker` 通道发 `Error(String)`,而非只 send result。
2. 主线程 `pump_worker_messages` 收 `Error(msg)` → 构造
   `ErrorEvent`(message/filename/lineno/name),dispatch 到注册的
   `error` listener。
3. watchdog 超时/heap → 同上,`name` 设
   `TimeoutError`/`ResourceExhausted`。

`ErrorEvent` 构造:参考 DOM lib `ErrorEventInit`,vell 实现
最小子集(message/filename/lineno/name)。

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vell-plugin-v8 worker_error worker_timeout -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
cargo fmt --all -- --check
cargo clippy -p vell-plugin-v8 --all-targets -- -D warnings
git add -A
git commit -m "feat(vell-plugin-v8): ErrorEvent dispatch for worker failures and timeouts"
```

---

## Task 6: editor.writeDecorations frame-safe sink

**Files:**

- Modify: `crates/vell-plugin-v8/src/script/host.rs:install_editor_api`
  (加 `editor.writeDecorations`)
- Modify: `crates/vell-plugin-v8/src/script/schema.rs`
  (从 analysis 搬 revision 校验 + 事务化安装逻辑到 sink)
- Modify: `crates/vell-plugin-v8/src/script/mod.rs`
  (删 `ScriptAnalysisDefinition`,新增 sink 实现依赖)
- Test: `crates/vell-plugin-v8/src/script/host.rs`(tests)

**Interfaces:**

- Consumes: Task 2(Worker 通道,用于接收结果回调)。
- Produces: `editor.writeDecorations(revision: number, spans:
  TextDecorationSpan[])` —— 校验 revision、事务化安装、失败回滚。

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn write_decorations_installs_for_current_revision() {
    let (host, _guard) = test_host_with_buffer("hello", 5);
    host.eval("editor.writeDecorations(5, [{ range: {...}, face: 'f' }])");
    let decs = host.content_decorations();
    assert_eq!(decs.len(), 1);
}

#[test]
fn write_decorations_drops_stale_revision() {
    let (host, _guard) = test_host_with_buffer("hello", 5);
    host.eval("editor.writeDecorations(3, [{ range: {...}, face: 'f' }])");
    assert_eq!(host.content_decorations().len(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vell-plugin-v8 write_decorations -- --nocapture`
Expected: FAIL(writeDecorations 未定义)

- [ ] **Step 3: Implement writeDecorations sink**

从 `schema.rs::parse_analyses` 旁的 analysis 调度代码搬:

- revision 校验(从 `ScriptAnalysisDefinition::apply` 的
  `context.revision` 比对逻辑搬)。
- 事务化安装(框进当前 `ExecutionFrame`,失败回滚)。
- `write_decorations` 回调解析 `(revision, spans)`,调搬过来的
  逻辑,写到 content decoration store。

注:analysis 的 generation/自动取消**不搬**(作者用 AbortController)。

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vell-plugin-v8 write_decorations -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
cargo fmt --all -- --check
cargo clippy -p vell-plugin-v8 --all-targets -- -D warnings
git add -A
git commit -m "feat(vell-plugin-v8): frame-safe editor.writeDecorations sink"
```

---

## Task 7: 删除 analysis / editor.worker.onMessage / job 表面

**Files:**

- Modify: `crates/vell-plugin-v8/src/script/schema.rs`
  (删 `parse_analyses`、`parse_worker`、analysis 字段校验)
- Modify: `crates/vell-plugin-v8/src/script/mod.rs`
  (删 `ScriptAnalysisDefinition`、`ContentJob`/`applyJob`/
  `ScriptWorker` 字段)
- Modify: `crates/vell-plugin-v8/src/script/mode_adapter.rs`
  (删 analysis 调度:input 轮询/generation/apply)
- Test: 确保现有非 analysis 测试仍过

**Interfaces:**

- Consumes: Task 6 的 sink(替代 analysis 写装饰)。
- Produces: 无 analysis 表面残留。

- [ ] **Step 1: Write a regression test for analysis removal**

```rust
#[test]
fn mode_definition_rejects_analysis_field() {
    let (host, _guard) = test_host();
    let err = host.define_mode_error(
        "{ on: { buffer: { analysis: {} } } }",
    );
    assert!(err.contains("unknown field") || err.contains("analysis"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vell-plugin-v8 mode_definition_rejects -- --nocapture`
Expected: FAIL(analysis 仍被接受)

- [ ] **Step 3: Delete analysis parsing + scheduling**

1. `schema.rs::install_editor_api` 删 analysis 相关安装(若有)。
2. `schema.rs::parse_buffer_adapter`(约 line 280-330)删
   `analysis` 分支、`parse_analyses` 调用。
3. `schema.rs` 删 `parse_analyses`/`parse_worker` 函数。
4. `schema.rs` legacy 校验(line 293/307)删 `worker`/`job`/
   `applyJob`/`analysis` 字段。
5. `mod.rs` 删 `ScriptAnalysisDefinition` struct、
   `ContentJob`/`applyJob`/`worker` 字段与解析。
6. `mode_adapter.rs` 删 analysis 调度(input 轮询/generation/
   apply 调用块)。

- [ ] **Step 4: Run full test suite to verify no regressions**

Run: `cargo test -p vell-plugin-v8 -- --nocapture`
Expected: analysis 相关测试改/删后全过

- [ ] **Step 5: Commit**

```bash
cargo fmt --all -- --check
cargo clippy -p vell-plugin-v8 --all-targets -- -D warnings
git add -A
git commit -m "refactor(vell-plugin-v8): remove Mode-bound analysis and job surface"
```

---

## Task 8: 更新 editor.d.ts 类型表面

**Files:**

- Modify: `runtime/editor.d.ts`(删 analysis/WorkerResponse/
  editor.worker/job 类型;加 Worker 引用 + writeDecorations)
- Test: `runtime/type-tests/mode.ts`、`pnpm typecheck`

**Interfaces:**

- Consumes: Task 6/7 的 Rust 侧表面。
- Produces: 与 Rust 同步的 TS 契约。

- [ ] **Step 1: Update editor.d.ts**

删除:

- `BackgroundAnalysisDefinition`/`BackgroundAnalysisInputContext`/
  `BackgroundAnalysisApplyContext`/`BackgroundAnalysisBase`/
  `TextSnapshotAnalysisMessage`(line ~327-388)
- Mode 定义里 `analysis?: Record<...>` 字段(line ~327)
- `editor.worker.onMessage`(line ~549-555)
- `ModeDefinition.worker`/`content.job`/`content.applyJob`/
  `WorkerResponse` 泛型(line ~485-503)

新增(文件顶部或 editor 对象内):

```ts
// editor.d.ts 内
// 标准 Web Worker 已由 TS DOM lib 提供 Worker/MessageEvent/
// AbortController/ErrorEvent 类型。vell 只需声明全局可用:
declare const Worker: typeof globalThis.Worker;

declare const editor: {
  // ... 既有 theme/modes/faces/resources
  writeDecorations(
    revision: number,
    spans: TextDecorationSpan[],
  ): void;
};
```

- [ ] **Step 2: Add type contract tests**

```ts
// runtime/type-tests/mode.ts 追加
import type { Worker, MessageEvent, AbortController } from "./helpers";
import type { TextDecorationSpan } from "../editor";

declare const editor: typeof import("../editor").editor;

// 构造面
const w: Worker = new Worker(
  new URL("./parser.ts", import.meta.url),
  { type: "module" },
);
w.postMessage({ text: "" });
w.addEventListener("message", (e: MessageEvent) => { void e.data; });
w.terminate();

const ac: AbortController = new AbortController();
new Worker(new URL("./x.ts", import.meta.url),
  { type: "module", signal: ac.signal });

// sink
editor.writeDecorations(1, [] as TextDecorationSpan[]);
```

- [ ] **Step 3: Run typecheck**

Run: `pnpm typecheck`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add runtime/editor.d.ts runtime/type-tests/mode.ts
git commit -m "refactor(runtime): update editor.d.ts for Web Worker platform primitive"
```

---

## Task 9: 迁移 tree-sitter 插件

**Files:**

- Modify: `runtime/plugins/tree-sitter/plugin.ts`
- Modify: `runtime/plugins/tree-sitter/worker.ts`
- Create: `runtime/plugins/tree-sitter/parser.ts`(可选拆分)
- Test: `cargo test -p vell-plugin-v8`(Rust 集成)、`pnpm typecheck`

**Interfaces:**

- Consumes: Task 2-8 的完整表面。
- Produces: tree-sitter 在新模型下工作。

- [ ] **Step 1: Migrate plugin.ts**

参考 spec 迁移示例:analysis 声明 → `on.buffer.changed` + 显式
`new Worker` + `AbortController` + `editor.writeDecorations`。
保留 `languageFor`/`HighlightState`/`HighlightResult` 类型。

- [ ] **Step 2: Migrate worker.ts**

`editor.worker.onMessage` → `self.onmessage`。
可选拆 `parser.ts`/`query.ts`(YAGNI 判断:若 355 行可读则不拆)。

- [ ] **Step 3: Run typecheck + tests**

Run: `pnpm typecheck`
Run: `cargo test -p vell-plugin-v8 -- --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add runtime/plugins/tree-sitter/
git commit -m "refactor(tree-sitter): migrate to standard Web Worker"
```

---

## Task 10: 迁移示例 + 全量验证

**Files:**

- Create: `runtime/examples/worker-platform.ts`
- Test: TS + Rust 双覆盖(AGENTS.md 要求)

- [ ] **Step 1: Write example**

```ts
// runtime/examples/worker-platform.ts
// 参考 spec 迁移示例,展示 new Worker + AbortController +
// editor.writeDecorations 的标准用法
```

- [ ] **Step 2: Full workspace verification**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
pnpm typecheck
cargo doc --workspace --all-features --no-deps
```

Expected: 全过

- [ ] **Step 3: Commit**

```bash
git add runtime/examples/worker-platform.ts
git commit -m "docs(examples): add worker platform migration example"
```

---

## Self-Review

(实施者完成所有任务后,对照 spec 自查)

**Spec coverage:**

- 总体架构三层分离 → Task 2(原语)+ 6(sink)+ 7(删 analysis)
- 标准 Web Worker 表面 → Task 2/3/4
- frame-safe sink → Task 6
- 嵌套 spawn/防爆炸/生命周期/错误 → Task 1/4/5
- 迁移影响 → Task 7/8/9/10
- 测试策略 → 各 Task 内 + Task 10 全量
- 删除表面 → Task 7/8
- 保留搬进平台 → Task 6
- 不保留 generation/自动取消 → Task 7(删)+ Task 9(作者
  自管 AbortController)

**Type consistency:**

- `WorkerQuota`/`QuotaHandle`/`QuotaError` — Task 1 定义,
  Task 4 消费,签名一致。
- `WorkerHandle` — Task 2 定义 postMessage/terminate,
  Task 4 扩 addEventListener("error")/signal,一致。
- `editor.writeDecorations(revision, spans)` — Task 6 定义,
  Task 8 TS 契约,Task 9 使用,签名一致。

**Placeholder scan:** 无 TBD/TODO。Task 3 Step 4 的
`host_import_module_dynamically` 主体用 `// ...` 标注参考方向,
实施时补全(非占位,是参考现有 resolve_module)。
