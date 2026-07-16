# Content 事务与 Motion/Range 执行计划

> 日期：2026-07-16  
> 对应规格：`docs/superpowers/specs/2026-07-16-content-transaction-motion-range-design.md`

## 1. TextChangeSet

- 新建 `core::transaction`，定义 `Affinity`、`TextChange`、`TextChangeSet`、validation、apply、
  invert、compose 和 position mapping。
- 先补纯单元测试：insert/delete/replace、Unicode、组合、反转、边界 affinity、非法输入。

## 2. Buffer active transaction 与 history

- Buffer 增加 active transaction、线性 history cursor、`TextStateId` 和 saved state。
- 所有 Rope 修改经 transaction delta 入口执行；移除编辑原语中的直接 revision/modified 更新。
- 实现 begin/commit/rollback/undo/redo 和 implicit transaction。
- 补 Insert 聚合、空事务、redo 截断、savepoint、CRLF 和 revision 测试。

## 3. Content 生命周期与保存

- 扩展 Content command/result，加入 transaction lifecycle、undo/redo 和 change map。
- `Content` 静态分派到 Buffer 的关联事务实现；StatusBar 返回 `NotHandled`。
- 保存前 commit active transaction，SaveSnapshot 携带 state ID；完成事件按 state ID 更新 savepoint。
- 保持 ContentStore 作为唯一表，不向 app 暴露 Buffer。

## 4. 多 View 变换

- Content outcome 携带中立的 Content change；ContentStore 提供同变体 view-state transform 分派。
- App 用 change map 更新所有绑定 View，并在发起 View 应用 selection intent。
- 测试同 Content 双 View 的 edit/undo/redo、方向 selection 和 collapsed 守恒。

## 5. Motion/Range/operator

- 新建 `core::motion`，定义 `TextMotion`、`MotionOutcome`、分型 `TextRange` 和纯 resolver。
- 普通移动复用 destination；delete operator lowering covered ranges 为一个 TextChangeSet。
- 实现/迁移阶段二所需 line、word、line start/end motions 和 linewise current-line target。
- 测试 LF/CRLF、末行无换行、count、多 selection merge 和 motion failure no-op。

## 6. Vim 接线

- Mode execution outcome 支持有序 Content commands。
- Insert enter/exit 控制 Content active transaction；`o/O/s/C/S` 初始编辑纳入同一事务。
- operator pending 接受 `d`、`w`、`$`、`0` 和第二个 `d`，正确合并 operator/motion count。
- 绑定 Normal `u`、`Ctrl+R`。

## 7. 验证

- 运行 `cargo fmt`。
- 运行 `cargo test`。
- 运行 `cargo clippy --all-targets --all-features`。
- 运行 `git diff --check` 并核对 touched files 的 EOL。
