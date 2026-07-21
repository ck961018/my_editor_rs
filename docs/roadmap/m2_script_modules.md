# M2 Script 物理模块拆分

**状态：** 已完成

**日期：** 2026-07-21

## 1. 结果

原 `src/app/script.rs` 已转换为目录模块，当前结构为：

```text
src/app/script/
├── mod.rs
├── host.rs
├── invocation.rs
├── module.rs
├── bridge.rs
├── schema.rs
├── primitives.rs
└── worker.rs
```

职责边界：

- `host.rs` 拥有主 isolate、context、module 生命周期和 callback 编排；
- `invocation.rs` 是唯一直接调用 `v8::Function::call` 的位置，并持有
  watchdog 与 heap 恢复机制；
- `module.rs` 负责 TypeScript 转译、本地 module graph 和异常格式化；
- `bridge.rs` 负责 Rust、JSON 与 V8 值的窄转换；
- `schema.rs` 只解析插件 Mode definition；
- `primitives.rs` 和 `worker.rs` 保持原有独立职责；
- `mod.rs` 保留 façade、共享私有类型和 ScriptMode adapter。

## 2. 有意延后

本阶段没有预建 `mode_adapter.rs`。adapter、状态和 presentation 仍共享大量
私有类型，M5 会修改这组类型边界；现在强拆会制造一轮临时可见性和紧随其后的
重写。`mode.rs` 与 `kernel.rs` 也没有因文件长度而拆分。

本阶段没有新增 trait、生命周期或动态分派，也没有改变公开 API、插件 schema
或执行顺序。

## 3. 验证

```text
cargo test
cargo clippy --all-targets --all-features
cargo fmt -- --check
pnpm typecheck
git diff --check
```

完整测试继续执行 482 项，其中 481 项通过，M0 手动性能基准保持忽略。
