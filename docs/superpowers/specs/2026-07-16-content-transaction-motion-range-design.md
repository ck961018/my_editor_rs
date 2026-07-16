# Content 事务与 Motion/Range 设计

> 日期：2026-07-16  
> 状态：已确认

## 目标与范围

阶段二建立可组合、可撤销的 Content 事务，以及不依赖 Vim grammar 的文本 Motion/Range。
本次完成机制与最小垂直切片：

- Content 级事务抽象，Buffer 实现文本事务；
- active transaction、线性 undo/redo、savepoint 与异步保存；
- 文本 change set、位置映射和多 View selection 变换；
- 纯 Motion 解析、charwise/linewise Range 和通用 delete operator；
- Vim `dd`、`dw`、`d$`、`d0`、`u`、`Ctrl+R`；
- Insert 会话作为一个 undo 单元。

register、yank/change operator、Visual、text object、blockwise、repeat、macro 和 undo tree 后置。

## 所有权

事务能力属于 `Content`，不是 `Buffer`。每种 Content 可定义不同的关联事务类型；
`Buffer` 的事务内容为 `TextChangeSet`，active transaction 状态由其实现内部封装。`Content` 继续是静态闭合枚举，
`ContentStore` 继续是唯一内容表，app 不识别具体 Content 变体。

```rust
trait TransactionalContent {
    type Transaction;
    type Change;

    fn begin_transaction(&mut self) -> TransactionResult;
    fn commit_transaction(&mut self) -> TransactionResult;
    fn rollback_transaction(&mut self) -> Result<Option<Self::Change>, TransactionError>;
    fn undo(&mut self) -> Result<Option<Self::Change>, TransactionError>;
    fn redo(&mut self) -> Result<Option<Self::Change>, TransactionError>;
}
```

具体 trait 签名可按 Rust 借用和现有 `ContentResult` 接线调整，但必须保持关联类型、静态分派和
Content-owned lifecycle。transaction 不保存 `ViewId`、selection 或 Vim 状态。

## 文本事务

`TextTransaction` 使用相对事务起始快照的规范化 change stream：

```rust
enum TextChange {
    Retain(usize),
    Delete(usize),
    Insert(String),
}

struct TextChangeSet {
    before_len: usize,
    after_len: usize,
    changes: Vec<TextChange>,
}
```

长度和位置均为 Unicode scalar value 的 char offset。ChangeSet 提供：

- validation 与 apply；
- invert；
- compose；
- `map_position(offset, Affinity::{Before, After})`；
- 相邻同类操作合并，以及 insert/delete 的抵消压缩。

change 基于同一个旧快照，规范化后按旧坐标递增且不重叠。同一旧 offset 最多一个 insert；
冲突由语义层预先处理，事务层发现越界、CRLF 非法边界或冲突时原子失败。

## Active transaction 与历史

Buffer 同时最多有一个 active transaction。事务内每次按键先验证一笔 delta，再立即修改 Rope，
并将 delta compose 到 pending ChangeSet；“未 commit”只表示尚未形成 history checkpoint。

- Insert 模式从进入到退出使用同一个 active transaction；
- 普通修改在没有 active transaction 时隐式执行 `begin -> apply -> commit`；
- `o/O/s/C/S` 的初始修改与随后 Insert 输入属于同一事务；
- save、undo/redo、View/Content 切换先 commit active transaction；
- 空事务不产生 state、revision 或 history entry。

历史条目保存 forward/inverse ChangeSet 和前后状态：

```rust
struct TextHistoryEntry {
    forward: TextChangeSet,
    inverse: TextChangeSet,
    before_state: TextStateId,
    after_state: TextStateId,
}
```

阶段二使用线性 history cursor；undo 后的新 commit 截断 redo。undo/redo 应用既有 inverse/forward，
不创建新历史条目。

`TextStateId` 只标识已 commit 的稳定状态且永不复用。`modified` 为派生值：active transaction
非空时为 true，否则比较 current/saved state。Buffer 的运行时 revision 在每次可见文本变化时递增，
用于渲染和异步保存排序，不承担 savepoint 身份。

异步保存先将 active transaction commit 为 checkpoint，再捕获 `Rope snapshot + revision + TextStateId`；
若仍处于 Insert 模式则立即打开下一笔 active transaction，使保存后的继续输入形成新的 undo 单元。
保存成功把该 state 标为 saved；若当前已继续编辑，仍保持 modified。

## Selection 变换

事务和历史保持 View-neutral。每次可见文本变化返回 `TextChangeMap`，app 将其应用到所有绑定该
Content 的 View state。发起 View 可带额外 selection intent，最后覆盖默认变换结果。

默认策略：

- collapsed selection 两端均使用 `After`，保持 collapsed；
- 非空 selection 跟随原内容：逻辑左边界 `After`、右边界 `Before`；
- 变换后保持原 anchor/head 方向；
- 被删除吞没的端点落到删除起点；
- 最终位置不得位于 CRLF 中间。

所有目标先在同一不可变文本快照上求值；语义层合并 operator 的重叠/相邻 Range，事务失败时不得
改变 Rope、history、revision 或任何 selection。

## Motion、Range 与 operator

Motion 是 `core` 文本领域内的纯计算，不进入 protocol，也不修改 Buffer 或 View：

```rust
struct MotionOutcome {
    destination: TextOffset,
    covered: TextRange,
}

enum TextRange {
    Charwise { start: TextOffset, end: TextOffset },
    Linewise { start_line: usize, end_line: usize },
}
```

`destination` 服务普通光标移动，`covered` 是 operator 可直接消费的已求值目标。
inclusive/exclusive 由具体 motion 算法内部消解，不成为公共类型。charwise 使用半开 char span；
linewise 使用半开逻辑行区间，由具体 operator lowering 为 char changes。以后 blockwise 作为新变体加入。

Vim mode 只保存 operator/count/等待字符等 grammar 状态。它输出通用文本命令：

```text
Vim grammar -> TextMotion/TextTarget -> MotionOutcome/TextRange
            -> Delete operator -> TextTransaction -> Buffer
```

Buffer 不知道按键 `d`、Vim prefix、register 或 grammar。`dd` 生成 linewise target；`dw`、`d$`、
`d0` 生成对应 Motion target。多个 selection 的 ranges 先排序求并集，再生成一次删除 transaction。

## Mode 与 Content lifecycle

现有 `Mode::execute -> Option<ContentCommand>` 升级为可返回有序 Content commands 的 outcome，
使 mode action 能表达 transaction boundary 与初始编辑的顺序，同时保持一个 binding 对应一个
结构化 action。事务控制是中立 Content command，不由 app 检查 Vim 状态。

```text
i       -> BeginTransaction
Esc     -> CommitTransaction
o/O/s   -> BeginTransaction, initial edit
a/I/A   -> movement, BeginTransaction
dd/dw   -> generic delete command (implicit one-command transaction)
```

## 错误与兼容

外部输入导致的不可执行 motion 是 handled no-op；内部不变量破坏返回结构化错误并保持原子性。
保留现有命令行为，逐步让已有移动和删除复用 Motion/Range；不为迁移方便把 Vim 专属概念塞入
`EditCommand`、Buffer 或 app。

## 验收

- `iabc<Esc>` 后一次 Normal `u` 删除完整 `abc`，`Ctrl+R` 恢复；
- `dd`、`dw`、`d$`、`d0` 通过通用 target/operator/transaction 路径；
- LF/CRLF、无尾换行、Unicode、多 selection、重叠范围和 no-op 有测试；
- 两个 View 共享 Buffer 时，编辑、undo、redo 都精确变换两个 View selections；
- undo 回 saved state 后 `modified == false`，redo 后重新为 true；
- stale async save 不错误清除较新状态的 modified；
- `cargo fmt`、`cargo test`、`cargo clippy --all-targets --all-features` 和
  `git diff --check` 通过。
