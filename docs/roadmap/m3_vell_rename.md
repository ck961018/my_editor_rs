# M3 Vell 命名迁移

**状态：** 已完成
**日期：** 2026-07-21

## 1. 名称决定

正式名称采用 `Vell`：

```text
显示名称       Vell
Cargo package  vell
Rust crate     vell
二进制命令     vell
环境变量       VELL_*
配置目录       vell
```

`Vell` 让人联想到 vellum（书写用的羊皮纸）和一张承载内容的页面。
它简短、克制，也可以进一步引申为多层页面：Content 是载体，Mode 赋予
行为，View 决定最终显现的层次。这个意象贴合编辑器架构，又不把项目限制在
终端、纯文本或某一种 Mode 模型中。

仓库托管平台和 registry 的实际名称保留属于独立的外部操作，本提交不执行。

## 2. 干净迁移

项目不保留旧品牌兼容层。用户配置只接受：

```text
VELL_CONFIG
<platform>/vell/config.ts
```

旧环境变量、旧配置目录、旧 crate 名和对应的迁移警告全部删除。
TypeScript 全局对象 `editor` 是领域 API，不随品牌改名。

## 3. 验证

```text
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features
cargo fmt --all -- --check
pnpm typecheck
cargo install --locked --path .
git diff --check
```
