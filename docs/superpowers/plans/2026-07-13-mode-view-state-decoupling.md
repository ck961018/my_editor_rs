# Mode 与 ContentViewState 解耦实施计划

**日期：** 2026-07-13

- [x] 用 `ModeRegistry`/`ModeInstance` 替代 Buffer 持有的 `ModeSet`。
- [x] 用静态 `ContentViewState` 替代 `ContentRuntime` 和 View 的无条件 selections。
- [x] 将 dispatcher、App 执行和 ViewData 组装迁移到新的所有权模型。
- [x] 更新测试、规格和 roadmap，运行 fmt、test、clippy 与 diff 检查。
