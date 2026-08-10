# Mode attachment 属于 View

## 背景

一个 Content 可以同时被多个 View 使用，而一个复合 View 也可以引用多个
Content。如果 Mode 直接附加在 Content 上，同一 Content 的所有呈现会被迫
共享快捷键、光标语义和启用状态；如果父 View 汇总全部 Content 的 Mode，
DiffView 和 SearchResultsView 又会获得与自身交互无关的语言行为。

## 决策

Mode definition 全局注册，但每个运行时 attachment 属于具体 View。
一个 View 可以拥有多个有序 attachment。

Mode 可以声明自己匹配的 View definition、命名 Content binding 和 Content
分类。统一的 ModeResolver 根据这些输入产生 attachment plan。它不会扫描
整个 View 子树，也不会合并所有间接引用 Content 的 Mode。

与 Content 分析相关的 state 可以按 `(ModeId, ContentId)` 共享。光标、
输入和呈现相关的 view state 按 `(ModeId, ViewId)` 隔离。

BufferView 默认通过 `document` binding 解析语言 Mode。复合 View 的独立
编辑区域优先表示为子 BufferView，由子 View 分别解析自己的 Mode。

## 结果

- 同一 Content 的不同 View 可以拥有不同的 Mode 组合和交互状态。
- 可复用分析不必因 View 隔离而重复计算。
- 插件不再各自实现文件后缀到 Mode 的全局分派。
- View definition 必须自行维护结构不变量，不能依赖可选 Mode 才能成立。
- ModeResolver 的具体 schema 暂缓到 M4，并由真实复合 View 验证。
