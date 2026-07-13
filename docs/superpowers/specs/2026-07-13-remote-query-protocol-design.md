# 远程 request/response 语义协议设计

**日期：** 2026-07-13
**状态：** 已实施

## 目标

- 将同步 `RenderQuery` 的 View/Content 查询表达为不借用本地对象的 owned 消息。
- 所有请求携带 `RequestId`，响应回显该 ID，并携带对应对象 revision。
- 定义 Scene/View/Content 变更通知、显式错误、协议版本与 capability 协商。
- 同进程 TUI 保持直接调用；本项不选择 transport 或序列化库。

## 消息

```text
ClientMessage
├── Hello { version, capabilities }
└── Request { id, View | Content }

ServerMessage
├── Welcome { version, capabilities }
├── Response { id, Result<ViewData | ContentData, ProtocolError> }
└── Notification
    ├── SceneChanged { revision }
    ├── ViewChanged { view, revision }
    └── ContentInvalidated { content, revision }
```

View request 返回 `ViewId + revision + ViewData`。Content request 返回
`ContentId + revision + ContentData`。`ContentData::Unsupported` 不跨远程边界，转换为显式
`UnsupportedQuery` 错误。未知 ID、主版本不兼容和缺失 capability 也使用结构化错误码。

## Revision

- Scene revision 在 resize 与成功的布局变更后单调递增。
- View revision 在该 View 的 mode 或会话状态经过命令路径后单调递增。
- ContentStore 为任何成功处理的 Content 输入维护单调 revision；StatusBar 的 revision
  同时反映目标文档 revision，避免派生状态静默过期。

revision 是失效检测标记，不承诺每次递增都对应用户可见差异；允许保守失效。

## 协商

协议主版本必须一致；服务端选择双方较低的 minor。协商结果只包含双方都声明的
capability。当前 capability 为 ViewQuery、ContentQuery、ChangeNotifications 与 Revisions。

## 边界

本项提供语义消息和 App 查询适配器，不提供 socket、进程管理、serde 格式或消息队列。
SceneChanged 暂只携带 revision；可传输的 Scene snapshot/delta 在 roadmap 第十项定义。

## 验收

- owned request/response 能查询 View 与 Content，并保留 request ID 和 revision。
- 未知对象与 Unsupported query 返回显式错误。
- capability 协商拒绝不兼容主版本，只返回交集。
- Scene、View 与 Content revision 在对应状态变化后递增。
- 现有本地 TUI 行为不变，fmt、test、clippy 与 diff 检查通过。
