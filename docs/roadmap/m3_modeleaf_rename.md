# M3 Modeleaf 命名迁移

**状态：** 已完成

**日期：** 2026-07-21

## 1. 名称决定

正式名称采用 `Modeleaf`：

```text
显示名称       Modeleaf
Cargo package  modeleaf
Rust crate     modeleaf
二进制命令     modeleaf
环境变量       MODELEAF_*
配置目录       modeleaf
```

名称取 Mode 与 leaf/page 的组合，既对应项目的 Mode 扩展模型，也不把未来
前端限制在终端。

2026-07-21 的工程冲突检查结果：

- `cargo search modeleaf --limit 5` 没有返回同名 crate；
- `npm view modeleaf name version` 返回 registry 404；
- GitHub `users/modeleaf` API 返回 404；
- 公开检索未发现同名软件或商标，只发现一个无内容的
  [同名博客][modeleaf-blog]。

这只是工程命名筛查，不构成法律意见，也不代表已经保留 registry 名称。
仓库托管平台的实际重命名属于独立的外部操作，本提交不执行。

## 2. 兼容迁移

新的配置优先级为：

```text
MODELEAF_CONFIG
MY_EDITOR_CONFIG
<platform>/modeleaf/config.ts
<platform>/my_editor_rs/config.ts
```

旧环境变量和目录只在 0.1.x 保留。首次使用旧入口时输出一次警告，并明确在
0.2.0 删除。TypeScript 全局对象 `editor` 是领域 API，不随品牌改名。

## 3. 验证

```text
cargo test
cargo clippy --all-targets --all-features
cargo fmt -- --check
pnpm typecheck
cargo install --path .
git diff --check
```

[modeleaf-blog]: https://heartia-model.hatenablog.jp/
