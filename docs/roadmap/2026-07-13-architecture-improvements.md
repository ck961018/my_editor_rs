# Architecture Improvements

**更新日期：** 2026-07-13

本路线图记录长期改进方向，不是逐步执行计划。每项工作在触发条件出现后再编写设计规格
和实施计划；不为尚未出现的需求提前搭建完整框架。

## 优先级 1：保存一致性

**触发条件：** 立即处理。  
**状态：** 已完成（2026-07-13）。

改进前，异步保存只按 ContentId 标记在途任务，完成事件没有文档 revision。保存期间继续编辑
时，旧快照完成可能错误地清除新内容的 modified 状态；重复保存请求也会被忽略。

已实现：

- Buffer 编辑时维护递增 revision；
- SaveSnapshot 和 SaveFinished 携带 revision；
- 仅当当前 revision 等于已保存 revision 时清除 modified；
- 在途保存期间记录最新的再次保存请求；
- 使用临时文件和 rename 提供原子保存，避免直接覆盖导致文件截断。

## 优先级 2：Mode 与 ContentViewState 解耦

**触发条件：** 第二种 Content 需要使用 Vim Mode，或开始接入脚本 Mode。  
**状态：** 已完成（2026-07-13）。

改进前，ModeSet 由 Buffer 持有，ModeRuntime 位于 BufferRuntime 内。这不利于同一 Mode 复用于
Terminal、WebPage 等 Content。

已实现：

- App/Kernel 持有 ModeRegistry；
- View 独立持有 ModeInstance；
- View 同时持有按 Content 类型区分的 ContentViewState；
- selection 只属于文本 View，不要求所有 Content 构造 selection；
- 原生与脚本 Mode 通过统一 ModeHost 契约执行。

暂不实现完整 Mode stack。出现需要组合的 input/major/minor mode 后，再定义优先级和
`Handled/NotHandled` 链。

## 优先级 3：语义命令与适配能力

**触发条件：** 一个 Mode 需要服务两种行为不同的 Content。  
**状态：** 待触发。

当前 Mode action 最终返回 Buffer 专属 EditCommand。后续应让 Mode 产生可由不同 Content
解释的语义命令，并让 Content 明确返回 `Handled` 或 `NotHandled`。

只有在需要提前展示“某 Content 可选哪些 Mode”时，才增加 capability 元数据。优先根据
实际支持的命令或能力匹配，不维护易失真的具体 Content 类型 allowlist。

## 优先级 4：运行时 Mode 标识

**触发条件：** 脚本可以注册 Mode 或 Mode action。  
**状态：** 待触发。

当前 `ModeId(&'static str)` 和 `ModeActionId(&'static str)` 只适合编译期定义。脚本接入时：

- 协议和脚本边界使用拥有所有权的名称；
- registry 将名称映射为稳定的运行时 ID；
- Rust `Any` 只作为原生 Mode 的内部实现；
- 脚本 Mode 状态由脚本 runtime 以 opaque handle 管理。

脚本语言、沙箱、热重载和 ABI 在选择 runtime 时另行设计。

## 优先级 5：View 与 Space 身份分离

**触发条件：** 远程协议定型、View 需要跨布局移动，或多个 session 共享 Content。  
**状态：** 待触发。

引入独立 ViewId：

```text
SpaceId   布局节点
ViewId    展示与交互会话
ContentId 共享内容
```

Scene host 引用 ViewId，View 再引用 ContentId。迁移时保证同一 Content 的多个 View 拥有
独立 Mode、selection、viewport 和其他 ContentViewState。

## 优先级 6：View presentation 泛化

**触发条件：** 增加第一个非文本、非状态栏 Content。  
**状态：** 待触发。

当前 ViewData 仍固定为可选 selections 加 cursor style，渲染器通过试探 TextLineCount 区分
文本与状态栏。后续需要：

- 使用显式 presentation kind 或 presentation enum；
- 为 Text、Terminal、Web、Status 等定义各自最小数据；
- 消除通过 Unsupported query 猜测 Content 类型的逻辑。

## 优先级 7：远程 request/response 协议

**触发条件：** 开始实现第一个进程外 Frontend。  
**状态：** 待触发。

将当前同步 RenderQuery 映射为异步消息：

- Scene/View 变更通知；
- ContentInvalidated 通知；
- 带 request ID 的 ContentRequest/ContentResponse；
- scene、view、content revision；
- 显式错误与协议能力协商；
- 可序列化的 owned 数据。

同进程 TUI 可以继续使用直接调用适配器。先实现语义协议，再选择序列化和 transport。

## 优先级 8：Session 层

**触发条件：** 需要同时连接多个 Frontend。  
**状态：** 待触发。

将共享内核状态与客户端状态分离：

- Kernel 共享 ContentStore、ModeRegistry 和后台服务；
- ClientSession 独立持有 Scene、Focus、ViewStore 和 viewport；
- 每个客户端通过自己的 transport 接收事件和查询数据。

如果远程场景始终只有一个连接，则不引入该层。

## 优先级 9：文本位置与显示位置

**触发条件：** 支持 tab 宽度、全角字符、组合字符、emoji 或软换行。  
**状态：** 待触发。

分离：

```text
TextOffset    selection 的文档位置
TextPoint     从 Buffer 派生的逻辑行列
DisplayPoint  前端计算的显示 cell/pixel 位置
```

避免长期同时缓存公开的 char_index、row 和 col。只有性能测量证明需要时，才增加带 revision
校验的位置缓存或完整 DisplayMap。

## 优先级 10：Scene 模型与 wire data 分离

**触发条件：** Scene 开始序列化或远程传输。  
**状态：** 待触发。

协议层只保留 Scene snapshot/delta 所需的数据。SceneBuilder、split、close、节点修复和焦点
回退迁移到后端模型层；TUI 继续只消费中立 Scene 数据进行布局。

## 暂不推进

- 不因未来脚本扩展而把静态 Content enum 改成通用插件对象；
- 不在第二个 Mode 出现前实现 Mode stack；
- 不在远程前端启动前选择网络协议和序列化库；
- 不在多客户端需求出现前拆 ClientSession；
- 不在性能测量前实现增量 Taffy tree、渲染缓存或零拷贝查询。
