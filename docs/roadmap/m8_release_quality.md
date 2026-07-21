# M8 发布质量验收记录

**完成日期：** 2026-07-21

**实现提交：** `9b4a90e`

## 交付结果

- 新增 GitHub Actions CI，固定 Rust 1.88，并在 Windows、Linux、macOS
  分别运行无 V8 核心路径和完整 V8 宿主测试。
- Linux quality job 运行格式检查、严格 Clippy、Rustdoc 和 TypeScript
  契约检查。
- 所有 Cargo CI 命令使用 `--locked`，避免流水线隐式更新依赖。
- CI 检查 `modeleaf-app` 的普通依赖树，禁止 V8 越过宿主边界。
- 增加 bare import 和配置目录逃逸的模块加载回归测试。
- 新增 [`docs/release.md`](../release.md)，记录自动门槛、插件兼容矩阵、
  性能基准命令和人工发布条件。

没有新增 fuzz 或 benchmark 依赖。`TextChangeSet` 已有穷举式组合等价测试；
UTF-16 surrogate、operation 预算、取消、stale result、Content/View 身份和
事务回滚也已有直接测试。当前新增框架的维护成本高于它能补足的风险覆盖。

## 迭代复审

第一轮复审发现模块路径逃逸只有实现而没有直接测试，已补充越界和 bare
specifier 两个用例。

第二轮复审发现 Unix CI 的管道可能把 `cargo tree` 失败误判成“没有 V8”，
已改为先捕获成功输出再匹配。

第三轮复审发现仓库的隐藏目录忽略规则会排除 `.github`，已增加只允许该
workflow 的精确例外。

第四轮复审确认：

- core job 不编译 V8，并独立检查 App 普通依赖；
- full-host job 在三个目标系统执行完整 workspace；
- quality job 覆盖 Rust 与 TypeScript 的静态契约；
- 发布不会在名称归属和许可证尚未决定时被自动触发。

未再发现需要修复的问题。

## 本地验证

```text
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo doc --locked --workspace --all-features --no-deps
cargo test --locked -p modeleaf-protocol -p modeleaf-core
  -p modeleaf-frontend -p modeleaf-mode -p modeleaf-tui
cargo check --locked -p modeleaf-app --lib
pnpm typecheck
cargo fmt --all -- --check
git diff --check
```

本机 Windows 结果为 488 个测试通过、1 个手工性能基准 ignored。Clippy、
Rustdoc、TypeScript、格式、无 V8 App 构建和差异检查均通过。

三平台结果需要在提交进入 GitHub 后由新工作流首次执行；工作流本身不发布
artifact，也不需要写权限。

## 验收判断

M8 的自动化质量门槛、风险回归和兼容性文档均已建立。registry 名称保留、
许可证选择和实际发布仍是维护者的外部决策，不由本路线图擅自执行。
