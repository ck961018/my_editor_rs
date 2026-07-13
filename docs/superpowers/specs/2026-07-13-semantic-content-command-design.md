# 语义 Content 命令与适配结果设计

**日期：** 2026-07-13
**状态：** 已实施

## 目标

- Mode action 返回 `ContentCommand`，不再把扩展边界固定为 `EditCommand`。
- Content 执行明确返回 `Handled(ContentEffect)` 或 `NotHandled`。
- 不按具体 Content 类型维护 Mode allowlist；适配性由 Content 是否处理实际命令决定。

## 命令边界

`ContentCommand` 是当前最小的语义命令信封。已有 `EditCommand` 继续作为文本交互词汇，
通过 `ContentCommand::Edit` 发送；Terminal、Web 等后续 Content 可以解释其中适用的子集，
对其余命令返回 `NotHandled`，也可以增加自己的语义命令变体。

```text
Mode action
-> Option<ContentCommand>
-> target Content + ContentViewState
-> ContentResult::Handled(effect) | ContentResult::NotHandled
```

`Handled(ContentEffect::None)` 表示命令已消费但没有外部副作用；`Handled(Save(...))`
表示 App 仍需执行 effect。`NotHandled` 只表达不支持该命令，不表示 IO 或内部错误。
Content 与 ContentViewState 变体不匹配仍是内部不变量错误。

## 非目标

- 不实现 Mode stack 或基于 `NotHandled` 的多 Mode 冒泡链。
- 不增加 capability 元数据、Content 类型 allowlist 或“可用 Mode”查询。
- 不重命名全部 `EditCommand`，也不提前定义 Terminal/Web 专属命令。

## 验收

- `Mode::execute` 和 `ModeInstance::execute` 返回 `Option<ContentCommand>`。
- Buffer 明确处理文本编辑命令；StatusBar 对同一命令返回 `NotHandled`。
- 保存 effect 和现有 Vim 行为保持不变。
- fmt、test、clippy 与 diff 检查通过。
