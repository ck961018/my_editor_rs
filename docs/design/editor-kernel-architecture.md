# Editor Kernel Architecture

**状态：** 架构方向  
**更新日期：** 2026-07-13

## 1. 定位

`my_editor_rs` 的长期目标不是把业务逻辑绑定在某一种终端界面上，而是形成一个可被
不同前端驱动的编辑器内核：

```text
编辑器内核 + 可扩展 Mode + 多种 Content + 本地或远程 Frontend
```

TUI 是当前的第一个前端实现。未来的 GUI 或远程前端应复用同一套内容、视图、命令和
任务语义，而不要求内核了解终端单元格、GUI 控件或传输实现。

## 2. 核心概念

### 2.1 Content

Content 是可被展示和操作的共享实体。当前包含文本 `Buffer` 和 `StatusBar`，未来可扩展
为 terminal、选择栏、帮助页或 web 页面等内建类型。

Content 负责：

- 保存自身的共享状态；
- 回答只读 `ContentQuery`；
- 执行语义命令或处理异步事件；
- 返回由 App 解释的 effect，而不直接执行前端 IO。

Content 不拥有某一次展示的 selection、viewport、焦点或 Mode 实例。同一 Content 可以
同时被多个 View 展示。

Content 当前采用静态闭合的枚举集合。只有在确实需要由脚本注册新 Content 类型时，才
重新评估动态注册机制；Mode 的可脚本扩展不要求 Content 同时动态化。

### 2.2 View

View 是某个 Content 的一次展示和交互会话。它至少绑定一个 Content，并持有该会话独立
的状态，例如 Mode 实例、文本 selection 或 terminal/web 的局部交互状态。

长期概念模型为：

```text
View
├── ContentId
├── ModeInstance
├── ContentViewState
└── presentation state
```

`ContentViewState` 是 Content 相关的，不要求所有 View 都具有文本 selection。Buffer View
可以有 selections，StatusBar 可以没有局部状态，Terminal 和 Web View 可以定义各自的
会话状态。

前端读取的 `ViewData` 使用显式 `ViewPresentation`。当前 `Text` presentation 携带 selections
与 cursor style，`StatusBar` 没有文本字段；前端只按该枚举发送对应的 Content query，
不通过 `Unsupported` 响应或 `TextLineCount` 探测 Content 类型。Terminal/Web 在实际加入时
扩展自己的 presentation 数据。

### 2.3 Mode

Mode 是可复用的输入和交互策略。Vim Mode 是当前实现，但不是内核中的唯一固定模式。
后续 Mode 可以由脚本语言注册，并可应用于一个或多个具有相应能力的 Content。

Mode 定义和 Mode 实例必须区分：

- Mode 定义由 Mode registry 管理，可被多个 View 复用；
- Mode 实例属于 View，保存该 View 独立的运行时状态；
- 原生 Rust Mode 和脚本 Mode 可以有不同的内部状态表示，但通过同一 host 契约工作；
- 脚本对象和 Rust `Any` 都属于后端内部实现，不进入前端协议。

Mode 不应以 Buffer 专属的编辑函数作为最终扩展契约。目标分发链是：

```text
KeyEvent -> Mode -> semantic command -> target Content -> ContentEffect
```

Content 可以返回 `Handled` 或 `NotHandled`。只有在需要提前筛选可用 Mode 时，才增加
capability 元数据；相比按具体 Content 类型维护 allowlist，能力或命令契约对新 Content
更稳定。

“Mode”未来可能包含不同维度：跨 Content 的输入模式、与 Content 语义相关的 major mode，
以及可叠加的 minor mode。出现第二个需要组合的 Mode 之前，不提前实现完整 Mode stack；
扩展契约应保留 `NotHandled`，使后续组合无需推翻命令模型。

### 2.4 Space 与 Scene

Space 是布局身份，View 是交互会话身份，Content 是共享资源身份。这三个概念不能在外部
协议中合并：

```text
Scene: SpaceId -> ViewId
View:  ViewId  -> ContentId + ModeInstance + view state
```

当前实现已使用独立 `ViewId`：Scene leaf 只引用 View，App 的 ViewStore 按 ViewId 索引，
View 再引用 ContentId。同一 View 同时只能被一个 Scene leaf 引用；同一 Content 可以创建
多个拥有独立 Mode、selection 和 viewport 的 View。

Scene 的协议表示应是可序列化的只读快照或变更消息。SceneBuilder、split、close 和焦点
修复属于后端模型行为，不应成为远程 wire protocol 的一部分。

## 3. 所有权

目标所有权关系如下：

```text
Kernel / App
├── ContentStore          共享 Content
├── ModeRegistry          原生与脚本 Mode 定义
├── TaskManager           保存等后台任务
└── ClientSession
    ├── Scene
    ├── Focus
    └── ViewStore
        └── View          Mode 实例 + ContentViewState
```

当前只有一个 Frontend，因此 App 可以直接持有一份 Scene、Focus 和 ViewStore。如果未来
允许多个前端同时连接，共享 Content 留在 Kernel，每个客户端的布局、焦点、View 和
viewport 下沉到独立 `ClientSession`。

## 4. 前后端边界

后端只依赖中立协议：

- 接收按键、Resize、退出等 FrontendEvent；
- 维护 Content、View、Mode、Scene 和后台任务；
- 提供 Content 和 View 的只读数据；
- 不依赖 crossterm、Taffy、GUI toolkit 或具体网络传输。

前端只依赖协议：

- 根据 Scene 进行布局和绘制；
- 按 ContentId/ViewId 查询所需数据；
- 持有与具体呈现相关的状态；
- 不借出或识别 Buffer、StatusBar 等后端具体类型。

当前 `Frontend::render(&Scene, &dyn RenderQuery, ...)` 是同进程适配器，不是最终远程协议。
它保留了 pull 模型和 owned query result，为以后迁移到异步 request/response 提供接缝。

## 5. 远程协议方向

远程前端仍可使用 pull 模型，但同步借用必须改成消息：

```text
Backend -> Frontend
  SceneChanged
  ViewChanged
  ContentInvalidated

Frontend -> Backend
  ContentRequest { request_id, content_id, query }

Backend -> Frontend
  ContentResponse { request_id, revision, data }
```

协议定型时需要 request ID、对象 revision、显式错误和 capability negotiation。传输格式、
序列化库、增量帧算法和断线恢复在真正实现远程前端前保持未定。

## 6. 不变量

- Content 共享状态与 View 会话状态分离。
- Mode 定义可共享，Mode 实例按 View 隔离。
- Mode 产生语义命令，不直接执行前端 IO。
- SpaceId、ViewId、ContentId 表达不同身份。
- 前端不依赖后端具体 Content 类型；后端不依赖具体前端。
- 协议只包含可传递的数据和消息，不包含本地对象借用或后端 builder。
- 异步结果必须携带足够的对象身份和 revision，不能覆盖更新后的状态。

## 7. 当前阶段的有意简化

- 只有一个 Frontend 和一份客户端会话状态。
- App 直接持有一份 Scene、Focus 和 ViewStore，尚未拆出 ClientSession。
- App 持有共享 ModeRegistry，View 从中创建独立 ModeInstance。
- Mode/Action 在命令边界使用 owned 名称，Registry 将其解析为进程内稳定的数值 ID。
- 原生 Mode 暂时使用 Rust trait object 和 `Any` 保存类型状态。
- `RenderQuery` 暂时是同步同进程调用。
- Content 集合暂时是静态枚举。

这些简化有明确升级触发条件，不因远期设想而提前引入脚本 runtime、网络 transport、
多客户端调度或通用插件 ABI。
