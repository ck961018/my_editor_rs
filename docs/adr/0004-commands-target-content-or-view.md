# 命令分别面向 Content 生命周期与 View 交互

## 背景

现有公开表面同时提供 `newBuffer`、`switchBuffer` 和
`context.buffers.*`。这把 Buffer 的数据生命周期、当前焦点和完整交互单元
替换混成了一个概念。在一个 View 可以组合多个 Content 和子 View 后，
“切换 Buffer”无法说明应该重绑一个 Content，还是替换整个复合 View。

## 决策

公开命令按领域目标分为两类：

- `content.*` 管理数据生命周期；
- `view.*` 管理焦点和完整 View 的替换。

`view.switch` 从当前焦点寻找最近的 Switchable View，并用新的 View spec
原子替换它。改变某个命名 Content binding 不是通用切换，由所属 View 的
专有命令表达。

Buffer 是 ContentKind 和内部文本实现，可以继续出现在类型、adapter、
局部变量及文本 operation 中，但不能作为公开命令目标。

现有公开 Buffer 命令采用一次性迁移。内建插件、TypeScript 声明、示例和
测试与实现一起更新，不保留 `newBuffer`、`switchBuffer`、
`context.buffers` 或 `buffer.*` 兼容别名。

## 结果

- 打开数据和展示数据可以独立组合，也可以由上层 `open` 体验统一编排。
- `view.switch` 的参数必须描述 View，而不只是裸 ContentId。
- Content binding 变化与 View replacement 在 operation 中保持不同类型。
- 外部插件需要随 M1 更新；Vell 当前不承诺旧实验性脚本接口兼容。
- Buffer 文本编辑能力仍可由 Buffer Mode adapter 暴露，不属于本决策要
  移除的命令空间。
