# M5 Typed Native Mode 与状态事务

**状态：** 已完成

**日期：** 2026-07-21

## 1. 结果

`vell-mode` 新增 `TypedMode` 和唯一的 `ErasedMode<M>` 适配器。原生 Mode
使用关联类型声明 content state、view state 和 job output；注册时才转换到
现有 object-safe `Mode`。Mode 身份、注册中心、状态表和事务生命周期没有产生
第二套实现。

动态 TypeScript Mode 继续使用原 `Mode` 契约和 JSON-compatible 状态，没有
被强制包装为泛型状态。

## 2. 类型边界

typed native Mode 的业务回调直接接收自己的状态类型：

```text
TypedMode
├── ContentState
├── ViewState
└── JobOutput
        |
        v
ErasedMode<M>
        |
        v
ModeRegistry / ModeContentStore / ModeViewStore
```

状态和 job output 的 downcast 只存在于 `ErasedMode<M>`。类型不匹配时返回
带 Mode 名称和状态种类的 `ModeError::StateTypeMismatch`。测试中的原生
`DraftStateMode` 已迁移到 typed API，回调内不再手写 downcast。

## 3. 状态提交策略

`ModeJobSlot` 已从裸 `String` 收敛为基于 `Arc<str>` 的结构化 newtype。
字符串仅在动态插件内部的 analysis catalog 中继续使用。

typed state wrapper 可以比较同类型状态。draft commit 仅在比较明确返回相同、
fault 状态相同且调度标记相同时跳过提交，因此无变化 typed callback 不推进
Mode revision。

动态状态默认不做值比较。审查期间曾验证通用深比较，但它会让脚本 JSON 状态
进入每次提交的比较热路径，因此没有保留。该决定同时避免要求所有动态状态
实现 `PartialEq`。

## 4. 性能门槛

使用 M0 的 ignored baseline 对 M4 完成提交和 M5 当前实现进行相同 A/B：

```text
M4 completion (7962e30)
  native input       124.648 us/iteration
  script input       352.702 us/iteration
  clone time         533,400 ns / 3,000 script clones

M5 typed state
  native input       128.073 us/iteration
  script input       364.089 us/iteration
  clone time         527,300 ns / 3,000 script clones
```

两组 debug-profile 数据处于同一量级。clone 总成本仍远低于 callback 总耗时，
没有证据支持引入通用 journal、copy-on-write 或 persistent collection。

## 5. 验证

执行并通过：

```text
cargo test --workspace --all-features --quiet
cargo clippy --workspace --all-targets --all-features -- -D warnings
$env:RUSTDOCFLAGS = "-D warnings"
cargo doc --workspace --all-features --no-deps
cargo check -p vell-mode --no-default-features
cargo fmt --check
pnpm typecheck
git diff --check
```

最终 Rust 测试结果为 484 项通过、1 项忽略、0 项失败。新增测试覆盖 typed
state、typed job output、集中式类型错误和无变化 revision。
