# AGENTS.md

本文件给在本仓库工作的 AI 编码代理使用。请优先遵守用户的直接指令；
当用户没有另行说明时，按本文约定执行。

## 项目概览

`Vell` 是一个 Rust 2024 终端文本编辑器，二进制 crate。
当前核心能力包括打开文件、基本文本编辑、光标移动、`Shift+Arrow`
扩展 selection、输入替换 selection、`Ctrl+S` 异步保存、`Ctrl+Q`
退出、Emacs 风格键映射、终端 Resize 重绘，以及文本区 + 状态栏 +
光标定位 + selection 高亮的 TUI 渲染。

技术栈：

- Rust 2024，MSRV 1.88。
- `ropey` 负责文本缓冲区。
- `crossterm` 负责终端 IO、原始模式、VT 序列和事件流。
- `tokio` + `futures` 负责异步主循环和后台保存。
- `taffy` 负责 Flex 布局。
- `tempfile` 用于生产路径的原子保存临时文件，也用于测试。

## 常用命令

- 格式化：`cargo fmt`
- 静态检查：`cargo clippy --all-targets --all-features`
- 测试：`cargo test`
- 运行：`cargo run -- <path>`

如果只做文档或注释修改，至少运行 `git diff --check` 检查空白问题。
如果修改 Rust 代码，默认运行 `cargo test`；涉及 API、类型或跨层边界时，
同时运行 `cargo clippy --all-targets --all-features`。

## 目录结构

- `src/main.rs`：程序接线，创建终端 guard、前端和 `App`。
- `src/core/`：编辑模型和纯逻辑层。不要引入终端、布局或异步依赖。
- `src/protocol/`：前后端共享协议和中立数据类型。保持低耦合、零业务 IO。
- `src/terminal/`：`crossterm` 封装，包含输入、输出和终端生命周期。
- `src/frontend/`：纯抽象层，只放 `Frontend` trait，`app` 与 `tui` 共同依赖。
- `src/tui/`：前端布局与渲染层，拥有 Taffy 布局、viewport 跟随逻辑和
  `TuiFrontend<W>`。
- `src/app/`：主循环、事件分发、Scene 模型、操作执行、内容表、View 归属和后台保存。
- `docs/roadmap/`：用户维护的长期改进方向和后续计划，不使用 Superpowers
  执行计划格式。
- `docs/superpowers/specs/`：设计规格。
- `docs/superpowers/plans/`：执行计划。

## 架构边界

依赖方向应保持单向：

```text
frontend -> protocol
app      -> frontend + core + protocol
tui      -> frontend + terminal + protocol
main     -> app + tui + terminal
terminal -> protocol
core     -> protocol/std
protocol -> std
```

关键约束：

- `frontend` 是纯抽象层，只放 `Frontend` trait 等前端行为接缝。
- `App<F: Frontend>` 使用泛型静态分发；不要重新引入 `FrontendImpl`、
  `Box<dyn Frontend>` 或 app 层前端枚举。
- `tui` 不得依赖 `app`；`app` 不得依赖 `tui`。具体接线只在 `main.rs`。
- 不要恢复全局 `HeadlessFrontend`。app 集成测试使用测试模块内的
  `ScriptedFrontend`，渲染字节细节由 `tui::scene_renderer` 单测覆盖。
- `core` 只能表达编辑领域逻辑；不要让它依赖 `crossterm`、`taffy`、
  `tokio` 或前端渲染概念。
- `protocol` 是中立层，适合放 ID、几何、selection、scene、
  viewport、key event、status 和 `ContentQuery` 等共享契约。
- `protocol::scene` 只保存 Scene 快照数据和只读访问；`SceneBuilder`、split、
  close、树修复和模型错误属于 `app::scene_model`。
- 布局所有权在 `tui` 层。不要把状态栏预留高度、viewport 高度等
  布局知识塞回 `core` 或 `protocol::Viewport`。
- 渲染是 pull 模型：前端通过 `ContentQuery` 查询内容、状态栏数据和
  selections；后端不要主动 push frame 或渲染数据。
- `ClientSession` 持有唯一 `SceneBuilder`。新增 space 必须通过该 builder 分配。
- `build_editor_scene` 只负责在传入的 `SceneBuilder` 上创建标准布局并
  snapshot，不得内部新建 builder 或消耗 builder。
- 按键协议使用 `KeyEvent { code, modifiers }`，通过 `KeyModifiers`
  表达 Ctrl/Alt/Shift；不要重新引入 `CtrlKey`、`KeyEvent::Ctrl` 或把
  修饰键特化进 `KeyCode` 的旧枚举。
- `View` 是 `app` 层编辑会话实体，按 `ViewId` 归属 selections。
  不要把 View 迁到 `core` 或 `protocol`，除非设计文档明确要求。
- selection 模型使用 `anchor/head`，方向由二者相对位置隐含；不要新增
  `direction` 字段。
- collapsed selection 的 cursor 等价于 primary selection 的 `head`。
  真 selection 由 `anchor != head` 表示；新增多光标行为前，应先写设计或
  更新现有 spec。

## 编码约定

- 优先保持现有模块边界和命名。用户面操作名仍使用 `Cursor*`，
  Buffer 内部实现面使用 `selection` 术语。
- 几何和布局 cell 单位使用整数；`f32` 只应出现在 Taffy adapter 边界。
- `Content` 是静态闭合的内容集合；新增内容类型必须扩展 `Content` 枚举和 `ContentStore` 分派。
- `ContentStore` 是唯一内容表；app 不得借出或识别 `Buffer`、`StatusBar` 等具体内容类型。
- 内容执行通过 `Content::execute(ContentInput)`，渲染数据通过 `ContentStore::query`；
  不要向 Content 加入渲染方法。
- `SceneRenderer` 负责布局、viewport 跟随、pull 可见行和画布输出。
- app 测试用局部 `ScriptedFrontend`（impl `Frontend`，事件回放 + render
  计数）驱动集成流程；修改 `Frontend` trait 或 `SceneRenderer` 时要同步
  考虑 `ScriptedFrontend` 与 `SceneRenderer` 单测。
- 仓库中存在一些为后续功能预留的 `#[allow(dead_code)]` 类型和变体。
  不要仅因未使用就删除，除非任务明确要求清理预留 API。
- 注释应解释不明显的约束或跨层原因，避免复述代码。
- 修改或新增 Markdown（`*.md`）文件时，所有行均不得超过 80 列。

## 测试重点

修改不同层时优先补充或调整对应测试：

- `core`：buffer 编辑、selection collapsed 守恒、打开/保存、keymap、
  operation、status bar。
- `protocol`：scene、space、geometry、selection、viewport、key event、
  content query、ids、status。
- `terminal`：输入事件翻译、输出 VT 序列和 Canvas 分派。
- `tui`：Taffy 几何、DFS order、SceneRenderer 渲染和 viewport 跟随。
- `app`：通过局部 `ScriptedFrontend`（impl `Frontend`，事件回放）覆盖
  输入到状态再到渲染的集成流程；渲染字节细节由 `SceneRenderer` 单测覆盖。

## 工作流程

- 开始改动前先用 `rg`/`rg --files` 定位相关文件，避免整仓无目的阅读。
- 对跨层改动，先对照 `docs/superpowers/specs/` 中最新相关设计和
  `docs/superpowers/plans/` 中对应执行计划。
- 保持改动范围窄，不做与任务无关的重构、格式化或 API 清理。
- 不要回滚用户未提交的改动。遇到脏工作区时，只处理与任务相关的文件。
- 新增行为应优先有测试；修复 bug 时先写能失败的回归测试，再改实现。
- 完成前说明实际运行过的验证命令；无法运行时说明原因。
