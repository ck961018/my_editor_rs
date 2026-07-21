# M1 主 ScriptHost 执行预算与恢复

**状态：** 已完成

**日期：** 2026-07-21

## 1. 结果

主 `ScriptHost` 的 module、state factory、action、content change、content
job 和 analysis callback 现在统一经过 invocation watchdog。显式 microtask
checkpoint 位于同一预算范围内。

默认限制：

```text
callback deadline       2 s
module startup deadline 5 s
isolate heap            128 MiB
heap recovery reserve   16 MiB
TypeScript/module file  4 MiB
module graph            16 MiB
structured input        32 MiB
state/callback result   4 MiB
staged operations       10,000
decorations             100,000
```

超时通过线程安全的 V8 handle 请求终止。终止异常传播出 V8 scope 后，宿主
清理 terminate 状态；heap 接近上限时也使用该路径，并在恢复后还原上限。
启动失败会撤销本次新增的 Mode definition 和 diagnostic。

worker isolate 已经运行在独立线程，因此关闭 V8 的异步 WASM 编译，使编译和
取消留在同一 worker 内，避免多个 isolate 的平台任务延迟取消。进入队列时已
取消的请求不会再进入 V8。

callback 产生的新 state、operation 和 presentation 先保存在 Rust 暂存值中，
完整验证成功后才发布。现有 `ExecutionFrame` 和 Mode draft 继续负责跨 Mode、
Content、View、history 与后台结果的外层原子提交。

## 2. 信任边界

所有 Script Mode 仍共享主 isolate。本阶段可以终止无限循环、无限 microtask
和受控 heap 压力，但不提供进程级恶意代码隔离，也不回滚插件修改的 module
全局变量。需要自动运行不受信任插件时，必须重新评估 per-plugin isolate 或
进程隔离。

## 3. 回归覆盖

新增测试覆盖：

- 启动阶段无限循环和无限 microtask；
- 超时及 heap 超限后的宿主复用；
- 超时 action 的 state、operation 和 view policy 回滚；
- TypeScript、module graph、state、analysis result、operation 和 decoration
  上限；
- 启动失败不保留部分 Mode definition；
- 脚本超时后继续 native 编辑、保存和退出。

## 4. 验证

同一台机器、同一 test profile 下，M0 与 M1 的脚本输入基准为：

```text
M0 script input  241.686 us/iteration
M1 script input  384.707 us/iteration
difference       +143.021 us/iteration
```

每次 callback 创建并回收一个 watchdog 线程。当前绝对延迟仍低于 0.4 ms；
在发布 profile 的真实输入延迟基准证明这是瓶颈前，不引入常驻调度线程。

```text
cargo test
cargo clippy --all-targets --all-features
cargo fmt -- --check
pnpm typecheck
git diff --check
```
