# 前缀键与通用 Awaiting 输入实施记录

> 日期：2026-07-16
> 状态：已完成

对应设计：`docs/superpowers/specs/2026-07-16-prefix-key-and-awaiting-input-design.md`

## 已完成

- [x] 将 `Keymap` 改为 generic trie，允许同一节点同时拥有 action 与 children；
- [x] 实现按序列 bind/unbind、Leader 定义期展开和 which-key continuation query；
- [x] 新增 `InputStatus`、`InputDecision<A>`、`InputContext<A>` 和 LIFO `InputCoordinator<S>`；
- [x] 实现 mode/global trie 虚拟叠加、最近显式 timeout、最长完整绑定与 replay；
- [x] 删除 Buffer、StatusBar、ContentStore 的静态 content keymap 捕获链；
- [x] 将 ModeInstance 封装在 View 后面，Dispatcher 不再接收或识别 ModeInstance；
- [x] 在 App 主循环的 `tokio::select!` 中接入最近 input deadline，不创建 timer task；
- [x] mode/focus 生命周期变化时取消对应 Awaiting 并丢弃固定 pending；
- [x] 实现 `gg`、`f/F`、count 和最小 `dd` operator 验证切片；
- [x] 新增通用 `MoveToLine`、`MoveToChar`、`DeleteLines` 编辑命令；
- [x] 补充 core、dispatcher 和 App 端到端回归测试。

## 验证命令

```powershell
cargo fmt -- --check
cargo test
cargo clippy --all-targets --all-features
git diff --check
```
