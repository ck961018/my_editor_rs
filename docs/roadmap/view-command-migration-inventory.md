# View 命令迁移清单

状态：M1 已完成（保留为迁移记录）

更新日期：2026-08-10

本文记录 M1 必须处理的旧 Buffer 命令表面。它只列迁移边界，不规定后续
模块的内部文件布局。领域决策见
[命令目标 ADR](../adr/0004-commands-target-content-or-view.md)。

## 1. 迁移策略

采用一次性迁移，不提供旧名称 alias 或 deprecation window。原因是当前
接口仍属于实验性插件 contract，同时保留两套名称会让插件作者继续把
ContentId 当作 View switch target。

M1 的同一个提交必须同步更新 Rust operation、原生命令、V8 primitives、
TypeScript 声明、内建插件、示例和测试。

## 2. 公开原生命令

当前定义位于 `crates/vell-app/src/native_commands.rs` 和生成的
`runtime/commands.generated.d.ts`。

- `newBuffer()` 迁到 `content.create()`，返回 ContentId。
- `switchBuffer(contentId)` 迁到 `view.switch(viewSpec)`。
- `save()` 迁到 `content.save()`，目标默认为当前 View 的主要 binding。

`view.switch` 不接受裸 ContentId。M1 先使用最小的封闭 BufferView spec，
当前使用 `{ type: "core.buffer", content: contentId }`。M3 再把该字段
演进为由 View definition 声明的 `document` binding，M1 不提前实现通用
binding contract。

## 3. TypeScript Mode primitives

当前 `BufferCommandContext.buffers` 暴露：

- `create`、`open`、`list`；
- `close`、`save`、`saveAs`、`reload`；
- `switch`。

迁移后：

- 数据生命周期能力进入 `context.content`；
- View 替换能力进入 `context.view`；
- `context.buffers` 整体删除；
- `switch` 不得混入 Content primitives。

`BufferCommandContext` 表示 Buffer Mode command callback 的 kind-specific
context，不是 Buffer 命令空间。它与 `BufferContentContext`、
`BufferAdapterDefinition` 暂时保留，等 M4 的 ModeResolver contract 一并
评估，不在 M1 做无关重命名。

## 4. 内建插件和文档

以下调用方必须在 M1 同步迁移：

- `runtime/plugins/vim/plugin.ts` 中的 `context.buffers.*`；
- `runtime/type-tests/mode.ts` 中的 `context.buffers.*`；
- `runtime/type-tests/command-system.ts`；
- `docs/scripting.md` 中的 `newBuffer` 和 `switchBuffer` 示例；
- `docs/design/command-execution-ownership.md` 中的 provisional 示例。
- `vell-plugin-v8` 命令行测试中嵌入的 `newBuffer` 源码。

通用命令 registry 测试中的 `buffer.save`、`buffer.typed` 也改用不携带旧
领域暗示的示例名称。它们不是内建命令，但继续保留会误导搜索和读者。

Vim 的 `:new`、`:edit`、`:buffer`、`:write` 等用户命令可以保留 Vim
名称，但 handler 必须分别调用新的 Content 或 View primitives。

## 5. Rust operation 与执行路径

当前 `BufferOperation` 同时包含两类语义：

- `New`、`Open`、`List`、`Close`、`Save`、`SaveAs`、`Reload` 是 Content
  生命周期；
- `Switch` 是 View replacement。

M1 必须拆开这两类 operation，并同步迁移：

- `vell-mode/src/operation.rs` 的请求类型；
- `vell-app/src/operation.rs` 的 resolved 类型；
- `vell-app/src/runtime.rs` 的目标解析、准备和提交；
- `vell-app/src/execution.rs` 的 prepared effect；
- `vell-app/src/buffer_lifecycle.rs` 的数据与切换职责；
- `vell-plugin-v8/src/script/primitives.rs` 的 primitive 映射。

M1 可以复用当前替换单个 View 的实现，但对外必须使用 View spec。完整的
View 子树生命周期会在 M2 收口到 ViewWorkspace，M1 不提前建立只被一个
调用方使用的浅模块。

## 6. 测试迁移

以下测试类别必须按行为迁移，不能只做名称替换：

- 原生命令 registry 与 TypeScript seed declaration 一致性；
- V8 primitive 到 typed operation 的映射；
- Content create/open/list/save/reload/close 的目标解析和 rollback；
- `view.switch` 的 View spec 校验、目标解析和原子替换；
- 命令行与持久 TypeScript 环境中的调用；
- Vim 命令对 Content 与 View primitives 的正确选择。
- `crates/vell-app/src/tests.rs` 中嵌入的 TypeScript 调用与命令 ID。

新增负向契约测试：公开 registry、TypeScript declaration 和 Mode context
均不得再出现旧 Buffer 命令名称。

## 7. 明确保留的 Buffer 用法

以下名称描述真实 ContentKind 或文本实现，不属于公开 Buffer 命令：

- `vell-core::Buffer` 及其文本编辑算法；
- `ContentKind::Buffer`、`Content::Buffer` 和 `ContentViewState::Buffer`；
- Buffer Mode adapter 的 kind-specific capability；
- `buffer` 局部变量和只处理文本 Buffer 的内部函数；
- JavaScript、TypeScript vendor 代码中的 `ArrayBuffer` 或普通 buffer。

M1 不应为了消除字符串匹配而重命名这些概念。

## 8. M1 搜索门禁

M1 完成时，以下公开位置不得再出现旧调用：

```text
runtime/editor.d.ts
runtime/commands.generated.d.ts
runtime/plugins/
runtime/type-tests/
docs/scripting.md
docs/design/command-execution-ownership.md
crates/vell-app/src/native_commands.rs
crates/vell-app/src/dispatcher.rs
crates/vell-app/src/tests.rs
crates/vell-mode/src/command_registry.rs
crates/vell-plugin-v8/src/script/command_line.rs
crates/vell-plugin-v8/src/script/primitives.rs
crates/vell-plugin-v8/src/script/global_script.rs
```

内部保留项应由精确类型或路径白名单解释，不能用全仓禁止单词 `buffer` 的
粗粒度检查代替领域审查。
