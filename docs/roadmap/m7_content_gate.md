# M7 Content 扩展门槛评估

**评估日期：** 2026-07-21

**结论：** 实施条件未触发，保持延期。

## 核对结果

- `Content` 和 `ContentKind` 仍只有 `Buffer`、`StatusBar` 两个变体。
- TypeScript 插件只注册 Mode、行为、状态、展示和后台分析，没有插件
  Content 用例。
- `ContentViewState` 仍通过闭合枚举表达配对关系。
- App 测试 `production_content_paths_use_closed_static_dispatch` 持续禁止
  `Box<dyn Content>`、动态 View state 和 App 对具体 Buffer 的探测。
- Content/View state 错配已有结构化错误与回归测试。

## 复审判断

当前平行分派规模很小，编译器穷尽检查仍比声明宏或动态 registry 更直接。
现在增加唯一声明宏不会减少真实维护成本，却会引入代码生成层；开放插件
Content 更需要新的身份、序列化、生命周期和安全设计，不能作为顺手扩展。

因此本阶段不修改代码。以下任一条件出现时重新开启 M7：

1. 合入第三种内建 Content；
2. 至少两个真实插件需要自定义 Content；
3. 新增 Content 已连续发生人工漏改，且现有测试不能可靠捕获。

到时先比较普通穷尽分派与单一声明源的维护成本。只有插件用例成立，才单独
设计动态 Content 协议。

## 验收判断

路线图要求“未满足触发条件时保持延期”。本次核对确认条件未满足，因此
M7 的决策门槛已完成，没有为理论开放性牺牲当前闭合模型的类型安全。
