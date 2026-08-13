# BufferView 原生 gutter 与混合行号 roadmap

状态：已实现。

## 1. 目标

为每个 `core.buffer` BufferView 提供宿主原生 gutter：

- 默认显示；
- 当前逻辑行显示一基绝对行号；
- 其他可见逻辑行显示相对当前行的距离；
- 用户可以配置是否显示和 gutter 宽度；
- DiffView 的左右子 BufferView 各自拥有 gutter；
- gutter 不由插件、Mode 或 View extension 创建。

这个功能属于 BufferView 的默认呈现结构。插件不需要注册 definition、
extension 或 Mode，也不能通过 render callback 接管它。

## 2. 用户行为

假设 cursor 位于第 10 行：

```text
   3 │ text on line 7
   2 │ text on line 8
   1 │ text on line 9
  10 │ text on line 10
   1 │ text on line 11
   2 │ text on line 12
```

示例中的 `│` 只标记 gutter 与正文的边界，不是默认占用的分隔 cell。

行号以逻辑行为单位，不受 tab、宽字符或水平滚动影响。当前行由该
BufferView primary selection 的 `head` 决定；同一 Content 出现在两个
BufferView 时，两边可以显示不同的当前行和相对行号。

初始配置为：

```ts
{
  visible: true,
  width: 4,
}
```

`width` 表示期望占用的终端 cell 数。实际宽度至少为 2 时，最右侧 cell
固定留空作为正文前的视觉间距，行号在其余 cell 内右对齐。标签超过行号
区域宽度时，最左 cell 显示 `>`，其余 cell 显示标签低位；宽度为 1 时无法
保留间距，直接显示一位数字或 `>`。窄终端中以布局返回的实际宽度为准，
不得覆盖正文 cell 或导致越界写入。

## 3. 已确认的设计

### 3.1 所有权

`ViewWorkspace` 继续作为 View 语义树、Pane 映射和 Scene 结构的深模块。
它负责 BufferView gutter 的创建、隐藏、宽度、替换和清理。

gutter 使用保留 PaneKey：

```text
builtin.gutter
```

BufferView 的默认结构为：

```text
BufferView
└── horizontal layout root
    ├── builtin.gutter  inert, Fixed(width)
    └── body            focusable, Grow(1)
```

per-pane 状态栏位于这个横向结构之外：

```text
BufferView pane root
├── horizontal layout root
│   ├── builtin.gutter
│   └── body
└── builtin.status
```

全局状态栏仍属于 workspace 根布局，不改变 gutter 的 per-BufferView
所有权。

不得新增以下对象：

- `GutterView`；
- `GutterContent`；
- `GutterMode`；
- `GutterState`；
- 插件侧 Pane 状态或第二棵布局树。

gutter Pane 没有独立生命周期、焦点、viewport 或 Mode attachment。它与
body 引用同一 ViewId，并随 BufferView 一起创建和销毁。

### 3.2 为什么不是 View extension

View extension 适合由插件按 owner 安装和卸载的可选派生区域。默认行号是
`core.buffer` 的结构不变量：

- 没有插件时也必须存在；
- 插件卸载不能移除它；
- BufferView 的所有创建路径必须得到相同结构；
- split、switch、close 和 Diff child 生命周期必须原子维护它。

因此不得用内建 TypeScript 插件伪装实现，也不得给 `viewPolicy` 增加
`gutter` render callback。用户配置只改变宿主原生选项，不改变所有权。

### 3.3 配置范围

第一版只提供 session 级 BufferView 默认配置，不提供 per-View、per-Mode
或 per-language override。配置在创建 `App` 前确定，后续创建的所有
BufferView 使用同一份值。

TypeScript 配置入口建议为：

```ts
editor.configure({
  bufferView: {
    gutter: {
      visible: false,
      width: 6,
    },
  },
});
```

规则：

- `visible` 默认为 `true`；
- `width` 默认为 `4`，只接受 `1..=16` 的整数；
- 配置对象可以省略任一字段；
- 多次顶层调用按字段覆盖，后出现的值优先；
- module 失败时与 Theme、Face、Mode 和 View definition 一起回滚；
- Mode callback、View extension callback 和 Worker 不能修改该配置。

配置 DTO 放在 `vell-protocol`，供 `vell-plugin-v8`、根二进制和
`vell-app` 共享。`App` constructor 接收一个 `EditorOptions`，不要增加
`gutter_visible`、`gutter_width` 等多个位置参数。

### 3.4 呈现接缝

行号是 derived presentation，不是 Content 数据。`vell-protocol` 增加
专用 owned DTO：

```rust
pub struct LineNumberPresentation {
    pub base_face: PaintFace,
    pub current_face: FacePatch,
    pub current_row: usize,
    pub line_count: usize,
}

pub enum ViewPresentation {
    Text(TextPresentation),
    LineNumbers(LineNumberPresentation),
    StatusBar(StatusBarPresentation),
    Lines(LinesPresentation),
}
```

`AppQuery::view` 根据来源 PaneKey 选择 presentation：

```text
body             -> Text
builtin.gutter   -> LineNumbers
builtin.status   -> StatusBar
extension Pane   -> Lines
```

`AppQuery` 负责：

- 验证目标是带 `document` binding 的 BufferView；
- 读取 primary selection `head` 对应的逻辑行；
- 读取 Content 的 `line_count`；
- 解析 gutter Face；
- 返回 owned `LineNumberPresentation`。

render query 不调用 Mode、V8、Worker 或插件 callback。

### 3.5 viewport 与绘制

`SceneRenderer` 继续按 ViewId 持有唯一 viewport。body 是唯一 focusable
Pane，因此 cursor follow 仍由 body 计算。绘制 `LineNumbers` 时读取同一
ViewId 的 viewport top，并为实际 Pane 高度生成标签。

绘制规则：

1. `row == current_row` 时显示 `row + 1`；
2. 其他行显示 `abs_diff(row, current_row)`；
3. `row >= line_count` 时显示空白；
4. 实际 Pane 宽度至少为 2 时，末尾保留一个空白 cell，标签在其余区域
   右对齐；
5. 溢出按第 2 节规则显示；
6. 每个 cell 都写入 gutter Face，当前行再叠加 current Face；
7. 水平 viewport、tab width 和文本 decoration 不影响 gutter。

TUI 只负责根据 presentation、viewport 和最终 Rect 绘制，不查询 View、
Mode state 或插件运行时。

### 3.6 Face

增加两个宿主 Face：

- `ui.gutter`：整个 gutter 的根 Face；
- `ui.gutter.current`：当前绝对行号的相对 Face。

`terminal-default` 为普通行号使用暗 ANSI 灰色，当前行号使用较亮前景并
加粗；Catppuccin 基础 Theme 为普通行号使用 `surface2`，当前行使用主文本色并
加粗。Theme 缺少 `ui.gutter.current` 时按 Face 继承规则退回
`ui.gutter`。

Ropey 会为以换行符结尾的文本暴露一个尾随空行。Character cursor 不得进入
这个内部行，gutter 也不为它编号；InsertionPoint 当前确实位于该行时，仍将
它作为正在编辑的新行显示。这一限制属于 Buffer cursor domain 与 derived
presentation 的协作，不应在 TUI 中伪造或隐藏 cursor。

## 4. 结构不变量

实现后必须满足：

- 每个 `core.buffer` View 恰有一个 `body` 和一个 `builtin.gutter` Pane；
- 其他 View definition 不会自动获得 gutter；
- gutter Space 永远不可聚焦；
- gutter 和 body 的 Scene leaf 引用同一 ViewId；
- 隐藏时保留 Pane identity，仅把 sizing 设为 `Fixed(0)`；
- rebind `document` 保留 gutter Space、ViewId 和 viewport；
- `view.switch` 结束旧 View 时清理旧 gutter；
- BufferView 与复合 View 相互替换时不留下孤儿 Pane 或 wrapper；
- DiffView 父 View 没有 gutter，两个子 BufferView 各有一个；
- split 和 close 操作把完整 BufferView pane root 当作一个布局槽位；
- workspace draft 校验失败时不发布 Scene、PaneKey 或结构 ID 变化。

最后两项要求不能通过在每个调用点手工 wrap/unwrap 来实现。
`ViewWorkspace` 应提供一个私有的 BufferView pane recipe helper，隐藏 body、
gutter、可选 status 和布局 root 的组合细节。调用方只提交完整 View
生命周期操作。

## 5. 模块职责

### `vell-protocol`

- 定义 `EditorOptions`、BufferView gutter 配置和默认值；
- 定义宽度上下限；
- 定义 `LineNumberPresentation`；
- 保持 owned、无 IO、无 app/TUI/V8 依赖。

### `vell-app::ViewWorkspace`

- 保留 `builtin.gutter` PaneKey；
- 将 BufferView pane recipe 封装在 workspace 内部；
- 维护完整 pane root 的 split、replace、close 与验证；
- 根据 options 设置 gutter 的 `Fixed(width)` 或 `Fixed(0)`；
- 不计算文本行号，也不保存 viewport。

### `vell-app::AppQuery`

- 将 BufferView state 与 Content metrics 组装成行号 presentation；
- 解析 `ui.gutter` 和 `ui.gutter.current`；
- 对错误的 View/Content 配对返回 `RenderQueryError`。

### `vell-tui::SceneRenderer`

- 复用 ViewId viewport；
- 按最终 Rect 绘制绝对/相对标签；
- 保证裁剪、清屏和宽度计算使用整数 cell。

### `vell-plugin-v8`

- 解析顶层 `editor.configure`；
- 把配置写入可回滚的 `ScriptConfigurationDraft`；
- 返回 owned `EditorOptions`，不返回 V8 handle；
- 不新增 gutter callback、Mode primitive 或 View extension owner。

### 根二进制

- 从 `LoadedEditorConfiguration` 取得 `EditorOptions`；
- 在 composition root 注入 `App`；
- 不参与 gutter 布局或渲染计算。

## 6. 实现阶段

### M0：冻结契约与测试基线

目标：先把用户可观察行为和结构不变量写成测试。

工作：

- 为绝对/相对标签、第一行、最后一行和溢出建立纯绘制测试；
- 为初始 BufferView、split、Diff child 和 switch 建立 pane recipe 测试；
- 为默认配置和非法宽度建立 TypeScript 契约测试；
- 记录当前无 gutter 时的 Scene 与渲染基线，便于定位布局回归。

验收：测试准确区分 View、Pane、Content 和 viewport owner，不断言私有
wrapper ID 或内部 helper 调用次数。

### M1：建立原生 BufferView pane recipe

目标：让 `ViewWorkspace` 原子拥有 gutter 结构。

工作：

- 新增保留的 `GUTTER_PANE`；
- 在 `SceneBuilder` 中增加一次构造完整 BufferView pane root 的操作；
- 让 workspace 私有 helper 统一创建、定位和验证完整 pane root；
- 改造 initial editor、split、replace、compound child 和 close 路径；
- 让 extension key 校验同时拒绝 body、status 和 gutter 保留名；
- 隐藏 gutter 时保留零宽 Space。

验收：所有 BufferView 创建路径得到相同结构；BufferView 与 DiffView
双向切换、关闭任一 Diff child、per-pane status 和 extension 共存时均无
孤儿 Space、重复 PaneKey 或焦点漂移。

### M2：打通行号 presentation 与 TUI

目标：默认 gutter 可以正确显示并跟随滚动。

工作：

- 增加 `LineNumberPresentation` 与所有穷尽匹配；
- 在 `AppQuery` 中实现 `builtin.gutter` 查询；
- 在 renderer 中实现共享 viewport 的行号绘制；
- 增加 gutter Face 与 bundled Theme 默认值；
- 确保 decoration 查询仍只服务 body Pane。

验收：移动 cursor、滚动 viewport、编辑跨越换行、undo/redo、共享 Content
双 View 和 Diff 左右 View 都显示正确；渲染路径不进入 Mode 或 V8。

### M3：开放原生配置

目标：用户可以在 `config.ts` 修改默认可见性和宽度。

工作：

- 在 `vell-protocol` 定义 `EditorOptions` 与默认值；
- 在 `runtime/editor.d.ts` 增加 `editor.configure`；
- 在 V8 schema 中严格解析 partial configuration；
- 将配置纳入 module draft 的原子提交和回滚；
- 经 `LoadedEditorConfiguration` 和根二进制注入 `App`；
- 默认 constructor 和测试 helper 使用 `EditorOptions::default()`。

验收：默认显示 4 cell gutter；`visible: false` 创建零宽 gutter；合法宽度
准确进入 Scene sizing；非法类型、范围和 callback 期修改均被拒绝且不留下
部分配置。

### M4：回归、文档与清理

目标：关闭跨层遗漏并让用户只需理解一个配置入口。

工作：

- 更新 `docs/design/editor-kernel-architecture.md` 与 `CONTEXT.md`；
- 更新 `docs/scripting.md`，明确 gutter 是 BufferView 默认功能；
- 删除把默认 gutter 推荐为 View extension 的示例措辞；
- 增加 remote/render query、主题回退和极窄终端测试；
- 检查所有 `ViewPresentation`、PaneKey 和 workspace 验证的穷尽分派。

验收：文档不要求插件作者定义 gutter；公共 TypeScript contract 与 Rust
schema 一致；完整门禁通过。

## 7. 测试矩阵

### `vell-protocol`

- options 默认值与宽度范围；
- `LineNumberPresentation` owned contract；
- `ViewPresentation` 穷尽分派。

### `vell-app`

- 初始 BufferView 默认拥有 gutter；
- visible/hidden 与指定宽度；
- split、switch、close、rebind、Diff child；
- global/per-pane status 与 View extension 共存；
- 同一 Content 的两个 View 使用各自 primary selection；
- query 缺失 View、Content 或错误 Pane 映射时返回结构化错误。

### `vell-tui`

- 当前行绝对、其他行相对；
- viewport top 不为零；
- 文件首尾与 EOF 后空白；
- 宽度 1、默认宽度、溢出和窄 Rect；
- gutter 不随水平滚动；
- Face、裁剪、清行与终端 cell 对齐。

### `vell-plugin-v8` 与 `runtime`

- `editor.configure` 类型声明；
- partial merge、字段覆盖与 module rollback；
- 非整数、零、负数、超上限和未知字段；
- callback/Worker 中修改配置被拒绝；
- `pnpm typecheck` 覆盖默认配置示例。

## 8. 非目标

第一版不包含：

- Git signs、diagnostics、breakpoints 或 fold markers；
- 多列 gutter 或插件向原生 gutter 注入 segment；
- 鼠标点击、选择或 gutter 焦点；
- per-View、per-language 或 Mode 控制的行号设置；
- soft wrap 后的视觉行编号；
- 自动根据文件总行数扩展配置宽度；
- 运行期间动态增删 gutter Pane。

未来如需 signs，应先设计原生 gutter presentation 的组合模型，不应让多个
插件各自创建竞争正文左侧的 Pane。本 roadmap 不提前为尚未出现的第二种
原生 gutter 内容建立抽象 seam。

## 9. 完成标准

- BufferView 默认显示混合绝对/相对行号；
- 用户可以通过一个 typed 配置入口控制可见性和宽度；
- DiffView 左右子 BufferView 独立工作；
- gutter 生命周期完全包含在 `ViewWorkspace` draft 中；
- renderer 只读取 owned presentation 和共享 viewport；
- 插件 contract 不包含 gutter render callback；
- 不存在 `GutterView`、`GutterContent`、`GutterMode` 或 Pane-owned state；
- `cargo fmt --all -- --check` 通过；
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` 通过；
- `cargo test --workspace --all-features` 通过；
- `cargo doc --workspace --all-features --no-deps` 通过；
- `pnpm typecheck` 通过；
- Markdown 行长与相对链接检查通过。
