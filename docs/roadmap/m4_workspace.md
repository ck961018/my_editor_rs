# M4 Workspace 边界提取

**状态：** 已完成

**日期：** 2026-07-21

## 1. 结果

仓库已从单一 binary crate 演进为以下 workspace：

```text
modeleaf
├── modeleaf-app
├── modeleaf-core
├── modeleaf-frontend
├── modeleaf-mode
├── modeleaf-plugin-v8
├── modeleaf-protocol
└── modeleaf-tui
```

根 package 只保留 CLI、终端初始化和 composition root。`terminal` 继续作为
`modeleaf-tui` 的内部模块；尚无第二个宿主需要复用插件 schema，因此没有创建
`modeleaf-terminal` 或 `modeleaf-plugin-api`。

## 2. 提取记录

各边界均由独立提交完成，可单独审查或 revert：

- `9edc146`：提取 `modeleaf-protocol`；
- `46449dd`：提取 `modeleaf-core`；
- `f70d765`：提取 `modeleaf-frontend`；
- `96260f7`：提取 `modeleaf-tui`；
- `0fcafaf`：解除 Mode context 对 App View 的依赖；
- `83b26f3`：提取 `modeleaf-mode`；
- `31511ea`：提取 `modeleaf-plugin-v8`；
- `591c807`：提取 `modeleaf-app`。

TUI 在其依赖已稳定后提前提取。V8 宿主在 App 之前提取，使 App 接收统一的
`Box<dyn Mode>`，不再认识 `ScriptHost` 或 `ScriptMode`。

## 3. 最终依赖边界

```text
modeleaf
├── modeleaf-app
├── modeleaf-plugin-v8
└── modeleaf-tui

modeleaf-app
├── modeleaf-core
├── modeleaf-frontend
├── modeleaf-mode
└── modeleaf-protocol

modeleaf-plugin-v8
├── modeleaf-core
├── modeleaf-mode
└── modeleaf-protocol

modeleaf-tui
├── modeleaf-frontend
└── modeleaf-protocol
```

已核对以下约束：

- `modeleaf-app` 的 normal dependency graph 不含 V8、TUI、crossterm 或
  Taffy；
- `modeleaf-tui` 不依赖 `modeleaf-app`；
- `modeleaf-plugin-v8` 的公共入口不暴露 V8 类型；
- `modeleaf-protocol` 保持零依赖；
- `modeleaf-core` 不依赖异步运行时、前端、终端或 V8；
- 完整脚本集成测试仅通过 App 的 dev-dependency 使用 V8。

## 4. 验证

阶段完成后执行并通过：

```text
cargo check --workspace --all-features
cargo test --workspace --all-features --quiet
cargo clippy --workspace --all-targets --all-features -- -D warnings
$env:RUSTDOCFLAGS = "-D warnings"
cargo doc --workspace --all-features --no-deps
cargo fmt --check
pnpm typecheck
git diff --check
```

Rust 测试结果为 482 项通过、1 项忽略、0 项失败。另行使用 `cargo tree`
核对了 App、TUI 与 V8 宿主的 normal dependency graph。

## 5. 后续边界

M4 只固化已有逻辑边界，没有顺便实施 M5 的 typed native Mode，也没有创建
未被真实消费者需要的 crate。后续继续按 roadmap 处理 Mode 类型安全、插件
诊断和发布质量。
