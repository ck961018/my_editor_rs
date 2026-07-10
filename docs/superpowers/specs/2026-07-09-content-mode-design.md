# Content Mode 与最小 Vim 交互模型设计

日期：2026-07-09

## 1. 背景

当前按键链路已经具备 content 捕获链、前缀 keymap、`ResolvedOperation`
目标解析和 `View` 归属 selections：

```text
KeyEvent
-> Dispatcher::dispatch(...)
-> ResolvedOperation
-> App::execute_operation(...)
-> executor::execute(Operation, content, selections)
```

但 `Operation` 现在同时表达编辑动作、保存、退出、焦点切换和预留多
selection 命令，语义过宽。`Buffer::default_binding` 也硬编码普通字符插入，
无法表达 Vim Normal 模式下“普通字符不插入”的输入模型。

目标是为 content 增加更灵活的 mode 能力，并在第一版实现最小 Vim 行为。
本设计不复刻 Emacs major mode；mode 是更轻量的输入与行为扩展单元。

## 2. 目标

- 引入通用 `Mode` trait。具体 mode 是实例，不能用 enum 固定。
- 允许具体 content 自行定义 mode layer。第一版只有 `Buffer` 使用 mode。
- `Buffer` 第一版只有 `Base` layer，默认启用 `vim`。
- 实现最小 Vim：Normal/Insert 状态、`h/j/k/l`、`i`、`Esc`、插入态输入。
- 将 keymap 绑定值从 `Operation` 改为 `Command`。
- 将现有 `Operation` 收窄/迁移为 `TextCommand`，不再混入保存、退出、
  mode 状态或模糊预留命令。
- 保持 `ContentHandler` object-safe，继续支持
  `HashMap<ContentId, Box<dyn ContentHandler>>`。

## 3. 非目标

- 不实现脚本语言或外部插件加载。
- 不实现完整动态 mode registry。内置 mode 仍需通过同一个 `Mode` trait。
- 不给 `StatusBar` 引入 mode runtime。
- 不实现 Vim count、operator、Visual、Command-line、`a/o/dd/x` 等行为。
- 不设计 language mode、readonly policy、auto-pair 或 typing hook pipeline。
- 不把 mode state 暴露成全局 `Command` 数据结构。

## 4. 核心概念

### 4.1 Mode

`Mode` 是可扩展实例。第一版内置 Rust struct 实现，未来脚本 mode 可通过
adapter 实现同一个 trait。

```rust
pub struct ModeId(&'static str);
pub struct ModeActionId(&'static str);

pub trait Mode {
    fn id(&self) -> ModeId;
    fn label(&self) -> &str;
    fn keymap(&self) -> &Keymap;
    fn typing(&self, key: KeyEvent) -> Option<Command>;
    fn handle_mode_command(&mut self, action: ModeActionId);
}
```

`typing` 表示普通输入兜底，由当前 base mode 决定。第一版不允许所有
extension mode 争抢普通输入；后续如需 auto-pair 或 readonly，可单独设计
typing hook 或 policy 层。

`handle_mode_command` 让 mode 自己解释内部动作。例如 Vim 的
`enter-insert`、`enter-normal` 只由 `VimMode` 解释，Normal/Insert 状态不泄漏
到全局 command 类型。

### 4.2 Content 自定义 layer

`ModeLayer` 不是全局 enum，而是具体 content 自己的静态结构。第一版只为
`Buffer` 定义：

```rust
enum BufferModeLayer {
    Base,
}

struct BufferModes {
    base: Box<dyn Mode>,
}
```

`StatusBar` 第一版不持 mode。未来如果某类 content 需要自己的层级，可定义
自己的 layer enum 和 mode 容器，不要求所有 content 都有 language/policy 等
固定层。

## 5. Command 模型

keymap 不再绑定 `Operation`，而是绑定更高层的 `Command`：

```rust
pub enum Command {
    App(AppCommand),
    Content(ContentCommand),
    Noop,
}

pub enum AppCommand {
    Quit,
    FocusNext,
    FocusPrev,
}

pub enum ContentCommand {
    Text(TextCommand),
    Save,
    Mode { mode: ModeId, action: ModeActionId },
}

pub enum TextCommand {
    MoveBy { chars: isize, lines: isize },
    MoveLeftBy(usize),
    MoveRightBy(usize),
    MoveUpBy(usize),
    MoveDownBy(usize),
    MoveTo { char_idx: usize, line_idx: usize },

    ExtendLeftBy(usize),
    ExtendRightBy(usize),
    ExtendUpBy(usize),
    ExtendDownBy(usize),

    InsertText(String),
    Delete(isize),
    CollapseSelections,
}
```

语义边界：

- `Command` 是 keymap 可绑定的顶层命令。
- `AppCommand` 不依赖 content。
- `ContentCommand` 作用于某个 content。
- `TextCommand` 是文本 content 的编辑会话命令，覆盖文本和 selections。

当前 `Operation` 将被删除或迁移为 `TextCommand`。以下旧变体不继续保留为
编辑操作：

- `Save`、`Quit`、`FocusNext`、`FocusPrev` 迁入 `Command`。
- `Cancel` 不保留；旧 `Esc` 行为由 `TextCommand::CollapseSelections` 明确
  表达。
- `AddAtNextMatch`、`RemoveSecondary` 暂不保留。多 selection 功能需要新设计
  时再以明确 command 加回。

## 6. Keymap 与 dispatcher

`KeyBinding` 改为：

```rust
pub enum KeyBinding {
    Command(Command),
    Prefix(Keymap),
}
```

`Dispatcher` 返回已解析目标的 `DispatchCommand`：

```rust
pub enum DispatchCommand {
    App(AppCommand),
    Content {
        command: ContentCommand,
        content: ContentId,
    },
    ViewContent {
        command: ContentCommand,
        space: SpaceId,
        content: ContentId,
    },
    Noop,
}
```

解析规则：

```text
Command::App(_)                  -> DispatchCommand::App
Command::Content(Text(_))        -> DispatchCommand::ViewContent
Command::Content(Save)           -> DispatchCommand::Content
Command::Content(Mode { .. })    -> DispatchCommand::Content
Command::Noop                    -> DispatchCommand::Noop
```

keymap 中的 `Command` 不携带 `ContentId` 或 `SpaceId`。dispatcher 继续基于
focused space、scene 和 capture chain 解析运行时 target。pending prefix 仍需
保留起始 keymap 的 source，用于解析 target，但 source 不进入
`DispatchCommand` 的执行契约。

捕获顺序保持：

```text
focused content
-> parent host content
-> global keymap
-> focused content typing fallback
```

其中 content keymap 由 `ContentHandler::resolve_key` 统一提供。

## 7. ContentHandler 变化

`ContentHandler` 继续保持 object-safe。新增输入解析和 mode 命令入口：

```rust
pub trait ContentHandler {
    fn resolve_key(&self, key: KeyEvent) -> Option<Command> {
        self.keymap().lookup_command(key)
    }

    fn handle_mode_command(&mut self, _mode: ModeId, _action: ModeActionId) {}

    // 现有 keymap/as_buffer/as_status_bar/buffer_mut 等保留并按新 Command 调整。
}
```

`Buffer::resolve_key` 不再直接使用旧 `default_binding`：

```text
1. 查 modes.base.keymap()
2. 未命中则 modes.base.typing(key)
```

`StatusBar` 可使用默认实现，返回 `None`。

## 8. Buffer 内置 mode

### 8.1 PlainEditMode

`plain-edit` 保留当前非 modal 行为，供测试和未来配置使用。

```text
Enter      -> Content(Text(InsertText("\n")))
Backspace  -> Content(Text(Delete(-1)))
Left       -> Content(Text(MoveLeftBy(1)))
Right      -> Content(Text(MoveRightBy(1)))
Up         -> Content(Text(MoveUpBy(1)))
Down       -> Content(Text(MoveDownBy(1)))
Shift+Left -> Content(Text(ExtendLeftBy(1)))
Shift+Right-> Content(Text(ExtendRightBy(1)))
Shift+Up   -> Content(Text(ExtendUpBy(1)))
Shift+Down -> Content(Text(ExtendDownBy(1)))
Esc        -> Content(Text(CollapseSelections))
typing     -> plain char => Content(Text(InsertText(char)))
```

### 8.2 VimMode

`vim` 是默认 base mode。它是一个 `Mode` 实例，自己持有内部状态：

```rust
enum VimState {
    Normal,
    Insert,
}

struct VimMode {
    state: VimState,
    normal_keymap: Keymap,
    insert_keymap: Keymap,
}
```

`VimState` 是 `VimMode` 私有实现细节，不进入 `Command`。

Normal：

```text
h   -> Content(Text(MoveLeftBy(1)))
j   -> Content(Text(MoveDownBy(1)))
k   -> Content(Text(MoveUpBy(1)))
l   -> Content(Text(MoveRightBy(1)))
i   -> Content(Mode { mode: "vim", action: "enter-insert" })
Esc -> Noop
typing -> None
```

Insert：

```text
Esc       -> Content(Mode { mode: "vim", action: "enter-normal" })
Enter     -> Content(Text(InsertText("\n")))
Backspace -> Content(Text(Delete(-1)))
Left      -> Content(Text(MoveLeftBy(1)))
Right     -> Content(Text(MoveRightBy(1)))
Up        -> Content(Text(MoveUpBy(1)))
Down      -> Content(Text(MoveDownBy(1)))
Shift+Left -> Content(Text(ExtendLeftBy(1)))
Shift+Right-> Content(Text(ExtendRightBy(1)))
Shift+Up   -> Content(Text(ExtendUpBy(1)))
Shift+Down -> Content(Text(ExtendDownBy(1)))
typing -> plain char => Content(Text(InsertText(char)))
```

第一版不支持 `a`、`o`、`dd`、count、Visual 或命令行模式。

## 9. App 执行规则

`App::execute_operation` 改为执行 `DispatchCommand`：

```text
DispatchCommand::App(Quit)
  -> tasks.cancel()

DispatchCommand::App(FocusNext | FocusPrev)
  -> 暂时空实现

DispatchCommand::Content { Save, content }
  -> spawn_save(content)

DispatchCommand::Content { Mode { mode, action }, content }
  -> contents[content].handle_mode_command(mode, action)

DispatchCommand::ViewContent { Text(cmd), space, content }
  -> 获取 Buffer + View.selections
  -> execute_text_command(cmd, buffer, selections)

DispatchCommand::Noop
  -> 不做事
```

`execute_text_command` 由当前 `executor::execute` 演进而来，处理文本和
selection 命令。`Save`、mode 命令和 app 命令不进入 text executor。

如果 `TextCommand` 错误分发到非 Buffer content，保持内部不变量风格：
测试中应暴露，执行路径可 no-op 或 `debug_assert!`，具体实现计划再定。

## 10. 状态栏

第一版状态栏仍显示已有文件名、modified 和 status message。mode label 不作为
本设计的必需 UI 输出，避免为 Vim 最小行为扩大 `ContentQuery` 范围。

后续如需显示 `NORMAL`/`INSERT`，可以扩展 `StatusBarData` 或新增 mode status
query；本设计只要求 mode trait 暴露 `label()`，为未来使用保留入口。

## 11. 文件影响范围

预计修改：

- `src/core/command.rs`：新增 `Command`、`AppCommand`、`ContentCommand`、
  `TextCommand`。
- `src/core/operation.rs`：删除或替换为 `command.rs`；所有引用迁移。
- `src/core/keymap.rs`：`KeyBinding::Command`，`bind` 接收 `Command`。
- `src/core/mode.rs`：新增 `Mode`、`ModeId`、`ModeActionId`。
- `src/core/buffer.rs`：新增 `BufferModes`、内置 `PlainEditMode`、
  `VimMode`，默认 vim；`ContentHandler::resolve_key` 和
  `handle_mode_command` 实现。
- `src/core/content.rs`：新增 object-safe 输入解析与 mode command 方法。
- `src/app/dispatcher.rs`：`ResolvedOperation` 改为 `DispatchCommand`，
  target 解析按 `Command` 类型重写。
- `src/app/executor.rs`：改为 `execute_text_command(TextCommand, ...)`。
- `src/app/mod.rs`：执行 `DispatchCommand`。
- 测试按新命名与默认 Vim 行为调整。

## 12. 测试策略

### core

- `Keymap` 绑定/查找 `Command`，prefix 仍可嵌套。
- `PlainEditMode` 复现旧行为：字符插入、方向移动、Shift+Arrow、Esc collapse。
- `VimMode` Normal：
  - `h/j/k/l` 移动。
  - `i` 触发 mode command 并进入 Insert。
  - 普通字符 typing 返回 `None`。
  - `Esc` 为 `Noop`。
- `VimMode` Insert：
  - 普通字符插入。
  - `Esc` 回 Normal。
  - Enter/Backspace/Arrow/ShiftArrow 行为正确。
- `TextCommand::CollapseSelections` 折叠并保留 primary selection。

### app/dispatcher

- content keymap 命中返回 `DispatchCommand::ViewContent` 或 `Content`。
- global quit 返回 `DispatchCommand::App(Quit)`。
- prefix key 保留 source 并解析到正确 target。
- typing fallback 在 content 和 global 都未命中后运行。
- 默认 `Buffer::new()` 为 Vim Normal，直接输入普通字符不修改文本。
- `i` 后输入字符再 `Esc`，文本被插入且状态回 Normal。

### 集成

- 默认 Vim 流程：`i`、`a`、`b`、`Esc`、`h`、`Ctrl+Q` 后文本为 `ab`，
  光标按 Vim normal 移动。
- `plain-edit` 测试构造保留旧行为，避免非 modal 行为退化。
- `Ctrl+S` 保存仍按 focused content 保存。
- selection 编辑和 highlight 已有行为不因命令迁移退化。

## 13. 接受标准

- keymap 绑定 `Command`，不再绑定旧 `Operation`。
- `Operation` 不再作为混合顶层命令存在；文本编辑语义迁入 `TextCommand`。
- `Save`、`Quit`、`FocusNext`、`FocusPrev`、mode 状态命令不出现在文本命令中。
- `Mode` 是 trait object/实例模型，不能实现为 mode enum。
- `Buffer` 默认使用 `vim`，初始为 Normal。
- Vim 最小行为可运行并有测试覆盖。
- `StatusBar` 不被迫拥有 buffer language/policy/base 等 layer。
- `cargo test` 通过；涉及命令边界迁移时运行
  `cargo clippy --all-targets --all-features`。
