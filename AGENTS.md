# AGENTS.md

本文件供在本仓库工作的 AI 编码代理使用。优先遵守用户的直接指令；
用户未另行说明时，按本文约定执行。

## 项目概览

`Vell` 是一个 Rust 2024 终端文本编辑器。仓库由一个轻量二进制 crate、
七个内部 library crate，以及内嵌 TypeScript 插件运行时组成。

主要技术栈：

- Rust 2024，MSRV 1.88；
- `ropey` 保存文本；
- `crossterm`、`taffy` 和 `unicode-width` 实现 TUI；
- `tokio`、`futures` 和 `tokio-util` 驱动事件循环、保存与后台分析；
- `rusty_v8` 与 `deno_ast` 执行和转译 TypeScript 插件；
- `tempfile` 用于原子保存和测试。

内建 Vim 行为与 Tree-sitter 高亮位于 `runtime/plugins/`，Rust 层不按插件名
硬编码这些行为。

## 常用命令

- 格式检查：`cargo fmt --all -- --check`
- 静态检查：
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- 测试：`cargo test --workspace --all-features`
- 文档：`cargo doc --workspace --all-features --no-deps`
- TypeScript 契约：`pnpm typecheck`
- 运行：`cargo run -- <path>`

只修改文档或注释时，至少运行 `git diff --check`，并检查 Markdown 行长和
相对链接。修改 Rust、TypeScript 或插件清单时，按影响范围增加对应检查；
跨 crate API 或执行边界改动默认运行完整测试和 Clippy。

## 仓库结构

- `src/main.rs`：composition root；加载脚本 Mode，创建 TUI 和 `App`。
- `crates/vell-protocol/`：零依赖的共享 ID、Scene、输入、viewport、
  render query 和远程语义消息。
- `crates/vell-core/`：Content、Buffer、文本编辑、selection 映射、
  ContentStore 和纯输入算法。
- `crates/vell-mode/`：Mode contract、typed adapter、Mode state、
  presentation、命令与 operation 请求。
- `crates/vell-frontend/`：只定义 `Frontend` 接缝。
- `crates/vell-app/`：Kernel、ClientSession、事件循环、目标解析、
  execution frame、history、Scene 模型、保存和后台任务。
- `crates/vell-plugin-v8/`：TypeScript schema、V8 host、模块加载、
  callback 原语、Mode adapter、worker 与诊断。
- `crates/vell-tui/`：终端生命周期与 IO、Taffy 布局、viewport 和渲染。
- `runtime/editor.d.ts`：公开 TypeScript API 的唯一真相源。
- `runtime/plugins/`：内建插件及其清单、脚本和 worker。
- `runtime/examples/`：受 TypeScript 与 Rust 测试覆盖的迁移示例。
- `docs/design/`：当前实现的架构与边界说明。
- `docs/superpowers/specs/`：已确认设计的历史规格记录。
- `docs/scripting.md`：插件作者指南。
- `docs/release.md`：CI 与人工发布门槛。

## Crate 依赖边界

内部 crate 的普通依赖保持单向：

```text
vell-frontend  -> vell-protocol
vell-core      -> vell-protocol
vell-mode      -> vell-core + vell-protocol
vell-plugin-v8 -> vell-mode + vell-core + vell-protocol
vell-app       -> vell-frontend + vell-mode + vell-core + vell-protocol
vell-tui       -> vell-frontend + vell-protocol
vell binary    -> vell-app + vell-plugin-v8 + vell-tui
```

关键约束：

- `vell-protocol` 保持零内部依赖和零业务 IO。
- `vell-core` 不依赖异步运行时、Mode、Frontend、终端、布局或 V8。
- `vell-mode` 定义扩展契约，不依赖 app、Frontend、TUI 或 V8。
- `vell-app` 的普通依赖图不得包含 V8、Taffy 或 crossterm。
- `vell-tui` 不依赖 app、core、mode 或 V8；终端实现属于该 crate。
- `vell-plugin-v8` 不向公共接口泄漏 V8 类型。
- 具体 TUI、V8 Mode 与 App 的组合只在根二进制中完成。
- `App<F: Frontend>` 保持泛型静态分发；不要引入 app 层前端枚举或
  `Box<dyn Frontend>`。

## 所有权与执行约定

- `Kernel` 持有 `ContentStore`、`ModeRegistry`、共享 Mode content state、
  `TransactionManager`、保存任务和 Mode 后台任务。
- `ClientSession` 持有 Scene、View、Mode view state、输入状态、Face 与
  presentation cache。当前生产路径是一对一 `Kernel + ClientSession`。
- `ContentStore` 是唯一 Content 表。`Content` 与 `ContentViewState` 都是
  和 `ContentKind` 对齐的封闭枚举。
- `View` 只持有 `ContentId`、`ContentViewState` 和 revision；不持有 Mode、
  history、viewport 或渲染缓存。
- selection 使用 `anchor/head`；collapsed cursor 等于 primary selection
  的 `head`。不要添加冗余方向字段。
- Mode content state 按 `(ModeId, ContentId)` 共享，view state 按
  `(ModeId, ViewId)` 隔离；一个 View 可以附加多个有序 Mode。
- Native Mode 与 TypeScript Mode 共用 `vell-mode` 的 adapter、state、
  operation 和 presentation contract。
- Mode callback 只能写自身 draft，并返回有序 typed operation；不得借出
  可变 Content、View、App 或宿主对象。
- 一次输入、timeout 或显式命令对应一个 `ExecutionFrame`。失败时恢复
  Content、View、input 和 history checkpoint，并丢弃 Mode draft 与
  prepared frontend/save effects。
- `TransactionManager` 是 undo/redo 生命周期的唯一所有者；Mode state、
  viewport、focus 和布局不进入文本历史。
- `SceneBuilder` 属于 app；布局和 viewport 状态属于 TUI。
- 渲染是 fallible pull 模型。`RenderQuery` 返回 owned 数据或
  `RenderQueryError`；渲染路径不得调用 Mode、V8 或 worker。
- 异步任务只接收 owned snapshot，请求结果回到 app 后必须通过 revision、
  generation 或 slot 校验才能安装。

## 编码约定

- 用户面操作名继续使用 `Cursor*`，Buffer 内部实现使用 selection 术语。
- 几何和布局 cell 使用整数；`f32` 只出现在 Taffy adapter 边界。
- 按键协议保持 `KeyEvent { code, modifiers }` 和 `KeyModifiers`；不要把
  Ctrl、Alt 或 Shift 特化回 `KeyCode`。
- `vell_protocol::scene` 只保存 Scene 快照和只读访问；split、close、
  replace、树修复和 ID 分配属于 `vell_app::scene_model`。
- `ClientSession` 持有唯一 `SceneBuilder`。新增 Space 必须通过该
  builder 分配。
- `build_editor_scene` 只在传入的 builder 上创建标准布局并 snapshot，
  不得内部创建或消耗另一个 builder。
- 新增 ContentKind 必须同时扩展 `Content`、`ContentViewState`、
  `ContentStore` 静态分派、Mode adapter context 和 render query 配对。
- app 不得借出或匹配 `Buffer`、`StatusBar` 等具体 Content 变体。
- 不要恢复全局 `HeadlessFrontend`。app 集成测试继续使用测试模块内的
  `ScriptedFrontend`；终端字节与布局细节由 `vell-tui` 单测覆盖。
- 不要仅因未使用就删除带原因的 `#[allow(dead_code)]` 预留契约，除非任务
  明确要求清理该 API。
- 注释解释不明显的不变量、所有权或跨层原因，不复述代码。

## TypeScript 插件约定

- `runtime/editor.d.ts` 与 Rust schema 必须同步；改变任一侧时补充契约测试。
- 内建插件通过 `plugin.json` 的 `order` 排序，不能在 Rust bootstrap 中按名
  选择 Vim 或 Tree-sitter。
- v2 adapter 使用 `on.buffer` 或 `on.statusBar`；只暴露该 ContentKind 合法
  的 context 能力。
- 脚本 state 必须是 JSON-compatible owned data。V8 handle、函数、Promise、
  循环引用和宿主引用不得进入 Mode state。
- 后台分析通过命名 `analysis` 和独立 worker 运行；render query 只读取已
  发布的 Rust presentation snapshot。
- 不要绕开脚本预算、原子发布、故障隔离、路径限制或 v1 兼容诊断。

## 测试重点

- `vell-protocol`：共享数据、fallible query 与远程消息契约。
- `vell-core`：Buffer、Content、编辑计划、selection 映射和文本事务。
- `vell-mode`：adapter 能力、状态类型擦除、draft、排序和 job contract。
- `vell-app`：使用测试模块内 `ScriptedFrontend` 覆盖输入、operation、
  rollback、history、保存、Scene 和 presentation 集成。
- `vell-plugin-v8`：schema、TypeScript 转译、模块加载、预算、worker、
  UTF-16 转换、v1 迁移与故障恢复。
- `vell-tui`：终端事件、Taffy 几何、SceneRenderer、viewport 和文本 cell。
- `runtime/`：运行 `pnpm typecheck` 检查声明、内建插件和迁移示例。

## 工作流程

- 开始前用 `rg` 或 `rg --files` 定位真实 owner，避免整仓无目的阅读。
- 跨层改动先对照 `docs/design/` 和相关已确认 spec，再以当前代码为准。
- 保持改动范围窄，不做无关重构、格式化或预留 API 清理。
- 不回滚用户未提交的改动；脏工作区只处理与任务相关的文件。
- 修复 bug 时优先先加失败的回归测试，再修改实现。
- Markdown 所有行不得超过 80 个字符，并保持 LF。
- 完成时说明实际运行的命令；未运行的检查必须说明原因。
