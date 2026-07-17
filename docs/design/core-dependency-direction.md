# Core 依赖方向设计

**状态：** 已实施
**日期：** 2026-07-17
**对应路线图：** R04

## 1. 目标

消除 `command <-> mode`、`buffer <-> motion` 以及
`command -> mode -> keymap -> command`，使基础数据结构不依赖具体编辑命令，文本运动解析
不反向依赖 Buffer 实体，同时保留 Mode 对 Vim grammar 的所有权。

## 2. 依赖方向

```text
mode_name <- command <- mode
                    \-> keymap

text data <- motion <- buffer
```

- `mode_name` 只保存动态边界使用的 owned `ModeName` 与 `ModeActionName`；`command` 和
  `mode` 都可依赖它，二者不再互相借用名称类型。
- `Keymap<A>` 不提供默认 action 类型，也不认识 `Command`、`ContentCommand` 或
  `EditCommand`。把编辑命令包装成顶层命令的 convenience API 归 Mode 的 keymap 构造代码。
- `motion` 拥有 operator/motion 数据类型以及解析它们所需的词法、行边界算法；Buffer
  调用 Motion，不再被 Motion 反向调用。

## 3. 不变项

- `Command` 的执行归属和 R03 契约不变。
- `Mode` 仍持有 Vim action、count、operator、capture 和 keymap 构造。
- Buffer 仍持有文本、selection、编辑与历史状态，不引入 Vim 按键语义。
- 只移动或收紧依赖，不改变运动、编辑和输入行为。

## 4. 验收

- `core::keymap` 不导入任何具体 core command；
- `core::command` 不导入 `core::mode`；
- `core::motion` 不导入 `core::buffer`；
- 现有行为测试通过，Motion 的词法与行边界辅助拥有直接单元测试；
- 架构文档和 roadmap 与实际依赖一致。
