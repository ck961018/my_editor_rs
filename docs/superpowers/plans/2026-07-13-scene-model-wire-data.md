# Scene 模型与协议数据分离实施计划

**日期：** 2026-07-13

- [x] 将 SceneBuilder、模型错误和 mutation result 迁入 app::scene_model。
- [x] 将 protocol::scene 收敛为 Scene/SpaceNode 快照数据与只读访问。
- [x] 将 ClientSession 与 app 测试迁移到后端 Scene 模型。
- [x] 用 TUI 本地纯数据 fixture 替代对 SceneBuilder 的依赖。
- [x] 更新架构文档、roadmap 与 AGENTS 边界并运行完整验证。
