# 命令执行归属与 Sequence 契约

**状态：** 已确认
**日期：** 2026-07-17
**对应路线图：** R03

## 1. 背景

当前 `ContentCommand` 同时包含三类实际执行者不同的命令：

- `Edit`、事务、undo/redo、`Save` 由 `Content` 执行；
- `Mode` 由目标 `View` 持有的 `ModeInstance` 执行；
- `Viewport` 由 Frontend 根据实际布局尺寸解析，再转换为编辑命令。

`Content::execute` 因而必须对 `Mode` panic、对 `Viewport` 返回 `NotHandled`。
`Sequence(Vec<ContentCommand>)` 还可以包含这些不兼容命令，导致前序编辑已经生效、
后序命令才返回 `NotHandled`，而 `ContentStore` revision 没有递增。

本设计只修正命令归属、执行上下文和组合契约，不改变 Mode、Content、View、Frontend
现有所有权，也不提前处理 R04 的 core 模块依赖环。

## 2. 设计原则

- 命令分类按实际执行者命名，不按“恰好需要同一个 ID”合并。
- `ContentViewState` 是部分 `ContentCommand` 的执行上下文，不形成第二套内容命令。
- 顶层只增加真实存在的执行边界，不增加 `ViewCommand` 等混合路由分类。
- `Sequence` 是有序命令集合，不等价于事务；需要回滚语义时仍使用
  `TransactionCommand`。
- 非法组合必须在执行任何成员前被拒绝。
- Mode action 的内部状态变化与向外产生的命令分开表达。
- keymap 与 Mode 产生的命令必须复用同一套目标解析与执行链。

## 3. 命令模型

```rust
pub enum Command {
    App(AppCommand),
    Content(ContentCommand),
    Mode(ModeCommand),
    Viewport(ViewportCommand),
    Noop,
}

pub struct ModeCommand {
    pub mode: ModeName,
    pub action: ModeActionName,
}

pub enum ContentCommand {
    Edit(EditCommand),
    Transaction(TransactionCommand),
    Undo,
    Redo,
    Sequence(ContentSequence),
    Save,
}
```

`ContentCommand` 中所有命令都由 `Content::execute` 执行，但执行上下文不同：

| 命令 | 执行输入 | 原因 |
| --- | --- | --- |
| `Save` | `ContentInput::Command` | 只操作共享 Content，不依赖某个 View |
| `Edit`、事务、undo/redo、`Sequence` | `ContentInput::View` | 需要目标 View 的 selection 等 `ContentViewState` |

该差异由 `ContentCommand` 的穷尽分类接口统一维护，Dispatcher 据此补充运行时目标。
`Content::execute` 同时校验输入与命令上下文匹配，防止错误接线被静默接受。

## 4. Sequence 契约

`Sequence` 使用经过验证的容器，而不是公开的任意 `Vec<ContentCommand>`：

```rust
pub struct ContentSequence(Vec<ContentCommand>);
```

构造规则：

- 允许 `Edit`、`Transaction`、`Undo`、`Redo`；
- 允许已经验证的嵌套 `Sequence`，实现可以在构造时展平；
- 拒绝 `Save`，因为保存产生独立 `ContentEffect`，也不使用 `ContentViewState`；
- `Mode` 和 `Viewport` 不属于 `ContentCommand`，类型上无法进入 Sequence；
- 空 Sequence 是需要 View state 的 handled no-op。

`ContentSequence` 只保证所有成员可以在同一次 `ContentInput::View` 分派中有序执行，
不承诺任意成员失败后的自动回滚。当前 Buffer 对合法成员进行穷尽处理，因此合法 Sequence
执行过程中不会出现 `NotHandled`。需要编辑回滚和 undo 单元时，继续由 Content transaction
承担。

## 5. Mode 执行结果

Mode action 直接修改自己的 `ModeState`，并可返回一个普通顶层 `Command`，交给与 keymap
命令相同的 Dispatcher 解析入口：

```rust
fn execute(
    &self,
    state: &mut dyn ModeState,
    action: &ModeActionName,
) -> Result<Option<Command>, ModeError>;
```

- `Ok(None)`：action 已处理，只改变 Mode 私有状态；
- `Ok(Some(Command::Content(command)))`：交给 Content 执行；
- `Ok(Some(Command::Viewport(command)))`：交给 Frontend 解析；
- `Ok(Some(Command::App(command)))`：交给 App 执行，例如未来实现的 focus space 切换；
- 也允许返回其他顶层命令，以保持用户定义 Mode 的组合能力；
- `Err(ModeError)`：mode/action 未知，或注册表与实现不一致。

不另建 `ModeEffect`，因为它会复制并限制顶层 `Command` 的能力集合。Mode 仍不直接调用
App、Content 或 Frontend，只产生命令；命令的目标和执行者继续由 Dispatcher 与 App 决定。

## 6. Dispatcher 与执行链

```text
Command::App
  -> App

Command::Content
  -> Dispatcher 解析 ContentId
  -> Save: ContentInput::Command
  -> 其他: 同时解析 ViewId，并使用 ContentInput::View

Command::Mode
  -> Dispatcher 解析 ViewId
  -> View::ModeInstance
  -> optional Command
  -> 以原始 ViewId 重新进入同一 Dispatcher 目标解析入口

Command::Viewport
  -> Dispatcher 将全局绑定解析到 focused View，或保留来源 ViewId
  -> Frontend 解析 pane 高度
  -> ContentCommand::Edit
  -> ContentInput::View
```

Mode 产生的 `ContentCommand` 仍按该命令自己的上下文分类执行；不能假设所有 Mode 输出
都需要 `ContentViewState`。全局 keymap 也可以直接绑定 `Command::Viewport`，由 Dispatcher
解析到 focused View。Viewport 的布局所有权继续保留在 Frontend/TUI，不迁入 core。

执行链使用有固定上限的迭代过程，而不是递归调用。这样 Mode 可以组合其他命令，同时恶意或
错误的 `Mode -> Mode` 循环会返回明确诊断，不会造成栈溢出或无限循环。

## 7. 错误策略

unknown mode、unknown action、目标 View 没有活动 Mode，以及已注册 action 未被 Mode 实现，
都属于当前内部命令接线错误。它们返回带 mode/action 信息的结构化 `ModeError`，由 App 转换为
带诊断信息的执行错误；不得再返回 `Ok(())` 静默吞掉。

Mode 连续产生的命令链超过执行上限时，App 返回包含上限信息的 `InvalidData` 错误。

未来脚本或远程输入如果需要非致命诊断，应在对应外部输入边界把 `ModeError` 转换为协议或 UI
错误。本次不为尚未存在的输入源扩展状态栏和远程错误协议。

## 8. 验收标准

- `ContentCommand` 只包含实际由 Content 执行的命令；
- `Mode` 与 `Viewport` 是独立的顶层 `Command` 变体；全局 keymap 可以直接绑定 viewport；
- Mode 返回 `Option<Command>`，可以产生 Content、Viewport、App 等顶层操作；
- keymap 与 Mode 输出复用同一个 Dispatcher 目标解析入口，并保留 Mode 的来源 View；
- 只有一个 `ContentCommand` 类型，是否需要 `ContentViewState` 由执行上下文分类决定；
- 非法 Sequence 无法绕过验证容器，且在任何 Content/View 修改前被拒绝；
- `Content::execute` 不再包含 Mode panic 或 Viewport `NotHandled` 分支；
- unknown Mode/action 返回明确错误；
- 递归 Mode 命令链有明确的执行上限和诊断；
- viewport 仍由 Frontend 解析后降为 `ContentCommand::Edit`；
- `cargo test`、`cargo clippy --all-targets --all-features` 和 `git diff --check` 通过。
