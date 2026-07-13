# 文本位置与显示位置分离实施计划

**日期：** 2026-07-13

- [x] 将 Selection 的 CursorPos 迁移为只含文档偏移的 TextOffset。
- [x] 增加 Buffer TextOffset -> TextPoint 派生查询。
- [x] 增加 ContentQuery::TextPoints，并迁移 View/TUI 查询链。
- [x] 在 TUI 中显式计算 DisplayPoint，迁移 viewport、cursor 与高亮。
- [x] 更新测试、架构文档与 roadmap，运行 fmt、test、clippy 与 diff 检查。
