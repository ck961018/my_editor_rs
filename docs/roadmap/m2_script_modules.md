# M2 Script 物理模块拆分

**状态：** 已完成

**日期：** 2026-07-21

## 1. 结果

原 `src/app/script.rs` 已转换为目录模块，workspace 提取后
当前位于：

```text
crates/vell-plugin-v8/src/script/
├── mod.rs
├── host.rs
├── invocation.rs
├── mode_adapter.rs
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
- `mode_adapter.rs` 负责 ScriptMode 的 Mode contract 和 job 编排；
- `primitives.rs` 和 `worker.rs` 保持原有独立职责；
- `mod.rs` 保留 façade、共享私有类型和转换函数。

## 2. M5 后续

M2 最初有意延后 `mode_adapter.rs`，避免在 M5 强类型 Mode
边界完成前制造临时可见性。M5 完成后，ScriptMode contract
和 job 编排已迁入该模块；共享状态类型仍由 façade 持有。
`mode.rs` 与 `kernel.rs` 仍不因文件长度而拆分。

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
