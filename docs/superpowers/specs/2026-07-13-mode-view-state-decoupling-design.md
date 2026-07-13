# Mode 与 ContentViewState 解耦设计

**日期：** 2026-07-13  
**状态：** 已实施

## 目标

- `App` 持有共享的 `ModeRegistry`，`Buffer` 不再拥有 Mode 定义。
- 每个 `View` 持有独立 `ModeInstance`，同一 Content 的多个 View 状态互不影响。
- `View` 使用静态 `ContentViewState` 保存 Content 专属会话状态；只有文本状态拥有
  `Selections`，状态栏不再构造虚假 selection。
- 原生 Mode 与未来脚本 adapter 通过同一个 `Mode` trait 注册和实例化。

## 所有权与执行链

```text
App
├── ModeRegistry
│   └── Mode definition (共享)
├── ContentStore
└── ViewStore
    └── View
        ├── Option<ModeInstance> (每 View 独立 state)
        └── ContentViewState
            ├── Buffer(BufferViewState { selections })
            └── StatusBar
```

`ModeRegistry` 将 owned `ModeName` 解析为稳定的运行时 `ModeId`，再查找共享定义并创建实例。
`ModeInstance` 保存共享定义引用、运行时 ID 和由定义创建的不透明 state；按键解析、cursor
style 和 mode action 均从实例进入同一个 `Mode` trait。未来脚本 Mode 只需提供该 trait 的
adapter，不向 View 或协议暴露脚本对象。

按键和命令路径为：

```text
KeyEvent -> focused View.ModeInstance -> Command
Mode action -> View.ModeInstance -> ContentCommand -> Content + ContentViewState
```

Content 负责创建匹配的 `ContentViewState` 并声明默认 `ModeName`，App 不识别 Buffer、
StatusBar 等具体变体。Content/state 不匹配继续作为内部不变量失败。

## 前端数据

本项实施时，`ViewData` 暂以 `Option<Selections>` 表达是否具有文本 selection。后续
“View presentation 泛化”已将它替换为显式 Text/StatusBar presentation；Terminal/Web
仍在对应 Content 出现时定义自己的数据。

## 非目标

- 不实现 mode stack、脚本 runtime 或热重载；动态名称与运行时 ID 映射由后续设计完成。
- 语义命令边界由后续的“语义 Content 命令与适配结果设计”完成；本设计不增加 capability。
- 本项不引入独立 `ViewId` 或远程协议；后续“View 与 Space 身份分离”已完成 ViewId。
- 不把静态 `ContentViewState` 改成 `Any` 或插件对象。

## 验收

- `Buffer` 不再持有 `ModeSet`，`ContentRuntime`/`BufferRuntime` 被移除。
- App 只有一个 registry；每个 Buffer View 从它创建独立 Vim instance。
- StatusBar View 没有 ModeInstance，也没有 Selections。
- 同一 Buffer 两个 View 的 Vim 状态、selection 继续独立。
- 现有编辑、保存、渲染与 cursor style 行为保持通过。
