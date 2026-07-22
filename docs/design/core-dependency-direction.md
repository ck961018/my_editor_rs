# Core 与 workspace 依赖方向

**状态：** 当前实现

**更新日期：** 2026-07-22

## 1. 目标

Vell 使用 workspace 的物理 crate 边界固定依赖方向。底层数据与算法不认识
具体界面、异步编排或 V8；扩展协议和具体脚本宿主分离；根二进制只负责组合。

## 2. 当前依赖图

以下只列内部 crate 的普通依赖：

```text
vell-protocol

vell-frontend  -> vell-protocol
vell-core      -> vell-protocol

vell-mode      -> vell-core
              -> vell-protocol

vell-plugin-v8 -> vell-mode
               -> vell-core
               -> vell-protocol

vell-app       -> vell-frontend
               -> vell-mode
               -> vell-core
               -> vell-protocol

vell-tui       -> vell-frontend
               -> vell-protocol

vell binary    -> vell-app
               -> vell-plugin-v8
               -> vell-tui
```

`vell-app` 只在测试依赖中使用 `vell-plugin-v8`，用于跨层脚本集成测试；
其普通依赖图不含 V8。

## 3. 各层边界

- `vell-protocol` 保存 ID、几何、Scene、输入、viewport、render query、
  status 和远程语义消息。它没有内部依赖，也不执行业务 IO。
- `vell-core` 保存封闭 Content 模型、Buffer、ContentStore、编辑计划、
  文本事务和通用输入算法。它不依赖 Mode、Tokio、Frontend 或终端。
- `vell-mode` 定义 Mode、typed adapter、state store、presentation、
  command 和 `OperationRequest`。它不知道 app 执行器和具体 VM。
- `vell-frontend` 只定义 `Frontend` trait，避免 app 与具体前端互相依赖。
- `vell-app` 拥有运行时编排、目标解析和宿主状态，不依赖 TUI 或 V8。
- `vell-plugin-v8` 把 TypeScript schema 适配为通用 Mode，不向外泄漏 V8
  类型。
- `vell-tui` 同时拥有 crossterm 封装、Taffy 布局和渲染，不依赖 app、
  core、mode 或 V8。
- 根 `vell` 二进制加载脚本 Mode，并组装 App 与 TUI。

## 4. Core 内部方向

`vell-core` 内继续保持算法与实体解耦：

```text
generic input trie <- dispatcher consumers
text motion/range  <- Buffer edit planning
Content            -> Buffer / StatusBar
ContentStore       -> Content
```

- `Keymap<A>` 和输入匹配算法不认识具体命令类型。
- `motion` 拥有纯文本运动、target 和 operator 解析。
- Buffer 调用纯算法生成编辑计划，算法不反向依赖 Buffer。
- Content 与 `ContentViewState` 按 `ContentKind` 封闭对应。
- ContentStore 只通过 Content 的静态分派管理内容，不向 app 暴露具体变体。

## 5. 验证约束

跨 crate 重构至少应确认：

- `cargo metadata --no-deps` 的内部依赖仍符合上图；
- `cargo tree -p vell-app -e normal` 不出现 V8、Taffy 或 crossterm；
- `vell-tui` 不反向依赖 app；
- `vell-plugin-v8` 的公共 API 只暴露通用 Mode 与结构化诊断；
- workspace 测试、Clippy 和 Rustdoc 通过。
