# Unified Transaction Management Roadmap

**状态：** 待审核

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
- `TransactionData`：静态配对的 Content 和 View 事务数据；
- `BufferTransactionData`：Buffer 拥有的文本事务数据；
- `BufferViewTransactionData`：Buffer View 拥有的 selections 数据。

这些名称可以在 design 阶段调整，但不得重新引入 editor 限定。

第一阶段不记录 Mode 运行状态的历史数据。Mode 仍可决定事务边界，
但 undo/redo 不恢复 Mode 状态。

## 2. 当前问题

当前 Buffer 同时拥有：

- 文本事务和 undo/redo 历史；
- `TextStateId` 和保存状态；
- transaction 前后的完整 selections。

但是 selections 属于 View，Mode 运行状态也由 View 会话持有。
这导致 content-global 的 Buffer 历史保存了 view-local 状态。

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

### 3.4 Content 与 View 数据静态配对

`Content` 是静态闭合的内容集合。事务数据也应使用闭合枚举，并在同一
变体内配对该 Content 的 Content 数据和 View 数据。

概念结构如下：

```rust
pub struct TransactionRecord {
    pub target: ContentId,
    pub data: TransactionData,
}

pub enum TransactionData {
    Buffer {
        content: BufferTransactionData,
        view: BufferViewTransactionData,
    },
}
```

不能使用两个互相独立的枚举分别保存 Content 和 View 数据。否则以后
可能形成类型合法但语义错误的组合，例如 Buffer 数据配上其他 Content
的 View 数据。

以后新增可事务化 Content 时，同时扩展：

- `Content`；
- `ContentViewState`；
- `TransactionData`；
- `ContentStore` 的穷尽分派。

不使用动态 Content transaction trait 绕过静态穷尽检查。

### 3.5 View participant 可以不存在

第一阶段的 Buffer View 数据只有两种情况：

```rust
pub enum BufferViewTransactionData {
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

### 3.6 Buffer 数据暂时只有文本数据

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

### 4.1 Content 是提交权威

`TransactionManager` 不应根据一次按键处理结束、一次命令分派结束，
或一次事件循环迭代结束自动提交。

事务是否达到提交边界，由目标 Content 决定。对于带 Mode 的 Content，
通常由 Mode 逻辑提出提交请求，再由 Content 接受并完成提交。

Mode 不直接操作 `TransactionManager`，也不直接访问 App。它应通过
中立结果把事务意图返回给 Content。

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

不能只让 `Mode::execute` 返回提交请求，而让 `on_timeout` 继续返回
`()`。Mode 的按键和定时器路径最终必须汇合到同一个 Content 结果。

### 4.3 事务请求必须表达执行顺序

事务接口不能只表达 begin、commit 等动作，还必须明确它们相对于
Mode 转换和 Content 命令的顺序。

至少需要覆盖：

```text
begin -> mode transition -> content command
mode transition -> content command -> commit
content command -> mode transition -> commit
checkpoint -> content command
```

Mode action 不能先隐藏地修改状态，再返回一个无法表达前后顺序的命令。
design 阶段需要选择 ordered result、transaction intent 或等价机制，
但不得依靠 Dispatcher 猜测正确顺序。

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

如果 Mode 仍处于可连续编辑状态，例如 Insert，checkpoint 后可以为同一
owner 重新开始事务。保存本身不要求退出当前 Mode。

异步 `SaveFinished` 只确认对应 text state 是否保存成功，不建立新的
undo entry。保存期间发生的后续编辑不得被错误标记为已保存。

### 4.6 原子回滚和历史恢复分离

Mode 状态可以参与当前 ordered operation 的 runtime rollback，但它不
进入 undo/redo history。

优先通过 ordered execution 避免在 Content 成功前提前修改 Mode。
只有确有失败恢复需求时，才为单次运行时操作引入临时 checkpoint；
这种 checkpoint 不得写入历史。

应用已提交记录前，应先验证 target Content、事务变体和文本数据。
验证成功后的 Content 和 View apply 应不可失败。来源 View 已关闭属于
允许跳过 View 数据的正常情况，不是部分失败。

### 4.7 空事务和 redo 截断

第一阶段只有产生语义 Content change 的记录才进入历史。普通 selection
移动、Mode 切换和其他纯 View 变化不单独形成事务。

Content 拒绝提交或判断当前事务为空时，Manager 必须丢弃活动记录。
只有成功提交非空事务后，才能截断 redo 分支。

## 5. Mode 转换与事务快照

### 5.1 不建立所有 Mode 共用的生命周期 hook

事务框架不要求所有 Mode 实现 enter/leave hook。

不同 Mode 对 selections 的语义不同。例如 Helix 风格 Normal 可以保留
范围 selections，而 Vim Normal 需要压缩活动 Visual selections。
将 Vim 规则提升为通用 hook 会增加无关 Mode 的实现负担。

具体 Mode 应在自己的实现内部集中处理转换。Vim 可以提供私有的
`transition_to_normal` 或等价入口；不需要转换副作用的 Mode 不增加
额外实现。

### 5.2 Visual operator 必须先解析操作对象

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

resolved operation 是 Mode 到具体 Content 执行之间的内部数据，不要求
扩展公开 `EditCommand`，也不进入 protocol。

### 5.3 Insert 和 timeout 提交

普通 Insert transaction 应在 Normal 的 canonical selections 上开始，
并在离开 Insert、重新进入 Normal 后提交。

Mode 也可以在不退出 Insert 的情况下通过 timeout 请求 checkpoint 或
commit。此时只记录文本和 selections，不记录“历史 Mode”。undo/redo
保持来源 View 当前的 runtime Mode 不变。

如果某种 Mode 无法保证其已提交 selections 能在不恢复 Mode 状态的
情况下安全应用，则第一阶段不得为它恢复 View 快照。它可以使用没有
View participant 的事务，或者等待后续扩展历史模型。

### 5.4 Mode 转换结果显式携带 View 调整

Mode 转换可以产生受限的 View state adjustment，但它应是显式转换结果，
而不是所有 Mode 都会触发的隐式 hook。

Vim 转换到 Normal 时可以声明压缩 selections。跨注册 Mode 的切换如果
需要调整 View 状态，也由该次转换显式携带。默认转换保持 View 状态。

事务管理器、View 和 ContentStore 不得包含 `if mode == vim` 等具体分支。

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
- 所有 View 的 runtime Mode 保持不变。

如果 View B 撤销了来源为 View A 的记录，恢复的是 A 的快照。B 作为
非来源 View 只执行 change mapping，不使用 A 的快照。

如果来源 View 已关闭，Content 分量仍应正常遍历。所有现存 View 只执行
change mapping，已关闭 View 的快照安全跳过。

没有 View participant 的记录只应用 Content 分量，并将 change 映射到
所有绑定该 Content 的 View。

## 7. 处理顺序总览

| 编号 | 优先级 | 状态 | 工作项 |
| --- | --- | --- | --- |
| T01 | P0 | 待处理 | 确认事务作用域和语义不变量 |
| T02 | P0 | 待处理 | 建立静态配对的事务数据模型 |
| T03 | P0 | 待处理 | 建立统一生命周期和 checkpoint 路径 |
| T04 | P0 | 待处理 | 建立 Mode 转换与 Visual 操作解析 |
| T05 | P0 | 待处理 | 迁移 Buffer 历史并修复 Vim undo 语义 |
| T06 | P1 | 待处理 | 验证多 Content 和多 View 扩展性 |

## 8. 工作项明细

### T01：确认事务作用域和语义不变量

目标：

- 在 `docs/design/` 编写统一事务设计；
- 确认每个 `ContentId` 拥有独立历史流；
- 确认 Manager 是唯一历史和活动事务权威；
- 确认活动事务 owner 和跨 View 抢占规则；
- 确认有 View 和无 View participant 的语义；
- 区分 runtime rollback 与 undo/redo history；
- 确认事务 identity、分支和保存状态的关系；
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
- Buffer View 数据支持 `Source` 和 `None`；
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
- Content 是事务是否提交的最终权威；
- Mode 可以通过 Content 返回事务边界请求；
- 按键和 timeout 复用同一条提交请求路径；
- Save、View close 和跨 View 编辑使用统一 checkpoint；
- 请求可以表达事务动作与 Mode/Content 操作的顺序；
- 提交逻辑不依赖事件循环迭代或命令 Sequence 结束。

完成标准：

- 按键和 timeout 都可以提交同一事务；
- View B 的编辑不会并入 View A 的活动事务；
- Save 能捕获完整 outer record 并继续连续编辑；
- Content 可以拒绝无效提交或消除空事务；
- 失败操作不会留下部分 Content 或 View 状态；
- redo 分支只在成功提交非空事务后截断。

### T04：建立 Mode 转换与 Visual 操作解析

目标：

- 不建立所有 Mode 共用的 enter/leave hook；
- Vim 在自身实现内集中处理状态转换；
- Mode 转换显式携带必要的 View state adjustment；
- Visual operator 在退出 Visual 前生成 resolved operation；
- resolved operation 保存 range、shape、operator 和 cursor intent；
- Visual delete 在 Normal 和 collapsed selections 下形成事务；
- Visual change 将事务延续到后续 Insert 结束。

完成标准：

- Vim 进入 Normal 时 live selections 保持合法；
- Visual delete/change 不再依赖退出后的活动 selections；
- selection 压缩不再伪装成普通文本编辑；
- 不需要转换副作用的新 Mode 不实现额外 hook；
- App 和事务核心没有 Vim 特判。

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
- 验证非来源 View undo 和来源 View 关闭场景；
- 验证没有 View participant 的 Content 事务。

完成标准：

- 新 Content 只扩展静态 Content 分派；
- 新 Mode 不需要实现历史 payload 接口；
- 不需要修改 Manager 中的具体 Mode 分支；
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
- `gv` 的完整用户行为实现；
- 对瞬时 Mode 输入状态执行 undo/redo。

## 10. 本版默认决策

本版 roadmap 按以下决策等待审核：

1. 每个可事务化 `ContentId` 拥有独立历史流，Manager 只是统一实现，
   不建立 App 全局 undo 顺序。
2. Content 数据和 View 数据在同一静态枚举变体中配对。
3. View participant 可以是来源 View，也可以不存在；不得伪造 View。
4. 第一阶段只记录 Content 和 selections，不记录或恢复 Mode 状态。
5. 每个 Content 最多一个活动文本事务，并显式记录 owner。
6. 跨 View 编辑、View close 和 Save 必须先走统一 checkpoint。
7. 非来源 View 发起 undo 时，来源 View 恢复快照，发起 View 只执行
   Content change mapping。
8. 事务框架不提供所有 Mode 共用的生命周期 hook。Mode 转换副作用使用
   显式 result 或 adjustment 表达。
9. Visual delete 在 Normal 下提交；Visual change 延续到 Insert 结束后
   再于 Normal 下提交。
10. Mode-only 和普通 selection-only 变化不形成 undo entry。
11. Manager 是 history cursor 和 redo 分支的唯一权威，Buffer 只维护
    text state identity。
