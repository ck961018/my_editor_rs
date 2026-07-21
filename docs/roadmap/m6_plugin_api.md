# M6 插件 API 与诊断成熟度验收记录

**完成日期：** 2026-07-21

**实现提交：** `e08eae0`、`b1288b1`

## 交付结果

- `PresentationLayerStore` 可报告 Mode 顺序、各策略字段的来源，以及每个
  Mode 的 Content/View decoration 数量。
- `FaceRegistry` 可报告当前 provider 和重复注册冲突；主题覆盖会更新有效
  provider，未知 Mode 不再被诊断接口静默丢弃。
- App 提供只读的 Mode 与 Face 诊断查询，render 热路径仍只读取缓存。
- `runtime/editor.d.ts` 是公开 TypeScript schema 的唯一真相源，并通过
  `TYPESCRIPT_DECLARATIONS` 嵌入 Rust crate。
- `load_typescript_modes` 提供不启动 TUI 的加载入口，只返回通用 Mode 和
  结构化诊断，不向调用方暴露 V8 类型。
- v1 schema 在 0.1.x 弃用，0.2.x 保留一次结构化警告，并计划在 0.3.0
  移除。
- `runtime/examples/v1-migration.ts` 同时由 TypeScript 编译器检查和 Rust
  宿主执行，覆盖 v1 与 v2 的机械迁移路径。

没有建立独立 `vell-plugin-api` crate。当前公开契约只有 V8 宿主这一
个消费者，留在 `vell-plugin-v8` 更小且不会制造空边界。

## 迭代复审

第一轮复审发现两个问题：未知 Mode 会被诊断查询静默忽略，主题覆盖后
Face provider 仍指向旧 Mode。两者已在 `e08eae0` 提交前修复并增加回归
测试。

第二轮复审发现迁移示例使用了宿主不支持的 `ctrl+i` 拼写。示例改为合法的
单字符键后，TypeScript 与 V8 双侧检查均通过。

第三轮复审确认：

- Rust schema、声明文件、弃用消息和移除版本均有契约测试约束；
- headless 入口不泄漏 V8 类型，也不创建第二套 Mode 生命周期；
- 结构化诊断只按调用方请求生成，不增加 render 路径开销；
- 现有 state、operation、decoration、异常、超时测试继续通过。

未再发现需要修复的问题。

## 验证

```text
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --all-features --no-deps
cargo fmt --all -- --check
pnpm typecheck
git diff --check
```

workspace 测试全部通过，手工性能基准仍保持 ignored；Clippy、Rustdoc、
格式、TypeScript 类型检查和差异检查均通过。

## 验收判断

M6 的四项验收条件均满足。插件契约已有单一可校验来源、v1 有明确迁移
周期、Mode/Face 合成来源可定位，插件逻辑也可在无 TUI 环境下执行测试。
