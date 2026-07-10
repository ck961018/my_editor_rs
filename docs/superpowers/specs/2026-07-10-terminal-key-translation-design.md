# 终端按键翻译边界设计

日期：2026-07-10

## 目标

移除 `protocol` 对 `crossterm` 的依赖，将终端原始按键到编辑器中立
`KeyEvent` 的转换归属到 `terminal` 层。此次只调整依赖和测试位置，所有已有
按键翻译行为必须保持不变。

## 背景

`protocol::key_event` 目前同时承担两种职责：定义编辑器使用的中立按键模型，
以及把 `crossterm::event::KeyEvent` 转换为该模型。这使 `protocol` 依赖具体的
终端输入库，违背 `protocol -> std` 的依赖方向。

按键翻译是终端适配细节。GUI、远程或测试前端应各自把原始输入映射为
`protocol::KeyEvent`，而不是依赖 crossterm 适配器。

## 范围

包含：

- 将 crossterm 按键翻译实现和其单测迁移到 `terminal`。
- 让 `terminal::input` 调用该适配器。
- 移除 `protocol` 中全部 crossterm 引用。
- 保持现有按键事件、Release 过滤和 Resize 处理语义。

不包含：

- 新增 GUI、远程前端或通用输入翻译 trait。
- 扩展 Unicode、媒体键或其他尚未支持的键码语义。
- 修改 keymap、dispatcher 或 `KeyEvent` 的中立数据模型。

## 模块职责

### protocol::key_event

保留以下中立类型及其构造、查询 API：

- `KeyModifiers`
- `ArrowKey`
- `KeyCode`
- `KeyEvent`

该模块不导入 `crossterm`，不公开任何终端库类型，也不包含输入库特定的翻译
测试。

### terminal::key_translate

新增 `src/terminal/key_translate.rs`，由 `src/terminal/mod.rs` 以私有模块方式
声明。它提供 crate 内部可见的适配函数：

```rust
pub(crate) fn translate_key(
    key: crossterm::event::KeyEvent,
) -> crate::protocol::key_event::KeyEvent;
```

这是唯一同时依赖 crossterm 原始按键和协议按键模型的模块。翻译规则和对应
单元测试全部归属这里。

### terminal::input

`Input` 继续负责事件流处理：读取 `EventStream`，忽略 `KeyEventKind::Release`，
保留 Press 和 Repeat，将 Resize 转换为 `FrontendEvent::Resize`。对于可处理的
键盘事件，它调用 `key_translate::translate_key` 并构造 `FrontendEvent::Key`。

它不定义具体键码映射规则。

## 数据流

```text
crossterm::event::Event
  -> terminal::input::map_event
  -> terminal::key_translate::translate_key
  -> protocol::KeyEvent
  -> frontend / app / keymap / dispatcher
```

## 行为守恒

`translate_key` 是总函数。无法表达的 crossterm 键仍转换为
`KeyCode::Unknown`，并保留 Ctrl、Alt、Shift 修饰键；不引入新的错误类型或
fallback。

以下已有映射必须逐值保持：

- 可打印 ASCII 字符与空格。
- Ctrl 字符的小写归一。
- Ctrl、Alt、Shift 修饰键。
- Backspace、Enter、Escape、方向键和 Function 键。
- 不支持键的 `Unknown` 映射。
- Release 忽略、Press 和 Repeat 保留、Resize 转换。

## 测试与验收

`terminal::key_translate` 的单元测试覆盖全部键码翻译规则。`terminal::input`
的测试只覆盖事件流和事件种类处理。

实施完成时必须执行：

```text
cargo test terminal::key_translate
cargo test terminal::input
rg "crossterm" src/protocol
cargo test
cargo clippy --all-targets --all-features
```

`rg "crossterm" src/protocol` 必须无匹配；其余命令必须成功。
