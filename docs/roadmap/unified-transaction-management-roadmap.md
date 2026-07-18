# Unified Transaction Management Roadmap

**状态：** 已实施并复核

**创建日期：** 2026-07-17

**最近更新：** 2026-07-17

## 1. 文档定位

本文记录统一事务管理机制的目标、边界和实施顺序。

它是一份长期改进 roadmap，不是具体实现计划。进入编码前，仍需在
`docs/design/` 中确认类型、接口和迁移步骤。

本文不使用 `EditorTransaction` 作为名称，因为事务服务于通用
`Content`，不能假定所有 Content 都是文本编辑器。

文中的暂定术语如下：

- `TransactionManager`：统一管理事务生命周期和历史顺序；
- `TransactionRecord`：一次已提交事务的完整记录；
- `ContentTransaction`：core 穷尽分派的 Content 事务数据；
- `TransactionData`：Content 事务和通用 View participant 的配对；
- `ViewTransactionData`：来源 View 的 selections 数据或无 participant；
- `ContentModeContext`：只提供目标 Content 读取能力的 Mode 上下文；
- `ViewModeContext`：提供目标 View 及其绑定 Content 读取能力的 Mode 上下文；
- `ContentAction`：与输入来源无关、由 Content 验证并应用的数据操作；
- `ViewAction`：由 app 应用于目标 View 的会话状态操作；
- `ContentModeResult`：不允许包含 ViewAction 的 Content Mode 有序结果；
- `ViewModeResult`：可包含 View/Content/App action 和事务意图的有序结果。

这些名称可以在 design 阶段调整，但不得重新引入 editor 限定。

第一阶段不记录 Mode 运行状态的历史数据。Mode 仍可决定事务边界，
但 undo/redo 不恢复 Mode 状态。

## 2. 当前问题

当前 Buffer 同时拥有：

- 文本事务和 undo/redo 历史；
- `TextStateId` 和保存状态；
- transaction 前后的完整 selections。

但是 selections 属于 View，`ModeInstance` 也被直接放在 View 中。
这导致两个不同问题：content-global 的 Buffer 历史保存了 view-local 状态；
本应控制输入、Content 和 View 协作顺序的 Mode，反而成为被 View 拥有的下层对象。

Mode 已经不只是 Content 命令的生成器。它还决定输入等待和 timeout、cursor style、
selection shape，并可以产生 App 与 Viewport 操作。以后 Mode 还需要切换 focus、调整
来源 View 状态，或协调多个 View。继续让 `View -> ModeInstance` 会迫使 View 承担
控制者职责，也会让 Mode 为了访问 View 反向依赖自己的 owner。

问题也不只在实例所有权。当前所有 Mode 共用同一个 trait；keymap、typing、capture、
timeout、presentation 和 execute 入口只接收 `ModeState`、按键或 action。该接口既不能让
需要 View 语义的 Mode 读取来源 View，也不能从类型上禁止只应面向 Content 的 Mode
访问 View。单纯移动 `ModeInstance` 不能修复这个能力边界。

当前 undo 直接恢复完整 selections，但不经过 Mode 语义协调。
因此 Visual 删除后在 Normal 模式执行 undo，会得到：

```text
VimState::Normal + non-collapsed selections
```

这个组合不符合 Vim 语义，也暴露出事务所有权和状态协调缺失。

当前 Vim 状态切换和 Visual operator 的解析顺序也不完整。Vim action
可以先修改 `VimState`，再让后续编辑命令继续使用活动 selections。
事务因而可能捕获 Normal 状态下尚未压缩的 Visual selections。

当前活动文本事务属于 Buffer，但没有记录产生它的 View。同一 Content
被多个 View 绑定时，不同 View 的编辑可能被错误合并到一个事务。

当前 Save 直接在 Buffer 内 checkpoint。统一事务同时包含 Content 和
View 数据后，Buffer 无法独立捕获来源 View 的完整 checkpoint。

另外，当前 `Mode::on_timeout` 只能修改 Mode 私有状态，不能通过与
按键相同的路径请求提交事务，无法满足由定时器决定事务边界的需求。

## 3. 已确认的目标

### 3.1 事务流按 Content 隔离

每个可事务化 `ContentId` 只有一个逻辑事务流和一套历史顺序。

`TransactionManager` 可以统一实现多个事务流，但不能建立 App 全局
undo 顺序。对 Content A 的 undo/redo 不得遍历 Content B 的历史。

第一阶段不支持跨 Content 的原子事务。以后如有需求，应在现有
per-Content 事务之上单独设计 transaction group。

### 3.2 只有一个历史权威

不能让 Buffer、View 和 `TransactionManager` 分别维护互不协调的
事务栈或历史游标。

`TransactionManager` 按 `ContentId` 统一拥有：

- 已提交历史和当前历史游标；
- redo 分支截断；
- 当前活动事务；
- 活动事务的来源和生命周期状态。

Buffer 只负责：

- 生成和应用 Buffer 事务数据；
- 维护当前和已保存的 `TextStateId`；
- 根据 state identity 判断 dirty 状态。

Buffer 不再保留第二套 undo/redo history 或 history cursor。

### 3.3 管理器属于 App 协调层

统一事务需要同时访问 `ContentStore` 和 `ClientSession` 中的 View。
因此协调器应位于 app 层，不得让 core 依赖 app 的 `View`。

core 可以定义 Content 自己的事务数据及其生成、验证和应用接口。
app 层负责把 Content 数据与来源 View 数据组成 outer record，并按顺序
协调 Content、View 和其他绑定同一 Content 的 View。

具体文件位置在 design 阶段确定，但依赖方向必须继续满足：

```text
app -> core + protocol
core -> protocol/std
```

### 3.4 Mode 按 Content 和 View 划分能力

Mode 定义、registry 和 runtime 实现提升到 app 层，但实例作用域不统一。Mode 定义必须在
注册时选择以下两种静态契约之一：

| 类型 | 实例 identity | 可读取 | 可产生 |
| --- | --- | --- | --- |
| `ContentMode` | `(ModeId, ContentId)` | 目标 Content | ContentAction、事务意图 |
| `ViewMode` | `(ModeId, ViewId)` | View + Content | 各类 action、事务意图 |

不使用 `ModeScope` 加 `Option<View>` 的统一上下文。否则 ContentMode 仍能在运行时尝试访问
View，能力错误只能到执行阶段才暴露。Registry 应使用闭合定义或等价静态分派保存两类 Mode：

```rust
pub trait ModeDefinition {
    fn name(&self) -> &ModeName;
    fn actions(&self) -> &[ModeActionName];
    fn new_state(&self) -> Box<dyn ModeState>;
}

pub trait ContentMode: ModeDefinition {
    fn execute(
        &self,
        state: &mut dyn ModeState,
        context: &ContentModeContext<'_>,
        action: &ModeActionName,
    ) -> Result<ContentModeResult, ModeError>;
}

pub trait ViewMode: ModeDefinition {
    fn execute(
        &self,
        state: &mut dyn ModeState,
        context: &ViewModeContext<'_>,
        action: &ModeActionName,
    ) -> Result<ViewModeResult, ModeError>;
}

pub enum RegisteredMode {
    PerContent(Box<dyn ContentMode>),
    PerView(Box<dyn ViewMode>),
}
```

keymap、typing、capture、timeout、cancel 和 action execute 等需要运行时
上下文的入口遵守相同的类型划分；上例只展示 execute，不要求建立第二套
无关输入框架。cursor style、selection shape 等 View presentation 只属于
ViewMode，ContentMode 不参与 View 渲染语义。

`ContentModeContext` 只包含目标 Content identity 和只读 query 能力。它不包含 `ViewId`、
View query、`ContentViewState` 或 View mutation；`ContentModeResult` 在类型上不能携带
ViewAction。输入即使来自某个 View，来源 `ViewId` 也由 Dispatcher 在 Mode 外部保存，仅供
事务 participant 和目标解析使用。

`ViewModeContext` 只提供绑定 View 以及该 View 所绑定 Content 的只读能力。
第一阶段不向它开放整个 ViewStore；需要 focus、split 或 close 时返回独立
App action，而不是直接读取或修改其他 View。所有 action 都由 app 协调器
验证后执行，Context 不借出长期可变引用，也不允许 Mode 在返回结果前
留下隐藏副作用。

ModeState 的原地修改只属于尚未提交的 provisional runtime。若后续 action
验证失败，执行器必须恢复该实例状态；design 也可以选择让 typed result
携带 next state，在所有 action 验证成功后再替换。两种方案只能选择一种，
不得让失败结果留下部分 ModeState。

概念所有权如下：

```text
App
├── Kernel / ContentStore
├── TransactionManager
├── ModeRegistry
├── ContentModeInstances: (ModeId, ContentId) -> ModeInstance
└── ClientSession
    ├── Scene + ViewStore
    ├── ViewModeInstances: (ModeId, ViewId) -> ModeInstance
    └── Dispatcher
```

第一阶段每个 Content 或 View 最多绑定一个 Mode 定义。Content 绑定
ContentMode 时建立实例；引用同一 Content 的多个 View 共享该实例。关闭
单个 View 不销毁它，Content 生命周期结束时才移除。

View 绑定 ViewMode 时建立实例，关闭 View 时移除。Vim 使用该策略，
因此不同 View 可以分别处于 Normal、Insert 或 Visual。Mode 的作用域是
View 而不是 Space；View 移动到其他 Space 时保留原实例。

每个 View 最多解析到一个 effective Mode binding。Content 绑定
ContentMode 时，其所有 View 使用该共享 binding，并且不能再绑定
ViewMode；Content 没有 ContentMode 时，各 View 才能独立绑定一个
ViewMode 或保持无 Mode。第一阶段不定义两类 Mode 的优先级或叠加规则。

ModeInstance 不再存放于 View 或 Content。目标对象只提供实例 identity 和运行上下文，实例表
才是 owner。具体 Mode 语义只存在于对应定义中；registry、Dispatcher、View、ContentStore 和
事务管理器不得包含 `if mode == vim` 等分支。

### 3.5 Content 只接受领域 Action

Mode 负责把按键、动态输入和 ModeAction 解释成有序结果；Content 不再承担顶层命令路由或
View 会话操作。目标分层如下：

```text
ModeAction  -> ContentMode / ViewMode
ViewAction  -> app applies to View
ContentAction -> Content validates and applies to data
TransactionIntent -> TransactionManager
AppAction   -> app
```

Content 仍是领域模型而不是被动数据袋。它只负责：

- 保存和查询 Content 数据；
- 验证并应用 ContentAction；
- 生成规范 ContentChange；
- 生成、验证和应用 Content 自己的事务数据；
- 维护 text state identity、保存快照和 Content event 等内容不变量。

Content 不再接收 `ContentInput::View` 或可变 `ContentViewState`。光标移动、selection
扩展/压缩和 selection shape 变化属于 ViewAction；文本插入、删除和替换属于
ContentAction。Content 返回 ContentChange 后，由 app 将 change 映射到所有绑定 View。

顶层 `Command`、AppCommand、ModeCommand 和目标路由属于 app；core 只保留 ContentAction、
纯文本 motion/target 算法、事务数据和其他内容领域类型。脚本或远程输入
可以直接产生 ContentAction；undo/redo、保存和 Content event 则通过各自
的 typed 路径进入同一 app 协调器。任何来源都不得为了复用执行接口伪造
Mode。

Content 不再声明或实例化默认 Mode。初始 Mode 绑定由 app bootstrap/session 配置建立；
新增 Content 不需要依赖 ModeName 或 ModeRegistry。

### 3.6 Content 与 View 数据静态配对

`Content` 是静态闭合的内容集合。core 使用同样闭合的
`ContentTransaction` 分派具体事务数据；app 只把它与通用 View
participant 配对，不识别具体 Content 变体。

概念结构如下：

```rust
pub struct TransactionRecord {
    pub target: ContentId,
    pub data: TransactionData,
}

pub struct TransactionData {
    pub content: ContentTransaction,
    pub view: ViewTransactionData,
}
```

`ContentViewState` 不镜像 `Content` 的具体变体，只表达跨 Content 复用的
View 会话能力。第一阶段它只区分是否具有文本 selections；具体 Content
负责创建适合自己的状态，`ContentStore` 在 change mapping 边界验证能力
是否匹配。

以后新增可事务化 Content 时，只在 core 同时扩展：

- `Content`；
- `ContentTransaction`；
- `ContentStore` 的穷尽分派。

app 的 `TransactionManager`、runtime 和 View participant 不随具体
Content 类型变化。

只有出现新的通用 View 会话能力时才扩展 `ContentViewState`，不得为每个
具体 Content 增加同名状态变体。

不使用动态 Content transaction trait 绕过静态穷尽检查。

### 3.7 View participant 可以不存在

第一阶段的 View participant 只有两种情况：

```rust
pub enum ViewTransactionData {
    Source {
        view: ViewId,
        before: Selections,
        after: Selections,
    },
    None,
}
```

普通 View 输入使用 `Source`。没有目标 View 的 Content event 或脚本
可以使用 `None`，但不得伪造 `ViewId`。

来源 View 由对应 View 数据变体表达，不再要求每条 outer record 都有
顶层 `source_view` 字段。

viewport、focus、布局和渲染状态暂时不进入事务记录。

### 3.8 Buffer 数据暂时只有文本数据

第一阶段的 Buffer Content 数据至少包含：

- 文本的 forward/inverse change；
- 文本事务前后的 state identity。

概念结构如下：

```rust
pub struct BufferTransactionData {
    pub text: TextTransactionData,
}
```

Mode 运行状态、pending key、count 和 operator 不进入 history。
普通 Mode 切换也不产生 undo entry。

undo/redo 不切换 Mode，只恢复 Content 和可用的 selections。需要撤销
跨 Mode 转换或恢复 Mode 私有状态时，再单独扩展事务模型。

## 4. 事务生命周期与提交权

### 4.1 Manager 拥有提交生命周期，Content 验证数据

`TransactionManager` 不应根据一次按键处理结束、一次命令分派结束，
或一次事件循环迭代结束自动提交。

Mode 可以提出 begin、commit、rollback 或 checkpoint 意图。目标 Content
负责验证 action 和事务数据、生成 change，并判断 payload 是否为空；
TransactionManager 负责执行生命周期转换、owner 检查、history cursor 和
redo 截断。不得让 Content 和 Manager 各自维护一套提交状态。

两类 Mode 都不得直接修改 TransactionManager 或绕过 ContentStore。
Mode 的 action 和事务意图必须进入对应 typed result，由 app 协调器统一
验证和执行，而不是先产生隐藏副作用再通知 Content。

### 4.2 提交请求不绑定输入来源

统一接口至少需要表达：

- begin；
- commit；
- rollback；
- checkpoint；
- 无事务边界变化。

相同的提交请求必须能够来自：

- 普通按键 action；
- 多键序列完成；
- 输入 timeout；
- Content event；
- 将来的脚本或远程输入。

这些输入可以关联 View，也可以是没有 View participant 的 Content 输入。
没有 View participant 的事务不记录或恢复 selections。

不能只让两类 Mode 的 action 返回提交请求，而让 `on_timeout` 继续返回
`()`。按键和定时器路径最终必须汇合到对应的 typed result。

### 4.3 事务请求必须表达执行顺序

事务接口不能只表达 begin、commit 等动作，还必须明确它们相对于
Mode 转换和 ContentAction 的顺序。

至少需要覆盖：

```text
begin -> mode transition -> content action
mode transition -> content action -> commit
content action -> mode transition -> commit
checkpoint -> content action
```

Mode action 不能先隐藏地修改 View 或 Content，再返回一个无法表达前后
顺序的结果。ModeState 的 provisional 修改必须遵守 4.6 的 runtime
rollback 契约。design 阶段需要确定 typed result 内部的有序表示，但不得
依靠 Dispatcher 猜测正确顺序。

### 4.4 活动事务必须记录 owner

每个 Content 同时最多只能有一个活动文本事务。活动事务的 owner 为：

- 某个 `ViewId`；或者
- 没有 View participant 的 Content 输入。

当 View B 尝试修改一个由 View A 持有活动事务的 Content 时，不得把
B 的修改合并到 A 的事务，也不得允许两个文本事务交错应用。

推荐顺序如下：

```text
checkpoint or commit View A transaction
-> begin View B transaction
-> apply View B operation
```

已有非空事务应先提交，空事务直接丢弃。单纯切换 focus 不需要提交；
只有另一来源真正开始修改同一 Content 时才建立边界。

关闭 owner View 前必须先处理其活动事务。非空事务提交，空事务丢弃，
然后才能移除 View。timeout 可以提交当前不处于 focus 的 owner View。

### 4.5 Save 必须通过统一 checkpoint

Save 不得继续由 Buffer 私自 checkpoint。推荐流程如下：

```text
receive Save request
-> find active transaction for ContentId
-> capture owner View after selections when it exists
-> checkpoint the complete outer transaction
-> create save snapshot with current TextStateId
-> optionally continue a new transaction for the same owner
```

如果 owner 对应的 ModeInstance 仍处于可连续编辑状态，例如 Insert，
checkpoint 后可以为同一 owner 重新开始事务。保存本身不要求退出当前
Mode。

异步 `SaveFinished` 只确认对应 text state 是否保存成功，不建立新的
undo entry。保存期间发生的后续编辑不得被错误标记为已保存。

### 4.6 原子回滚和历史恢复分离

产生操作的 ModeInstance 状态可以参与当前 ordered operation 的 runtime
rollback，但它不进入 undo/redo history。

优先通过 ordered execution 避免在 Content 成功前提前修改 Mode。
只有确有失败恢复需求时，才为单次运行时操作引入临时 checkpoint；
这种 checkpoint 不得写入历史。

应用已提交记录前，应先验证 target Content、事务变体和文本数据。
验证成功后的 Content 和 View apply 应不可失败。来源 View 已关闭属于
允许跳过 View 数据的正常情况，不是部分失败。

### 4.7 空事务和 redo 截断

第一阶段只有产生语义 Content change 的记录才进入历史。普通 selection
移动、Mode 切换和其他纯 View 变化不单独形成事务。

Content 验证失败时不得进入提交；Content 声明 payload 为空时，Manager
必须丢弃活动记录。只有成功提交非空事务后，才能截断 redo 分支。

## 5. Mode 控制、转换与事务快照

### 5.1 ModeInstance 生命周期由静态类型决定

ModeRegistry 在注册时已经知道定义是 ContentMode 还是 ViewMode。分派器
据此选择唯一实例 key，不在运行时猜测 scope：

```text
ContentMode -> (ModeId, ContentId)
ViewMode    -> (ModeId, ViewId)
```

Content 绑定 ContentMode 时建立 ContentModeInstance，销毁 Content 时移除。
多个 View 绑定同一 Content 时共享该实例；View close 不影响它。

View 绑定 ViewMode 时建立 ViewModeInstance，close View 时移除。split
如果创建新 View，就创建独立实例；移动既有 View 到其他 Space 不创建新
实例。Vim 的 Normal、Insert、Visual、pending key、count 和 operator
因而继续按 View 隔离，但实例由集中表拥有，不再嵌入 View。

View 只保留：

- `ContentId`；
- `ContentViewState`；
- View revision 和其他真正按 View 隔离的展示状态。

`ContentViewState` 是中立能力记录，不包含 `Buffer`、`StatusBar` 等具体
Content identity。View 只调用其通用能力方法，不匹配具体 Content 变体。

Mode keymap、typing fallback、dynamic capture、timeout 和 action transition
由具体定义处理。ContentMode 的这些行为只能依赖 ContentModeContext；
ViewMode 可以依赖绑定 View 和 Content。

cursor style、selection shape 等 Mode presentation 只由 ViewMode 提供，
并按 ViewModeInstance 与对应 View 组合。使用 ContentMode 或无 Mode 的
View 使用中立 View presentation。渲染查询不识别 Vim/Helix 等具体定义。

Dispatcher 可以记录输入事件来自哪个 View，用于结果目标解析和 transaction
participant，但该 identity 不进入 ContentModeContext。ContentModeInstance
共享输入状态是其 Content 作用域的明确语义，不得通过隐藏 View identity
重新建立 per-View 状态。

### 5.2 不建立所有 Mode 共用的生命周期 hook

事务框架不要求所有 Mode 实现 enter/leave hook。

不同 ViewMode 对 selections 的语义不同。例如 Helix 风格 Normal 可以保留
范围 selections，而 Vim Normal 需要压缩活动 Visual selections。
将 Vim 规则提升为通用 hook 会增加无关 Mode 的实现负担。

具体 ViewMode 应在自己的实现内部集中处理转换。Vim 可以提供私有的
`transition_to_normal` 或等价入口；不需要转换副作用的 Mode 不增加
额外实现。

### 5.3 Visual operator 必须先解析操作对象

Vim Visual operator 不能在退出 Visual 后继续依赖活动 selections 作为
删除或修改范围。

退出 Visual 前，应先生成内部 resolved operation。它至少包含：

- 已解析的 ranges；
- charwise、linewise 或后续 blockwise shape；
- operator 类型；
- 必要的方向和目标光标位置。

Visual delete 的推荐顺序为：

```text
capture resolved operation
-> transition to Normal
-> collapse live selections
-> begin transaction with canonical before selections
-> apply resolved delete
-> commit in Normal
```

Visual change 在应用删除后进入 Insert，并继续同一个事务。它在离开
Insert、重新进入 Normal 后才提交：

```text
resolve and normalize Visual state
-> begin transaction
-> apply resolved delete
-> enter Insert and continue editing
-> leave Insert and normalize selections
-> commit in Normal
```

这样进入 Normal 时可以立即维护 selection 不变量，同时不会丢失 Visual
操作范围。

resolved operation 是 Vim ViewMode 的私有中间数据。它最终生成普通
ViewAction 和带 resolved ranges 的 ContentAction，不把 Vim 专用类型加入
公开 ContentAction，也不进入 protocol。

### 5.4 Insert 和 timeout 提交

普通 Insert transaction 应在 Normal 的 canonical selections 上开始，
并在离开 Insert、重新进入 Normal 后提交。

Vim ViewMode 也可以在不退出 Insert 的情况下通过 timeout 请求 checkpoint
或 commit。此时只记录文本和 selections，不记录“历史 Mode”。undo/redo
保持所有现存 ModeInstance 的 runtime 状态不变。

如果某种 Mode 无法保证其已提交 selections 能在不恢复 Mode 状态的
情况下安全应用，则第一阶段不得为它恢复 View 快照。它可以使用没有
View participant 的事务，或者等待后续扩展历史模型。

### 5.5 只有 ViewMode 转换可以控制 View

ContentModeResult 在类型上不能包含 ViewAction，因此 ContentMode 转换不能
读取、修改或规范化任何 View。Content change 对 View 的通用映射由 app
处理，不属于 ContentMode 的隐藏能力。

ViewMode 转换可以控制其绑定 View，但控制必须记录为有明确目标和顺序的
ViewAction，不能藏在 View 自身的方法中，也不能在返回结果前直接修改
ContentViewState。

Vim 转换到 Normal 时可以声明压缩 selections。其他 ViewMode 的转换如果
需要调整 View 状态，也由该次转换显式携带；默认保持 View 状态。

Vim ViewMode 可以在同一个 ViewModeResult 中表达“先读取绑定 View 的
Visual ranges，再压缩该 View，随后修改 Content 并提交事务”。事务管理器、
View 和 ContentStore 不得包含 `if mode == vim` 等具体分支。

## 6. 历史遍历与多 View 规则

统一事务机制必须保持以下边界：

- 历史流按 `ContentId` 隔离；
- selections 属于产生事务的 View；
- Mode 运行状态不属于第一阶段历史；
- 同一 Content 可以被多个 View 绑定；
- 非来源 View 只通过 Content change 映射当前 View 状态；
- 关闭来源 View 不得破坏 Content 文本历史；
- undo 分支和 redo 截断只有一个权威实现；
- `ViewId` 在同一 session 内不得复用。

从 View 发起 undo/redo 时，首先根据该 View 绑定的 `ContentId` 选择历史
流。显式 Content 输入则直接指定目标 `ContentId`。

如果记录的来源 View 仍存在：

- Content 分量正常应用；
- 来源 View 恢复该记录的 selections；
- 其他 View 通过 Content change 映射当前 selections；
- 所有现存 ModeInstance 的 runtime 状态保持不变。

如果 View B 撤销了来源为 View A 的记录，恢复的是 A 的快照。B 作为
非来源 View 只执行 change mapping，不使用 A 的快照。

如果来源 View 已关闭，Content 分量仍应正常遍历。所有现存 View 只执行
change mapping，已关闭 View 的快照安全跳过。

没有 View participant 的记录只应用 Content 分量，并将 change 映射到
所有绑定该 Content 的 View。

## 7. 处理顺序总览

| 编号 | 优先级 | 状态 | 工作项 |
| --- | --- | --- | --- |
| T00 | P0 | 已完成 | 建立两类 Mode 能力与 Action 边界 |
| T01 | P0 | 已完成 | 确认事务作用域和语义不变量 |
| T02 | P0 | 已完成 | 建立静态配对的事务数据模型 |
| T03 | P0 | 已完成 | 建立统一生命周期和 checkpoint 路径 |
| T04 | P0 | 已完成 | 建立 Mode 转换与 Visual 操作解析 |
| T05 | P0 | 已完成 | 迁移 Buffer 历史并修复 Vim undo 语义 |
| T06 | P1 | 已完成 | 验证多 Content 和多 View 扩展性 |

实施复核结论：

- Mode 定义、实例表、Context 和 typed result 已迁入 app；
- ContentMode 与 ViewMode 的 Context 能力由不同 trait 静态约束；
- `TransactionManager` 已成为 per-Content 历史和生命周期唯一所有者；
- outer transaction 已配对中立 Content 事务和通用 View participant；
- Buffer 已删除 history stack、history cursor 和 View selections 快照；
- Save、跨 View 编辑和 owner View 关闭共用 checkpoint 路径；
- Visual delete/change 先解析静态操作，再切换 Mode 状态；
- ordered result 失败会恢复 Content、View、Mode 和事务管理器；
- 多 Content、多 View、无 View participant 和来源 View 关闭均有测试覆盖。

## 8. 工作项明细

### T00：建立两类 Mode 能力与 Action 边界

目标：

- 在 `docs/design/` 更新 Mode、View、Content 和命令所有权设计；
- 将 Mode 定义、registry 和实例表从 core/View 提升到 app；
- 建立 ContentMode 与 ViewMode 两个静态 trait 契约；
- 使用 RegisteredMode 闭合枚举或等价类型安全注册方式；
- 建立 ContentModeContext、ViewModeContext 和对应 typed result；
- Context 只读，所有修改通过 ContentAction、ViewAction 和事务意图返回；
- 明确每个 View 最多解析到一个 effective Mode binding；
- 禁止同一 View 同时使用 ContentMode 和 ViewMode；
- 按 `(ModeId, ContentId)` 和 `(ModeId, ViewId)` 分别管理实例；
- 选择 ModeState provisional rollback 或 typed next-state 契约；
- View 删除 mode 字段和所有 ModeInstance 代理方法；
- Content 删除 default mode、ContentInput::View 和 View 会话修改；
- 将顶层 Command、AppCommand、ModeCommand 和目标路由迁入 app；
- Dispatcher 保留输入来源，但不向 ContentMode 暴露 View identity；
- 渲染查询只组合 ViewMode presentation；ContentMode 不影响 View 渲染。

完成标准：

- ContentMode 在编译期无法访问 View 或构造 ViewAction；
- ViewMode 只能读取绑定 View 及其 Content，不能直接修改二者；
- Mode action 失败不会留下部分 ModeState、View 或 Content 修改；
- Vim 的不同 View 保持独立 ModeInstance 和可观察状态；
- 共享 ContentMode 的多个 View 使用同一 ModeInstance；
- 每个 View 只能解析到 ContentMode、ViewMode 或无 Mode 三者之一；
- View、ContentStore、registry 和事务管理器没有具体 Mode 分支；
- core 不再保存 AppCommand、ModeCommand 或 Mode 定义；
- ContentAction 可由 Mode、undo/redo、event 或脚本使用，无需伪造 Mode；
- 现有 `docs/design/editor-kernel-architecture.md` 和
  `docs/design/command-execution-ownership.md` 不再描述旧所有权；
- 该边界确认后再进入 T01-T05 的事务实现。

### T01：确认事务作用域和语义不变量

目标：

- 在 `docs/design/` 编写统一事务设计；
- 确认每个 `ContentId` 拥有独立历史流；
- 确认 Manager 是唯一历史和活动事务权威；
- 确认活动事务 owner 和跨 View 抢占规则；
- 确认有 View 和无 View participant 的语义；
- 区分 runtime rollback 与 undo/redo history；
- 确认事务 identity、分支和保存状态的关系；
- 确认 typed Mode result、TransactionManager、ContentStore 与 ViewStore
  的调用顺序；
- 确认第一阶段不恢复 Mode 历史状态。

完成标准：

- 不再存在 App 全局 undo 顺序的歧义；
- 不再依赖“完整复制所有运行时状态”的模糊定义；
- View 关闭、跨 View 编辑和 Save 都有明确边界；
- 本 roadmap 的审核结论已经写入 design。

### T02：建立静态配对的事务数据模型

目标：

- 引入不带 editor 限定的 outer transaction 类型；
- 在同一枚举变体中配对 Content 和 View 数据；
- Buffer 数据成为第一个静态事务变体；
- 通用 View participant 支持 `Source` 和 `None`；
- View 数据只包含 before/after selections；
- 不把 viewport、focus、布局或 Mode 状态纳入历史。

完成标准：

- 新增 Content 类型时由编译器要求补齐穷尽分派；
- 不可能构造不同 Content 类型之间的事务数据组合；
- Buffer 不再直接拥有某个 View 的 selections 快照；
- 没有 View participant 的事务不需要伪造 `ViewId`。

### T03：建立统一生命周期和 checkpoint 路径

目标：

- 由 `TransactionManager` 按 `ContentId` 管理事务流；
- 每个 Content 最多一个带 owner 的活动文本事务；
- Manager 是事务生命周期和 history 的唯一权威；
- Content 只验证 action/transaction data 并生成 change；
- 两类 typed Mode result 都可以携带事务边界请求；
- 按键和 timeout 复用同一条提交请求路径；
- Save、View close 和跨 View 编辑使用统一 checkpoint；
- 请求可以表达事务动作、ViewAction 和 ContentAction 的顺序；
- 提交逻辑不依赖事件循环迭代或命令 Sequence 结束。

完成标准：

- 按键和 timeout 都可以提交同一事务；
- View B 的编辑不会并入 View A 的活动事务；
- Save 能捕获完整 outer record 并继续连续编辑；
- Content 可以拒绝无效数据或声明 payload 为空；
- 失败操作不会留下部分 Content 或 View 状态；
- redo 分支只在成功提交非空事务后截断。

### T04：建立 Mode 转换与 Visual 操作解析

目标：

- 不建立所有 Mode 共用的 enter/leave hook；
- 不把 ViewModeInstance 放回 View；
- Vim 实现 ViewMode，并通过 ViewModeContext 读取绑定 View 和 Content；
- Vim 在自身实现内集中处理状态转换；
- Vim 通过 ViewModeResult 中的 ViewAction 控制绑定 View；
- Visual operator 在退出 Visual 前生成 resolved operation；
- resolved operation 保存 range、shape、operator 和 cursor intent；
- Visual delete 在 Normal 和 collapsed selections 下形成事务；
- Visual change 将事务延续到后续 Insert 结束。

完成标准：

- Vim 进入 Normal 时 live selections 保持合法；
- Visual delete/change 不再依赖退出后的活动 selections；
- selection 压缩不再伪装成普通文本编辑；
- 不需要转换副作用的新 Mode 不实现额外 hook；
- View、ContentStore 和事务核心没有 Vim 特判。

### T05：迁移 Buffer 历史并修复 Vim undo 语义

目标：

- 将当前 Buffer 中的 `SelectionSnapshots` 迁移为 View 分量；
- 将 history、history cursor 和 redo 截断迁移到 Manager；
- Buffer 只实现 Buffer 事务数据的生成、验证和应用；
- undo/redo 通过 outer record 协调 Content 和来源 View；
- 为后续 `last_visual_selection` 和 `gv` 保留清晰扩展点。

完成标准：

- Visual 删除后 undo 不再产生
  `Normal + non-collapsed selections`；
- redo 使用一致的 Content/View 快照；
- 其他 View 只通过文本 change 映射 selections；
- 关闭来源 View 后仍能 undo/redo 文本；
- Buffer 和 Manager 不存在两个历史游标；
- 保存状态和 dirty 判断继续使用稳定 text state identity。

### T06：验证多 Content 和多 View 扩展性

目标：

- 用测试 Content 验证新的静态事务变体；
- 验证两个 Content 的历史完全隔离；
- 验证按键提交和 timeout 提交；
- 验证同一 Content 的多个 View 不会串联事务；
- 验证 ContentModeInstance 在多个 View 间共享状态；
- 验证 ViewModeInstance 按 View 隔离状态；
- 编译期证明 ContentMode 无法取得 View 或构造 ViewAction；
- 验证同一 View 不能同时解析到 ContentMode 和 ViewMode；
- 验证非来源 View undo 和来源 View 关闭场景；
- 验证没有 View participant 的 Content 事务。

完成标准：

- 新 Content 只扩展静态 Content 分派；
- 新 Mode 只选择 ContentMode 或 ViewMode 契约；
- 不需要修改 TransactionManager 或 View 中的具体 Mode 分支；
- 所有跨层失败都能 rollback 到一致状态；
- `cargo test`、clippy、fmt 和 diff 检查全部通过。

## 9. 非目标

本 roadmap 暂不包含：

- Mode 运行状态的历史快照和恢复；
- 撤销跨 Mode 转换；
- viewport、scroll、focus 和布局历史；
- 多客户端协同编辑事务；
- 跨 Content 原子事务；
- 持久化 undo 文件；
- 跨进程事务恢复；
- 任意 Content 的动态插件化；
- 所有 Mode 共用的 enter/leave 生命周期 hook；
- 恢复 View 对 ModeInstance 的所有权；
- ContentMode/ViewMode 之外的第三种实例作用域；
- major/minor Mode stack 或运行时组合多个 Mode；
- `gv` 的完整用户行为实现；
- 对瞬时 Mode 输入状态执行 undo/redo。

## 10. 本版默认决策

本版 roadmap 按以下已审核决策实施：

1. Mode 定义分为 ContentMode 和 ViewMode 两个静态契约，不使用
   `Option<View>` 统一上下文。
2. ContentMode 只能读取目标 Content，不能访问 View、产生 ViewAction
   或提供 View presentation。
3. ViewMode 可以读取绑定 View 及其 Content，并通过 ViewModeResult
   产生 ViewAction 和 ContentAction。
4. ContentModeInstance 按 `(ModeId, ContentId)` 共享；
   ViewModeInstance 按 `(ModeId, ViewId)` 隔离。实例都由集中表拥有；
   每个 View 只能解析到一个 effective Mode binding。
5. Context 只读；Mode、View 和 Content 修改必须通过 typed result 进入
   app 协调路径。失败结果不得留下部分 ModeState。
6. Content 只保存和查询数据、应用 ContentAction、生成 ContentChange，
   并生成/验证/应用自身事务数据；它不拥有 Mode 或 View 会话状态。
7. 顶层 Command 和目标路由属于 app；core 只保留内容领域 action、
   motion/target 算法和事务数据。
8. 每个可事务化 `ContentId` 拥有独立历史流，Manager 只是统一实现，
   不建立 App 全局 undo 顺序。
9. core 以闭合 `ContentTransaction` 分派具体事务；app 只配对中立
   Content 载荷和通用 View participant。
10. View participant 可以是来源 View，也可以不存在；不得伪造 View。
11. 第一阶段只记录 Content 和 selections，不记录或恢复 Mode 状态。
12. 每个 Content 最多一个活动文本事务，并显式记录 owner。
13. 跨 View 编辑、View close 和 Save 必须先走统一 checkpoint。
14. 非来源 View 发起 undo 时，来源 View 恢复快照，发起 View 只执行
   Content change mapping。
15. 事务框架不提供所有 Mode 共用的生命周期 hook。ViewMode 转换
   副作用使用显式 ViewAction 表达。
16. Visual delete 在 Normal 下提交；Visual change 延续到 Insert 结束后
   再于 Normal 下提交。
17. Mode-only 和普通 selection-only 变化不形成 undo entry。
18. Manager 是 history cursor 和 redo 分支的唯一权威，Buffer 只维护
    text state identity。
