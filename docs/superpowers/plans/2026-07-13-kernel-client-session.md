# Kernel 与 ClientSession 分层实施计划

**日期：** 2026-07-13

- [x] 提取 Kernel，共享 ContentStore、ModeRegistry 与后台保存服务。
- [x] 提取 ClientSession，归拢 Scene、focus、ViewStore、Dispatcher 与分配器。
- [x] 将 App 事件、命令、布局与渲染路径迁移到新所有权。
- [x] 测试共享 Kernel 下会话状态相互独立。
- [x] 更新架构文档与 roadmap，运行 fmt、test、clippy 与 diff 检查。
