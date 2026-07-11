# Scene Mutation 与 View 生命周期设计

日期：2026-07-12

## 背景

当前 `App` 持有 `Scene`、`SceneBuilder`、按 `SpaceId` 索引的 `View`，以及
`focused: SpaceId`。初始化时先由 `SceneBuilder` 生成 Scene，再一次性建立所有
View。运行期间只有 resize 会修改 Scene；测试若要改变布局，通常直接分别替换
`app.scene`、`app.views` 和 `app.focused`。

这种方式不能安全支撑后续 split、close、panel、overlay 或 Content 改绑。一次
布局变更会同时影响场景树、View、每个 View 的 `ContentRuntime` 和焦点，不能由
调用者分别维护。

本设计建立后端布局 mutation 与 App 生命周期协调，但不增加用户可见的 split
命令或 UI。

## 目标

- `Scene` 是当前布局节点的唯一事实来源。
- `SceneBuilder` 直接分析和修改 App 传入的 `&mut Scene`，自身不保存节点副本。
- 所有结构修改由 Builder 的语义操作完成，外部不能任意修改节点。
- App 在 Builder 修改成功后统一协调 View、ContentRuntime 和焦点。
- 同一 `SpaceId + ContentId` 绑定保留原 View；新绑定创建新 View。
- `SpaceId` 在 App 生命周期内单调分配且不复用。
- 失败操作不改变 Scene、ID 分配器、View 或焦点。

## 非目标

- 不增加 split/close 的按键绑定、命令面板入口或可见边框。
- 不实现 pane tab、View history 或切回 Content 时恢复旧位置。
- 不改变 ContentStore、RenderQuery 或 TUI 的所有权边界。
- 不引入 Scene clone、Scene draft、mutation 日志或持久化树。
- 不实现插件回调、异步布局 mutation 或跨线程场景编辑。

## 所有权

```text
App
  scene: Scene
  scene_builder: SceneBuilder
  views: HashMap<SpaceId, View>
  focused: SpaceId
```

`Scene` 保存当前 root、size 和节点表。Dispatcher 和 TUI 只读该 Scene。

`SceneBuilder` 只保存 `next_space_id`。每次布局请求把当前 Scene 的引用传给
Builder；Builder 读取现有父子关系，完整验证操作，然后直接修改 Scene。

```text
SceneBuilder
  next_space_id: u64
```

Builder 不持有 Scene、节点表、root 或 snapshot。现有长期节点副本和
`snapshot(root, size)` 模型被删除。初始化辅助函数可以在局部构造节点并返回第
一个有效 Scene，但局部节点不会留存在 Builder 中。

## Space 树结构

父子关系只保存一份。`SpaceNode` 持有结构，`SpaceKind` 只描述节点角色：

```rust
pub struct SpaceNode {
    pub id: SpaceId,
    pub parent: Option<SpaceId>,
    pub children: Vec<SpaceId>,
    pub space: Space,
}

pub enum SpaceKind {
    Container {
        arrangement: Arrangement,
    },
    Content {
        content: ContentId,
        focusable: bool,
    },
}
```

`focusable` 属于具体 Space 实例，不属于 Content 类型。同一种 Content 可以在一个
可交互 Space 中可聚焦，也可以在预览 Space 中不可聚焦。编辑区为可聚焦，状态栏
为不可聚焦。所有 Content Space 都有 View，focusable 只限制其是否能成为
`App.focused`。

Scene 必须始终满足：

- root 存在且 parent 为 `None`；
- 非 root 节点恰有一个 parent；
- parent 的 children 与 child 的 parent 一致；
- Content 节点没有 children；
- child ID 全部存在；
- 树中没有 cycle。

`Scene::node_mut` 不再作为外部修改入口。结构字段仅由 `scene` 模块内的 Builder
操作更新；App、Dispatcher 和 TUI 只读取 Scene。

## SceneBuilder API

Builder 提供有明确结构语义的操作，不提供任意 `set_children` 或公开节点修改：

```text
split
close
replace_content
set_sizing
```

每个操作返回 `Result<具体结果, SceneError>`，而不是 `bool`。结果携带 App 后续
处理需要的 SpaceId，例如：

```rust
pub struct SplitResult {
    pub new_space: SpaceId,
}

pub struct CloseResult {
    pub removed_space: SpaceId,
    pub surviving_neighbor: Option<SpaceId>,
}
```

Builder 只检查场景树结构，不查询 ContentStore，不创建 View，也不决定最终焦点。

所有操作必须先完成全部可失败检查，再进行第一次写入。验证成功后才分配 ID；从
第一次分配 ID 到操作返回之间不得再有普通错误分支。因此返回 `Err` 时 Scene 与
`next_space_id` 都保持不变。

## ID 分配

`SceneBuilder` 在 App 生命周期内持续存在，并用单调计数器分配 ID：

```rust
fn alloc(&mut self) -> SpaceId {
    let id = SpaceId(self.next_space_id);
    self.next_space_id += 1;
    id
}
```

删除 Space 不降低计数器；resize 和 View 协调不影响计数器；失败操作在分配前
返回。因此已经分配的 SpaceId 不复用，后续动态节点从初始化布局后的下一个 ID
继续分配。

## 结构操作语义

### Split

`split` 只接受 Content leaf 作为目标。

- 若父容器的方向与 split 方向一致，在目标前后插入新的 Content sibling；
- 若方向不同，创建新 Container 包住原目标与新 Content，并在原父节点中替换
  目标；
- 目标 Content 的 SpaceId 保留；
- 新 Content 总是获得新 SpaceId；
- 需要新 Container 时，Container 也获得新 SpaceId。

### Close

`close` 删除一个 Content leaf。

- 从父节点 children 中移除目标；
- 父 Container 只剩一个 child 时折叠该 Container；
- 折叠可沿祖先持续进行，并在需要时替换 root；
- Builder 返回可供 App 选择焦点的 surviving neighbor；
- 是否允许关闭最后一个可聚焦 Space 由 App 在调用 Builder 前判断。

### Replace Content

`replace_content` 保留 SpaceId，只替换 ContentId 和 focusable。该操作结束后，App
把它视为一次新的 Space-Content 绑定并重建对应 View。

### Set Sizing

`set_sizing` 只修改目标 Space 的 sizing，不触发 View 重建。

## App 生命周期协调

App 是 Scene、View 和焦点一致性的唯一协调者。高层 App 操作按以下顺序执行：

```text
验证 ContentId 和 App 级生命周期条件
  -> 调用 SceneBuilder 修改 Scene
  -> 协调 views
  -> 解析 focused
  -> 请求重绘
```

Builder 成功后，App 遍历当前 Scene 的 Content Space，并按 `SpaceId` 与旧 View
比较：

- `SpaceId` 与 `ContentId` 都不变：保留整个 View，包括 selections 与 runtime；
- 新 `SpaceId`：由 ContentStore 创建匹配 runtime，并创建新 View；
- `SpaceId` 消失：删除旧 View；
- `SpaceId` 保留但 `ContentId` 改变：销毁旧 View，创建新 View，selection 回到
  新 Content 原点，runtime 重新创建。

切回先前 Content 时不会恢复旧 View。未来若需要恢复位置，应新增独立 View
history，而不是让 View 跨绑定存活。

App 在调用 Builder 前确认新建或改绑所引用的 ContentId 存在，并阻止产生没有
任何可聚焦 Content Space 的布局。这样 Builder 成功后的 View 协调是同步且不可
失败的，不需要 Scene clone 或回滚。

## 焦点规则

结构操作不直接写 `App.focused`。高层 App 操作可以根据 Builder 返回结果指定
preferred focus；App 按以下顺序解析：

1. 明确指定的 preferred Space 仍存在且可聚焦时使用它；
2. 否则旧 focused Space 仍存在且可聚焦时保留旧焦点；
3. 否则使用新 Scene DFS 顺序中的第一个可聚焦 Content Space；
4. 没有可聚焦 Space 时拒绝该高层操作，不调用 Builder。

例如 split 可以选择聚焦新 Space 或保留旧 Space；close 使用 Builder 返回的
surviving neighbor；只改变 sizing 时保持原焦点。

## 错误边界

Builder 使用结构错误类型表达目标不存在、目标不是 Content leaf、父子关系不合法
等问题。App 使用自身错误表达 Content 不存在、关闭最后一个可聚焦 Space或指定
焦点不可用。

普通错误必须满足：

```text
Scene 不变
SceneBuilder.next_space_id 不变
views 不变
focused 不变
```

内存分配失败和内部不变量 panic 不作为可恢复 transaction 处理。本设计依赖 App
事件处理期间的同步执行；如果未来 mutation 包含异步步骤、插件回调或其他中途
可失败操作，再单独设计 transaction。

## 实现范围

本轮实现 Builder 的后端 `split`、`close`、`replace_content` 和 `set_sizing`，
以及 App 的 View/focus 协调入口。初始化使用同一 ID 分配器和 Scene 构造边界。

本轮不接入按键、命令面板或其他用户入口，不绘制 pane 边框，也不实现方向焦点
导航。现有用户行为保持不变。

## 测试

SceneBuilder 测试覆盖：

- 同方向 split 插入 sibling；
- 不同方向 split 创建 Container；
- close 后折叠单 child Container 并正确替换 root；
- replace_content 保留 SpaceId；
- 失败操作不修改 Scene，也不消耗 ID；
- 删除 Space 后 ID 继续递增且不复用；
- 每次成功操作后树不变量成立。

App 测试覆盖：

- 相同 `SpaceId + ContentId` 保留 View 状态；
- 新 Space 创建独立 ContentRuntime；
- replace_content 重建 View、selection 和 runtime；
- 删除 Space 同时删除 View；
- 关闭 focused Space 后采用 surviving neighbor；
- 不可聚焦 Content Space 不会成为 fallback focus；
- 不存在的 ContentId 在调用 Builder 前被拒绝，所有状态保持不变。

TUI 只需适配新的 Scene 构造 API并保持现有布局和渲染测试通过；本轮不增加 split
UI 测试。

## 验收标准

- Scene 是唯一节点表，Builder 不保存 Scene 数据。
- Scene 的结构只能经 Builder 的语义操作修改。
- 动态操作后 Scene、View、ContentRuntime 与焦点保持一致。
- 所有失败路径保持 Scene、ID、View 和焦点不变。
- 已提交 SpaceId 单调且不复用。
- 没有新增用户可见行为，现有测试、格式检查和 Clippy 通过。
