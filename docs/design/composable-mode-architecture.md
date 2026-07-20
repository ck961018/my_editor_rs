# 可组合 Mode 架构设计

**状态：** 已确认，基础架构已实现
**更新日期：** 2026-07-20

当前默认 Mode 已全部迁移为 TypeScript 插件。Rust 只持有统一 Mode contract、
registry、state store、effect executor、后台任务和 presentation cache；不注册或
识别任何具体编辑模式、解析器或语言。

## 1. 文档定位

本文定义下一阶段的统一、可组合 Mode 架构。

当前实现已从 `mode-action-ownership.md` 中互斥的 `ContentMode` 和
`ViewMode` 模型迁移到本文架构。本文取代其中与 Mode 类型二选一、单一
effective Mode binding 和 presentation 独占相关的规则。

本文不改变已实现的 Content、View、Command 和历史事务所有权：

- Content 继续只保存领域数据，不保存或执行 Mode；
- View 继续只保存会话数据，不拥有 Mode instance；
- app 继续负责目标解析、验证和有序执行；
- core 继续不知道具体 Mode、脚本运行时和渲染；
- frontend 和 TUI 继续通过 protocol pull 呈现数据。

## 2. 目标

- 一个 Mode 同时定义 content state、view state 和两类 action；
- 同一 View 可以按明确顺序附加多个 Mode；
- Mode 可以处理输入后停止，也可以处理后继续传递；
- 一个 Mode 的 content state 由同一 Content 的所有 View 共享；
- 每个 View 拥有独立的 Mode view state 和输入序列状态；
- Mode 间通过上层命令执行器调用，不直接引用彼此；
- native Mode 与未来 script Mode 使用同一后端协议；
- Mode 可以贡献颜色、高亮、光标和 selection 呈现策略；
- 保持渲染 pull 模型和现有依赖方向；
- 将输入原子执行与 undo/redo 历史事务明确分离。

## 3. 非目标

第一阶段不包含：

- 具体脚本语言或 VM 的选择；
- 自动迁移热重载前后的脚本状态；
- 跨 Content 原子命令；
- 跨进程 Mode state；
- virtual text、gutter、浮窗和 widget 系统；
- 可产生宿主 effect 的任意异步 callback；
- Mode state 的 undo/redo 历史；
- 同一 Mode 在同一 View 中附加多次。

## 4. 核心概念

Mode 是一个逻辑定义，包含两种状态作用域和两种 action 作用域：

```text
ModeDefinition
├── ContentState: 每个 (ModeId, ContentId) 一份
├── ViewState:    每个 (ModeId, ViewId) 一份
├── ContentAction
├── ViewAction
├── InputHandler
├── ContentChange callbacks
└── Presentation queries
```

`ContentState` 和 `ViewState` 是同一个 Mode 的状态，不是两种 Mode。
不需要某种状态的 Mode 使用空状态，不通过类型分类表达。

术语约定：

- `ModeDefinition`：native 实现或 script adapter 提供的行为定义；
- `ModeContentState`：Mode 在一个 Content 上的共享私有数据；
- `ModeViewState`：Mode 在一个 View 上的局部私有数据；
- `ModeAttachment`：Mode 与 View 的启用关系；
- `ModeChain`：一个 View 上有序的 `ModeId` 列表；
- `ExecutionFrame`：一次物理输入、timeout 或显式命令的原子执行范围；
- `HistoryTransaction`：用于 undo/redo 分组的编辑历史事务。

不引入 `ModeInstanceId`。`(ModeId, ContentId)` 和
`(ModeId, ViewId)` 已经是稳定、充分的状态 identity。

## 5. 所有权

持久状态按共享范围分布：

```text
Kernel
├── ContentStore
├── ModeRegistry
├── ModeContentStore
└── HistoryTransactionManager

ClientSession
├── Scene
├── Views
├── ModeViewStore
├── Mode chains
├── Global input state
├── FaceRegistry
└── PresentationLayerStore

App runtime
└── ExecutionFrame
```

建议的数据结构为：

```rust
struct ModeContentStore {
    entries: HashMap<(ModeId, ContentId), ModeContentEntry>,
}

struct ModeViewStore {
    chains: HashMap<ViewId, Vec<ModeId>>,
    entries: HashMap<(ModeId, ViewId), ModeViewEntry>,
}

struct ModeContentEntry {
    state: Box<dyn ModeState>,
    attachment_count: usize,
    revision: Revision,
    fault: Option<ModeFault>,
}

struct ModeViewEntry {
    state: Box<dyn ModeState>,
    input: ModeInputState,
    revision: Revision,
    fault: Option<ModeFault>,
}
```

不建立同时拥有 Kernel 和 ClientSession 数据的大型 `ModeManager`。
app runtime 通过已有顶层所有权协调两个 Store。

## 6. Mode 定义

后端只注册一种 Mode：

```rust
trait Mode {
    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError>;

    fn create_view_state(
        &self,
        content_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError>;

    fn handle_input(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Result<ModeResult, ModeError>;

    fn execute_content_action(
        &self,
        content_state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        action: ModeActionId,
        arguments: ModeValue,
    ) -> Result<ModeResult, ModeError>;

    fn execute_view_action(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        action: ModeActionId,
        arguments: ModeValue,
    ) -> Result<ModeResult, ModeError>;
}
```

具体 Rust 签名可以在实现中按借用约束收敛，但必须保持以下能力边界：

- content action 没有 View identity 或 View mutation 能力；
- view action 可以读取本 View 和绑定 Content；
- view action 可以修改本 Mode 的两种私有状态；
- 两种 action 都不能直接借出可变 Content 或 View；
- 宿主状态变更必须通过 typed effect 返回；
- Context 只在 callback 期间有效，Mode 不得保存其中引用。

每个 action 注册唯一名称和明确 scope：

```rust
enum ModeActionScope {
    Content,
    View,
}
```

同一 Mode 中 action name 必须唯一，不能依赖调用环境消除歧义。

## 7. 有序结果与 effect

`ContentModeResult` 和 `ViewModeResult` 合并为：

```rust
struct ModeResult {
    flow: InputFlow,
    effects: Vec<ModeEffect>,
}

enum InputFlow {
    Continue,
    Stop,
}

enum ModeEffect {
    Edit(EditCommand),
    Content(ContentAction),
    View(ViewAction),
    Transaction(TransactionIntent),
    Command(ModeCommand),
    App(AppCommand),
    Viewport(ViewportCommand),
    Save,
}
```

语义为：

```text
Pass       = Continue + 空 effects
Consumed   = Stop     + 空 effects
处理并继续 = Continue + effects
处理并停止 = Stop     + effects
```

不保留 `Noop` operation。空 effect 列表已经表达 no-op。

ModeEffect 是意图，不是预先应用的 mutation。`EditCommand` 必须在轮到它
执行时，基于当时的 selections 生成 edit plan。不得在 Mode callback 中提前
保存跨 effect 的 `ResolvedViewEdit`。

effect 严格按列表顺序执行。`ModeEffect::Command` 同步、深度优先地重新进入
同一个执行器，随后才继续执行原列表中的下一个 effect。

## 8. Mode 间命令

Mode 不保存或调用其他 Mode 对象。跨 Mode 调用使用：

```rust
struct ModeCommand {
    mode: ModeName,
    action: ModeActionName,
    arguments: ModeValue,
}
```

`ModeValue` 是 owned、语言无关、可 checkpoint 的数据：

```rust
enum ModeValue {
    Null,
    Bool(bool),
    Integer(i64),
    String(String),
    List(Vec<ModeValue>),
    Map(HashMap<String, ModeValue>),
}
```

脚本 VM 对象和宿主引用不得直接跨 Mode 传递。命令进入执行器时立即把名字
解析为 `ModeId` 和 `ModeActionId`。目标 Mode 必须已附加到来源 View；
content action 的目标 Content 由来源 View 推导。

嵌套 action 和 operation 共用一个有界执行预算。超出预算时当前
`ExecutionFrame` 失败。

## 9. 输入顺序

每次物理输入开始时冻结来源 View 的 `ModeChain`：

```text
KeyEvent
-> Mode 1
-> Mode 2
-> ...
-> Global keymap
-> ignore
```

每个 Mode 返回结果后，app 必须先执行其 effects，再根据 `InputFlow` 决定
是否调用下一个 Mode。后续 Mode 因此能读取前一个 Mode 已成功产生的状态。

执行期间：

- 已移除的 Mode 到达时跳过；
- 新增或重排的 Mode 从下一次物理输入开始生效；
- 输入重放继续使用同一份 chain 快照；
- 跨 ModeCommand 直接调用目标，不改变输入遍历位置。

Global keymap 位于所有 Mode 之后。Mode 可以覆盖全局快捷键。Content 没有
按键 fallback，也永远不接收原始 `KeyEvent`。普通文本输入由 `plain-edit`、
Vim 或其他 Mode 提供。

## 10. 每个 Mode 的输入状态

每个 `(ModeId, ViewId)` 拥有独立 `ModeInputState`，用于 key sequence、
deadline 和最长完整匹配。多个 Mode 可以同时处于 pending 状态。

下一次输入仍从 chain 开头处理。pending Mode 不隐式提升优先级。需要独占
后续输入的 Mode 必须在进入 pending 时返回 `Stop`。

Mode 内部处理顺序为：

```text
custom capture
-> Mode keymap sequence
-> typing fallback
-> ModeResult
```

公共 keymap helper 负责序列重放：

- 有最长完整前缀时，执行该 action 并重放剩余键；
- 没有完整前缀时，清除 pending 并重放最后一个未匹配键；
- timeout 执行最长完整匹配，否则取消 pending；
- 重放在当前执行帧内从 chain 开头开始；
- 重放前必须清除对应 pending，避免同一序列循环。

第一阶段不向自定义 script capture 暴露任意 replay API。

## 11. 输入执行帧

一次物理输入及其 Mode chain、operation、重放和嵌套命令属于同一个
`ExecutionFrame`。

失败时恢复本次输入修改过的：

```text
Content(ContentId)
Selections(ContentId 下的全部 View)
HistoryTransactionManager 的本次增量
```

Content、selections、history 和 input 在第一次变更前 lazy checkpoint。
Mode callback 第一次写入时建立 content/view state draft；同一 frame 的后续
callback 读取最新 draft，成功时一次提交，失败时直接丢弃。Mode state 不进入
undo/redo 历史。

Save、Quit 和 frontend viewport mutation 等不可逆或外部 effect 延迟到执行帧
成功后发布。

## 12. 与历史事务的边界

`ExecutionFrame` 不是 `HistoryTransaction`。

历史事务语义保持不变：

- `Begin` 后持续累积编辑；
- 只有显式 `Commit` 才发布到 undo 栈；
- 输入失败不自动 commit、关闭或 rollback 整个活动历史事务；
- 已提交历史记录不会因为后续 Mode 失败而删除；
- undo/redo 不恢复当前 Mode runtime state。

如果执行帧开始前活动历史事务包含 A，本次输入追加 B 后失败，则撤销 B，
活动事务仍包含 A 并保持打开。这是清除执行帧增量，不是调用历史事务的
`Rollback`。

本次输入产生的 `Commit` 请求在执行帧成功后才发布。因此真正完成的历史
commit 不需要被执行帧撤回。

## 13. 附加和生命周期

Mode 附加到 View 时：

```text
确保 (ModeId, ContentId) state 存在
-> 创建 (ModeId, ViewId) state
-> 创建 ModeInputState
-> 加入 View 的有序 ModeChain
```

规则为：

- 首个 attachment 创建共享 content state；
- 后续 View 复用共享 content state；
- Split 继承 Mode 顺序，但创建全新的 view state；
- 关闭 View 只删除对应 view state 和 input state；
- 最后一个 attachment 消失时删除共享 content state；
- View 切换 Content 时保留 Mode 顺序，重新创建 view state；
- 同一 Mode 在同一 View 中最多附加一次。

View 创建改为两阶段：先建立基础 View 和全部 Mode state，全部成功后再发布
到 Scene。layout 层不实例化 Mode。

不增加可产生宿主 effect 的通用 `on_attach` 或 `on_detach`。初始化放在
`create_*_state`，资源清理由 state drop 或脚本运行时负责。

## 14. Content 变化通知

Mode 提供两个只修改私有状态的 lifecycle callback：

```text
on_content_changed(ModeContentState, change)
on_view_content_changed(ModeContentState, ModeViewState, change)
```

它们不能返回 `ModeEffect`，避免隐式内容变化循环。

任何 edit、undo、redo 和未来 reload 产生的结构化 ContentChange 都走：

```text
应用 ContentAction
-> 变换所有相关 View selections
-> 每个 (ModeId, ContentId) 通知一次
-> 每个 (ModeId, ViewId) 通知一次
-> 继续下一个 effect 或 Mode
```

保存状态和消息等非结构化变化不触发文本分析更新。

未来脚本 contract 使用稳定的 `ModeContentChange`，不直接暴露当前内部
`TextChangeSet`：

```rust
struct ModeContentChange {
    before_revision: Revision,
    after_revision: Revision,
    change: ModeContentChangeKind,
}

enum ModeContentChangeKind {
    Text(TextDelta),
    Reset,
}
```

`TextDelta` 使用编辑器中立坐标描述旧范围、新范围和插入文本。Tree-sitter
adapter 负责转换为 parser 所需的 byte offset 和 point。

## 15. Fault isolation

错误分为主动执行错误和被动观察错误：

- input handler 或显式 action 失败时，整个执行帧失败并恢复；
- `on_content_changed` 等被动通知失败时，不撤销用户文本；
- 被动通知失败只恢复该 callback 的 Mode state；
- content callback 失败标记共享 entry faulted；
- view callback 失败只标记对应 View entry faulted；
- faulted attachment 暂停输入和呈现查询；
- 重新附加或 reload 会清除 fault。

语法高亮、分析器或脚本缓存错误不得阻止基本文本编辑。

## 16. State checkpoint

Mode state 必须支持 checkpoint 和 restore，但不要求所有实现使用普通深拷贝：

```rust
trait ModeState {
    fn checkpoint(&self) -> Box<dyn ModeCheckpoint>;
    fn restore(&mut self, checkpoint: Box<dyn ModeCheckpoint>);
}
```

允许的实现包括：

- native 小状态直接 Clone；
- Tree-sitter Tree 使用低成本 clone；
- script data 使用结构化复制；
- 未来需要时使用 copy-on-write 或 mutation journal。

脚本 Mode state 只能保存可 checkpoint 的纯数据。文件句柄、socket、协程和
宿主引用由脚本运行时作为外部资源管理。

## 17. 同步与异步

input、action、content change、view policy 和 decoration callback 必须同步
完成。Mode 不得在 callback 中 await 或保存 Context。

Mode 可以从 content state 产生 owned 后台任务。任务只读取不可变快照，
不得保存 Context，也不得直接修改 Content、View 或 Mode state。

```text
content change 同步更新 Mode 私有状态
-> 当前执行帧成功结束
-> Kernel 启动或合并 Mode 后台任务
-> 后台完成并投递带版本的 AppMessage
-> 主循环调用所属 Mode 安装结果
-> 相关 View touch 并重绘
```

后台任务按 `(ModeId, ContentId, slot)` 标识。同一个 slot 最多运行一个任务；
运行期间的新请求只保留最新版本。CPU 密集任务使用 blocking worker，不能阻塞
Tokio 主循环。

完成消息携带 Mode generation 和 Content revision。Mode 负责拒绝过期结果。
完成回调只能更新所属 Mode 的 content state 并请求重绘，不能发出编辑、View
操作或任意 command。需要修改内容的分析器操作必须由显式命令读取当前快照，
再进入正常事务路径。

后台失败保留上一份有效状态，不回滚用户文本，也不进入 undo/redo 历史。

Tree-sitter Mode 使用共享、只读的语言注册表保存 grammar、highlight query、
injection query、文件扩展名和注入别名。宿主语言和嵌入语言使用同一份配置，
注入解析由后台任务递归完成；未知语言只跳过对应注入层，不影响宿主语法树。

content state 保存增量宿主语法树和按字节排序的高亮快照。高亮快照使用共享
所有权参与 execution frame checkpoint，View 查询只二分定位并复制可见范围的
decoration。App 只附加通用 Tree-sitter Mode，不按具体语言分支。
文本 revision 变化时立即丢弃旧高亮快照，只安装同 revision 的后台结果，避免
将旧快照的字节区间用于新文本。

## 18. 呈现策略

Mode 呈现分为独占 policy 和可叠加 decoration。

独占 policy 为：

```rust
struct ModeViewPolicy {
    cursor_style: Option<CursorStyle>,
    cursor_domain: Option<CursorDomain>,
    selection_shape: Option<SelectionShape>,
    selection_face: Option<FaceName>,
}
```

按 ModeChain 顺序取每个字段的第一个 `Some`。没有 Mode 声明时使用宿主
默认值。`cursor_domain` 在执行帧结束前用于统一校正 selections。

可叠加文本装饰为：

```rust
struct TextDecoration {
    range: TextRange,
    face: FaceName,
}
```

低优先级 Mode 先绘制，高优先级 Mode 后绘制。实际 selection 和 terminal
cursor 位于 Mode decoration 之上。Face 以字段 patch 方式合并，使 selection
背景色可以保留语法前景色。

同一份 Mode 顺序同时决定输入优先级、独占 policy 优先级和 decoration
层级。第一阶段不增加独立 presentation priority。

## 19. Face 与颜色

Decoration 引用命名 Face，不直接携带 RGB：

```text
TextDecoration(range, "syntax.function")
```

FaceRegistry 集中解析：

```text
"syntax.function" -> Face(...)
"search.match"    -> Face(...)
"vim.selection"   -> Face(...)
```

Mode 可以声明默认 Face，用户主题和脚本可以覆盖。脚本边界使用
`FaceName`，内部可以解析为 `FaceId` 以避免热路径字符串查找。

protocol 保存中立的 Color、Face、TextDecoration 和呈现数据。TUI 只处理
解析后的样式，不知道 Mode、Tree-sitter capture 或脚本对象。

## 20. Presentation cache 与 Pull 查询

呈现 callback 只读，不能修改状态或产生 operation。它们只在 app 主循环的
受控 refresh 阶段运行：

```text
view_policy(content_state, view_state, context)
decorations(content_state, view_state, context, visible_range)
```

refresh 结果进入 `PresentationLayerStore`。共享 content layer 按
`(ModeId, ContentId)` 保存，view layer 按 `(ModeId, ViewId)` 保存，并记录
source content/view revision。

TUI 仍先确定 viewport，再按可见文本范围拉取 decorations：

```text
TUI viewport
-> RenderQuery::decorations(view, visible_range)
-> AppQuery 查询 PresentationLayerStore
-> 拒绝 stale layer，按 Mode 顺序过滤并解析 Face
-> TUI paint
```

`AppQuery` 和 renderer 不调用 Mode trait、V8、worker 或 plugin runtime。
Frontend 继续使用 pull query，不物化包含全部 Content/View 的 frame snapshot。

content state 初始化可通过 `ContentQuery::Text` 读取保留原始换行形式的
精确文本和当前 revision；后续增量更新走 `ContentChange`，不在呈现查询中
重复复制全文。

共享 content state 的 revision 变化会使其全部 attachment View 失效；
view state revision 变化只使对应 View 失效。可变 callback 被调用后保守递增
revision，不要求动态 ModeState 实现相等比较。

## 21. 脚本 adapter

Native 和 script Mode 都注册为同一个 `ModeDefinition`。App 只持有通用
Mode trait object，不按实现类型分支。

具体 TypeScript runtime、状态、Content 编辑和模块边界由
`typescript-scripting-architecture.md` 定义；本文只保留语言无关的 Mode
contract。

脚本作者定义：

```text
mode name
content state factory
view state factory
content actions
view actions
input handler
content change callbacks
view policy
decorations
```

脚本 adapter 负责 callback 调用、`ModeValue` 转换、checkpoint 和 VM 错误
映射。具体脚本语言不是 Mode architecture 的组成部分。

## 22. 动态卸载与重载

`ModeId` 分配后不复用。卸载时：

1. 从所有 View 的 chain 移除；
2. 删除对应 view state 和 input state；
3. 删除无 attachment 的 content state；
4. 取消 pending key sequence；
5. 使相关 View 失效；
6. 从 Registry 移除定义。

热重载分配新的 `ModeId`，记录旧 attachment 位置，完整卸载旧定义，再在
原位置附加新定义。全部 content/view state 重新创建，fault 状态清除。

第一阶段不自动迁移旧状态。未来确有需求时，通过显式
`export_state`/`import_state` 扩展，而不改变基础生命周期。

## 23. 分层约束

目标依赖方向保持：

```text
frontend -> protocol
app      -> frontend + core + protocol
tui      -> frontend + terminal + protocol
main     -> app + tui + terminal
terminal -> protocol
core     -> protocol/std
protocol -> std
```

具体边界为：

- `core`：Content、ContentAction、ContentChange 和纯编辑算法；
- `protocol`：Face、Color、Decoration 和 pull query 数据；
- `app::mode`：Mode contract、Registry、状态表、命令和 effect；
- `app::runtime`：输入遍历、执行帧、effect 和跨 Store 协调；
- `app::query`：policy、decoration 和 Face 合并；
- `tui`：viewport、可见范围查询和绘制。

`ModeName` 和 `ModeActionName` 最终属于 app extension contract，不应继续
作为 core 概念。

## 24. 迁移顺序

实现按四个阶段推进：

1. 合并 `ContentMode` 和 `ViewMode`，暂时只运行现有 Vim；
2. 引入 ModeChain、独立 input state 和顺序输入执行；
3. 加入生命周期、ContentChange 通知和 fault isolation；
4. 扩展 Face、Decoration 和可见范围 pull query。

script runtime 和 Tree-sitter 在以上基础设施完成后接入，不进入基础重构。

每一阶段都必须保持现有 Vim、普通编辑、事务、保存、viewport 和 TUI 行为，
并在进入下一阶段前通过测试和 clippy。

## 25. 架构不变量

- 一个 Mode 定义同时拥有两种状态作用域和两种 action 作用域；
- 同一 View 的 Mode 按一个稳定、显式的顺序处理输入；
- Stop 阻止后续 Mode，Continue 允许后续 Mode 观察已执行效果；
- Content 不拥有 Mode，不接收 KeyEvent，不访问 View；
- View 不拥有 Mode instance，不解释具体 Content 或 Mode；
- App 不识别具体 Mode 实现；
- content state 每 `(ModeId, ContentId)` 唯一；
- view state 每 `(ModeId, ViewId)` 唯一；
- Mode 只能直接修改自己的私有状态；
- Content 和 View mutation 只通过 typed effect；
- 一次物理输入是一个 ExecutionFrame；
- HistoryTransaction 只由显式 commit 发布；
- 被动 Mode 故障不能阻止文本编辑；
- 呈现 callback 在受控阶段刷新 cache，renderer 按可见范围只读 pull；
- Mode 产生语义 Face，统一 registry 决定最终颜色；
- native 和 script Mode 共享同一注册、执行和生命周期模型。

## 26. 完成标准

- Registry 不再包含 `RegisteredMode::{PerContent, PerView}`；
- `ContentMode`、`ViewMode` 和两套 result/operation 被统一模型取代；
- 一个 View 可以附加并观察多个有序 Mode；
- 输入 Continue/Stop、pending、timeout 和 replay 有集成测试；
- 跨 ModeCommand 按顺序执行并受统一预算限制；
- 多个 View 共享同一 Mode content state，但 view state 独立；
- edit、undo 和 redo 都通知共享及局部 Mode state；
- 主动错误恢复执行帧，被动通知错误隔离 attachment；
- execution frame 与 history transaction 的测试分别覆盖；
- cursor domain 在合并后的 View policy 下保持有效；
- TUI 只查询可见范围 decoration；
- 多 Mode Face 层级和 selection 合成有渲染测试；
- core、View、ContentStore、TUI 和 transaction manager 中没有具体 Mode 分支；
- `cargo test`、`cargo clippy --all-targets --all-features`、`cargo fmt`
  和 `git diff --check` 全部通过。
