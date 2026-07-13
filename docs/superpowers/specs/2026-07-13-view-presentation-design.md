# View presentation 泛化设计

**日期：** 2026-07-13  
**状态：** 已实施

## 目标

- `ViewData` 显式声明 presentation，前端不再通过试探 Content query 猜测内容类型。
- 当前实际存在的 Text 与 StatusBar 使用不同的数据形状；文本 presentation 必须携带
  selections 与 cursor style，状态栏不再携带虚假的文本字段。
- TUI 根据 presentation 选择渲染路径，并只发送该路径允许的 Content query。

## 协议

```text
ViewData
├── content: ContentId
└── presentation
    ├── Text { selections, cursor_style }
    └── StatusBar
```

`ContentQuery` 继续负责按需拉取共享内容。Text 只请求可见的 `TextRows`；StatusBar 只请求
`StatusBarData`。预期 query 返回错误的数据变体属于同进程
协议违反，应立即失败，而不是回退到另一种 presentation。

Terminal 与 Web 在对应 Content 出现时添加自己的 presentation 变体和最小数据。当前没有
可验证的终端网格、Web surface 或事件契约，因此本项不预先定义空壳字段。

## 所有权

App 的 RenderQuery adapter 从 View 的实际会话状态构造 owned presentation 数据。Core
仍只表达内容与编辑领域，不新增 TUI/GUI 渲染方法；ContentStore 继续是 Content query 的
唯一分派入口。

## 非目标

- 不实现 Terminal/Web Content 或通用组件树。
- 不增加序列化、远程 request/response 或 capability negotiation。
- 不改变 Scene、ViewId、ContentId 与 viewport 的所有权。

## 验收

- `ViewData` 不再含可选 selections 或顶层 cursor style。
- `TextLineCount` 探测 query 被移除，TUI 不再通过 Unsupported 区分文本与状态栏。
- StatusBar View 不发送文本 query，Text View 不发送状态栏 query。
- 现有文本、selection、cursor style、viewport 与状态栏测试通过。
- fmt、test、clippy 与 diff 检查通过。
