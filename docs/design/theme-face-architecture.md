# Theme 与可扩展 Face 架构

**状态：** 已确认方向，待分阶段实现

**更新日期：** 2026-07-22

## 1. 文档定位

本文定义 Vell 的 Theme、named Face、用户覆盖和局部 Face remap 架构。

第一阶段需要提供 terminal-default 与 Catppuccin Latte、Frappé、
Macchiato、Mocha。整体模型同时为后续类似 Emacs 的能力保留稳定边界：

- 扩展定义自己的 named Face；
- Theme 覆盖 Face 的视觉属性；
- 用户覆盖当前 Theme 的个别属性；
- Content 或 View 对 Face 做局部 remap；
- 多个互不知情的扩展安全地叠加相对 remap；
- 不同显示能力选择不同 Face spec；
- Theme 切换不重新运行 Mode、V8 或后台分析。

本文以当前的 Kernel、ClientSession、Mode presentation 和 pull render
边界为前提。除非本文明确修改，现有所有权与执行约束继续成立。

## 2. 结论

Theme 是 ClientSession 级视觉配置，不是 Mode、Content、View 或 Frontend
行为。Mode 和高亮器产生语义 FaceName，Theme 决定这些名字的视觉含义，
AppQuery 在受控的只读查询阶段解析最终样式，Frontend 只绘制具体属性。

标准 Face namespace 不由 Mode 拥有：

- `ui.*`、`syntax.*`、`diagnostic.*` 和 `diff.*` 是共享语义词汇；
- Mode 可以引用这些名字，但不能宣称自己是其颜色 provider；
- 扩展私有 Face 使用 `plugin.<plugin-name>.*`；
- 私有 Face 可以声明 fallback 与继承关系；
- Theme 和用户可以按名字覆盖标准或私有 Face。

活动 Theme、用户覆盖和局部 remap 都属于 ClientSession。Theme catalog
可以在多个 ClientSession 之间共享，Kernel 不持有视觉配置。

## 3. 设计目标

### 3.1 语义与视觉分离

Tree-sitter、LSP、Mode 和其他 presentation provider 只产生：

```text
syntax.comment.documentation
syntax.function.macro
diagnostic.error
plugin.git-conflict.ours
```

它们不选择 Catppuccin、RGB 值或终端 ANSI 索引。

### 3.2 稳定的覆盖模型

默认定义、Theme、用户覆盖和局部 remap 必须是彼此独立的层。增加或删除
任何一层都不能破坏其他层，也不能产生虚假的 provider conflict。

### 3.3 属性级组合

覆盖必须按属性合并。例如 Theme 只改变 comment 前景色时，默认的 italic
仍然保留；selection 只设置背景色时，syntax 前景色仍然可见。

### 3.4 局部性

未来的 Face remap 可以作用于整个 Session、某个 Content 或某个 View。
局部变化不能修改共享 Content，也不能影响其他客户端。

### 3.5 可撤销的扩展贡献

运行时扩展增加的相对 remap 返回稳定 token。扩展卸载、Mode detach、View
关闭或显式 remove 时，只移除自己的贡献，不能覆盖其他贡献。

### 3.6 渲染纯度

render path 不执行 Mode、TypeScript、配置脚本或 Theme loader。所有 Theme
文件在受控阶段解析为 owned Rust 数据，AppQuery 只执行无副作用 lookup。

## 4. 非目标

第一阶段不实现：

- Theme 改变编辑行为或 keymap；
- 每个文本 span 执行 CSS selector；
- Theme 进入文本 undo/redo；
- Theme 文件执行任意 TypeScript 或 Rust 代码；
- 多个活动 Theme 的公开用户接口；
- Emacs 的全部 frame/font/display 语义；
- portable hardware cursor 颜色控制；
- 未经 profiling 的 FaceName interning 或 FaceId 优化。

## 5. 参考模型

本设计吸收下列已经验证的机制，但不复制其历史兼容负担：

- Zed：独立 ThemeRegistry、强类型 UI color、syntax 层级 fallback 和
  属性级 override；
- Helix：数据化 Theme、palette、Theme inheritance 和点分 scope fallback；
- Neovim：named highlight group、默认 link、Normal 基础样式和
  window-local override；
- Emacs：默认 Face、Theme spec、用户 customization、显式 inheritance 和
  buffer/window-local relative remap。

Vell 采用单个活动 Theme 的简单用户模型，但内部保留有序 layer 与 remap
结构，避免以后增加用户 customization 时更换核心数据模型。

## 6. 术语

### 6.1 FaceName

稳定的语义样式名字，例如 `syntax.comment`。它不是颜色，也不表示所有权。

### 6.2 FaceDefinition

Face 的语义定义，包括显式父 Face 和无 Theme 时的 fallback patch。

### 6.3 FacePatch

一组可能只指定部分属性的视觉修改。它可以覆盖另一个 patch 或具体样式。

### 6.4 PaintFace

发送给 Canvas 的具体样式。所有布尔属性已经确定，颜色的 `None` 表示使用
终端或前端默认颜色。

### 6.5 ThemeDefinition

从 TOML 得到的未解析 Theme，包括 palette 引用、Theme inheritance 和
Face patch。

### 6.6 ResolvedTheme

完成 Theme inheritance、palette 解析和 schema 校验后的不可变 Theme。

### 6.7 Face override

用户或 Session 对 named Face 的持久属性覆盖。它不改变 FaceName。

### 6.8 Face remap

在特定 Session、Content 或 View 中，将一个 named Face 替换或相对修改。
这是未来类 Emacs 局部 Face customization 的基础。

## 7. Crate 与所有权

### 7.1 `vell-protocol`

保存无 IO 的共享样式数据：

- `FaceName`；
- `Color`；
- `FaceValue<T>`；
- `FacePatch`；
- `PaintFace`；
- `FaceDefinition`；
- render query 中的 resolved presentation DTO。

protocol 不加载 Theme 文件，不访问配置目录，也不持有全局 registry。

### 7.2 `vell-theme`

新增只依赖 `vell-protocol` 的 crate，负责：

- Theme TOML schema；
- palette 解析；
- Theme inheritance 与循环检测；
- `ThemeRegistry`；
- `ResolvedTheme`；
- 点分层级 lookup；
- 内建 Theme assets。

它不依赖 app、core、mode、V8、TUI 或异步运行时。

### 7.3 `vell-mode`

Mode contract 只允许贡献扩展私有 `FaceDefinition`。Mode presentation 和
decoration 继续只引用 `FaceName`。

`FaceCatalog` 以及附带 `ModeName` 的 provider registration 保留在这一层。
`FaceDefinition` 本身只是 protocol 中的纯数据，不能引用 `ModeName`，从而
避免 `vell-protocol` 反向依赖 `vell-mode`。

标准 Face 的默认颜色从 Mode 中移除。Tree-sitter Mode 不再注册
`syntax.*` 的 ANSI 颜色。

### 7.4 `vell-app`

ClientSession 持有 `FaceEnvironment`：

- 当前 `Arc<ResolvedTheme>`；
- fallback Theme；
- 用户和 Session override；
- Content/View remap；
- Face revision 与解析 cache。

AppQuery 根据 View、Content 和 FaceName 查询 `FaceEnvironment`。

### 7.5 `vell-plugin-v8`

负责把 TypeScript 的 Face definition、override 或 remap 请求转换为纯数据
或 typed operation。它不解析 Theme 文件，也不向外泄漏 V8 handle。

### 7.6 `vell-tui`

只接收 `PaintFace` 或已解析的 `FacePatch`，完成 cell 合成和终端输出。
它不依赖 `vell-theme`，也不认识 ThemeName 或 palette。

依赖方向增加：

```text
vell-theme -> vell-protocol
vell-app   -> vell-theme + existing dependencies
vell binary -> vell-theme + vell-app + vell-plugin-v8 + vell-tui
```

## 8. 核心属性模型

当前 `Option<T>` 可以表示“未指定”和显式 `false`，但不能表示“恢复基础
Face 的颜色”。为兼容未来 customization，第一阶段即采用三态属性：

```rust
pub enum FaceValue<T> {
    Unspecified,
    Value(T),
    Reset,
}

pub struct FacePatch {
    pub foreground: FaceValue<Color>,
    pub background: FaceValue<Color>,
    pub bold: FaceValue<bool>,
    pub italic: FaceValue<bool>,
    pub underline: FaceValue<bool>,
}

pub struct PaintFace {
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}
```

语义如下：

- `Unspecified`：不修改较低层或 underlying Face；
- `Value(value)`：显式设置属性；
- `Reset`：恢复当前 presentation root Face 的该属性；
- `PaintFace` 是最终发送给 Canvas 的完整属性集合。

`Reset` 的基准不是当前较低优先级 decoration，而是当前 presentation 的
root Face：

- Buffer 使用 `ui.editor`；
- StatusBar 使用 `ui.status-bar`；
- 未来 popup 使用 `ui.popup`。

解析 root Face 自身的 `Reset` 时，颜色恢复 frontend 默认值，布尔属性恢复
`false`。

这个规则对应 Emacs face attribute 的 `unspecified` 与 `reset` 区别，同时
保持终端 renderer 的确定性。

## 9. FaceDefinition 与 namespace

```rust
pub struct FaceDefinition {
    pub name: FaceName,
    pub inherits: Vec<FaceName>,
    pub fallback: FacePatch,
}

// vell-mode 内部的注册记录，不属于 vell-protocol。
pub struct RegisteredFaceDefinition {
    pub definition: FaceDefinition,
    pub provider: FaceDefinitionProvider,
}

pub enum FaceDefinitionProvider {
    Host,
    Mode(ModeName),
}
```

这个拆分很重要：定义描述 Face 的语义，provider metadata 只负责注册冲突、
诊断与卸载清理，两者不能互相污染。

标准 namespace：

| Namespace | 用途 | 定义者 |
|---|---|---|
| `ui.*` | 编辑器 UI | Host |
| `syntax.*` | 语法与 markup | Host |
| `diagnostic.*` | 错误、警告等 | Host |
| `diff.*` | diff 与 merge | Host |
| `plugin.<name>.*` | 插件私有语义 | 对应插件 |

Theme 可以为尚未安装的插件提供 `plugin.*` 样式。该条目保留在 Theme 中，
当插件以后引用相同 FaceName 时自动生效。

不同插件重复定义同一个私有 Face 时：

- 完全相同的 definition 视为幂等注册；
- 不同 definition 返回结构化 conflict；
- Theme 或用户 override 从不算 definition provider conflict；
- Mode 不能注册 `ui.*`、`syntax.*` 等 host namespace。

## 10. Theme 数据模型

```rust
pub struct ThemeDefinition {
    pub name: ThemeName,
    pub appearance: Appearance,
    pub inherits: Option<ThemeName>,
    pub selectable: bool,
    pub palette: HashMap<String, ColorDefinition>,
    pub faces: HashMap<FaceName, FacePatchDefinition>,
}

pub struct ResolvedTheme {
    pub name: ThemeName,
    pub appearance: Appearance,
    pub faces: HashMap<FaceName, FacePatch>,
}

pub enum Appearance {
    Light,
    Dark,
}
```

`ThemeDefinition` 保留 palette 名称引用。Loader 必须先递归合并原始
definition，再解析 palette。这样 Catppuccin base fragment 可以引用
`mauve`，具体 flavor 在子 Theme 中提供实际色值。

Theme inheritance 第一阶段只允许单父节点，避免多继承 diamond 规则。Loader
必须检测：

- 不存在的父 Theme；
- inheritance cycle；
- 无效 palette 引用；
- 重复 ThemeName；
- 无效颜色与属性；
- selectable Theme 缺少必要的 `ui.editor`。

解析完成的 `ResolvedTheme` 不保留字符串 palette lookup，render path 只读取
具体 `FacePatch`。

## 11. 层级 fallback 与显式 inheritance

点分 fallback 与 Face inheritance 是两个不同概念。

### 11.1 点分 fallback

当一个 layer 中不存在精确名字时，从右向左删除点分 suffix：

```text
syntax.function.macro.rust
syntax.function.macro
syntax.function
syntax
```

返回最近的一个已定义条目，不把每一级隐式合并。

这样高亮器可以保留完整 capture 名，而旧 Theme 只定义
`syntax.function` 也能继续工作。

### 11.2 显式 inheritance

当 exact FaceDefinition 存在时，只有 `inherits` 声明才继承其他 named Face。
这支持点分关系之外的语义复用：

```text
plugin.todo.warning inherits diagnostic.warning
ui.status-bar.inactive inherits ui.status-bar
```

若有多个父 Face，列表前面的父 Face 优先级更高。实现按父列表逆序应用，
然后应用当前 Face 的 fallback 与各视觉 layer。

Face inheritance cycle 是 definition 错误。运行时不得递归到栈溢出；失败的
插件 definition 原子回滚，host definition 错误阻止启动。

## 12. 全局 named Face 解析

ClientSession 的 `FaceEnvironment` 至少包含：

```rust
pub struct FaceEnvironment {
    catalog: Arc<FaceCatalog>,
    fallback_theme: Arc<ResolvedTheme>,
    active_theme: Arc<ResolvedTheme>,
    global_overrides: FaceOverrideSet,
    theme_overrides: HashMap<ThemeName, FaceOverrideSet>,
    session_overrides: FaceOverrideSet,
    remaps: FaceRemapStore,
    revision: Revision,
}
```

一个 named Face 的基础解析优先级从低到高为：

1. 显式继承的父 Face；
2. `FaceDefinition::fallback`；
3. terminal-default fallback Theme；
4. 当前 active Theme；
5. 用户对所有 Theme 的 persistent override；
6. 用户对当前 Theme 的 persistent override；
7. 当前 Session 的临时 override。

每个 Theme 或 override layer 独立执行点分 fallback，然后把得到的单个
`FacePatch` 叠加到结果上。

缺失的 FaceName 不 panic：

- 返回空 patch，最终显示 underlying/root Face；
- 记录去重后的 unknown Face diagnostic；
- Theme 可以预先包含未知 Face，不产生 diagnostic；
- 只有实际 presentation 引用且所有 layer 都未知时才报告。

## 13. 类 Emacs Face remap

### 13.1 为什么 override 不够

用户 override 改变 named Face 在整个 Session 中的含义。以下需求需要 remap：

- 只放大或改变某个 Content 的正文；
- 同一 Content 的两个 View 使用不同显示风格；
- 非活动 pane 的 `ui.status-bar` 映射到 inactive Face；
- major Mode 为自己的 Content 提供基础 remap；
- 多个 minor Mode 分别增加 italic、背景或 underline，并能独立移除。

### 13.2 Scope

```rust
pub enum FaceRemapScope {
    Session,
    Content(ContentId),
    View(ViewId),
}
```

即使 scope 是 Content，remap 仍由 ClientSession 持有。它只表示当前客户端
如何显示该 Content，不进入 `ContentStore`，也不影响其他客户端。

### 13.3 Remap expression

```rust
pub enum FaceExpr {
    Named(FaceName),
    Patch(FacePatch),
}

pub struct FaceRemap {
    pub base: Option<Vec<FaceExpr>>,
    pub relatives: Vec<RelativeFaceRemap>,
}

pub struct RelativeFaceRemap {
    pub token: FaceRemapToken,
    pub owner: FaceContributionOwner,
    pub expressions: Vec<FaceExpr>,
}
```

`base` 用于替换 scope 内该 Face 的普通基础定义。没有 base 时继续使用全局
named Face 解析结果。

`relatives` 是独立贡献。新增 relative remap 返回 token，remove 必须使用该
token，避免一个扩展重写或删除另一个扩展的贡献。

### 13.4 Remap 优先级

对给定 `(ViewId, ContentId, FaceName)`：

1. 解析全局 named Face；
2. 应用 Session base remap 与 relative remap；
3. 应用 Content base remap 与 relative remap；
4. 应用 View base remap 与 relative remap；
5. 返回仍可与其他 decoration 合成的 `FacePatch`。

在同一 scope 内：

- base expression 从低到高依次合成；
- relative remap 按稳定 insertion sequence 从低到高合成；
- 后加入的 relative remap 优先级更高；
- 删除 token 后，其余 relative remap 顺序不变。

`Named` expression 通过普通 Face resolver 解析，但不会再次应用当前目标的
同 scope remap，防止自引用无限递归。跨 Face remap cycle 返回结构化错误。

### 13.5 生命周期

- View 关闭时删除对应 View remap；
- Content 删除时删除对应 Content remap；
- Mode detach 时删除 owner 为该 attachment 的 relative remap；
- Theme 切换保留 remap，因为 remap 引用语义 FaceName；
- 用户重载配置时原子替换 persistent override；
- Session 销毁时所有临时 override 与 remap 一起销毁。

## 14. 扩展与执行边界

### 14.1 启动期定义

未来 TypeScript config 可以声明：

```ts
editor.faces.define({
  name: "plugin.todo.warning",
  inherits: ["diagnostic.warning"],
  fallback: { underline: true },
});

editor.faces.override("syntax.comment", {
  italic: false,
});
```

启动期调用只写 loader draft。整个 config module 成功后，Mode、Face
definition、Theme selection 与 override 一次原子发布。config 失败时全部
回滚，不能留下部分 Face。

### 14.2 运行期修改

Mode callback 不得借用或直接修改 `FaceEnvironment`。运行时修改必须产生
typed operation，例如：

```text
FaceOperation::SetBase
FaceOperation::AddRelative
FaceOperation::RemoveRelative
```

TypeScript v2 callback 通过 `ctx.faces.setBase`、`ctx.faces.addRelative` 和
`ctx.faces.removeRelative` 产生这些 operation。scope 是 `session`、`content`
或 `view`；`addRelative` 同步返回安全整数 token，但实际 remap 仍只在 frame
成功后发布。

目标结合 operation origin 在 app 中解析。operation 进入现有
`ExecutionFrame`：

- frame 失败时不发布；
- frame 成功时与其他 prepared effect 一次提交；
- Face 变化不进入文本 transaction history；
- Mode 只能删除自己持有 token 的 relative remap；
- 用户命令可以拥有独立的 user contribution token。

### 14.3 被动 presentation

Mode 的 passive presentation callback 只能返回 FaceName，不能在刷新过程中
改变 Theme、override 或 remap，避免 presentation refresh 自身触发无限失效。

## 15. Render query 与绘制

`TextPresentation` 和 `StatusBarPresentation` 增加 root Face：

```rust
pub struct TextPresentation {
    pub base_face: PaintFace,
    pub selections: Selections,
    pub cursor_style: CursorStyle,
    pub selection_shape: SelectionShape,
    pub selection_face: FacePatch,
}

pub struct StatusBarPresentation {
    pub base_face: PaintFace,
    pub left: Vec<StatusBarSegment>,
    pub center: Vec<StatusBarSegment>,
    pub right: Vec<StatusBarSegment>,
}
```

AppQuery 解析时携带上下文：

```rust
pub struct FaceResolveContext {
    pub content: ContentId,
    pub view: ViewId,
}
```

Text root 使用 `ui.editor`；StatusBar root 使用 `ui.status-bar`。将来 popup、
menu 和 overlay 使用各自 root Face。

一个文本 cell 的绘制顺序从低到高为：

1. presentation root `PaintFace`；
2. 按 Mode presentation 顺序合成的 content decoration；
3. view decoration；
4. selection Face；
5. renderer 自身的临时视觉状态。

`FacePatch::Reset` 始终恢复步骤 1 的 root 属性。每次 cell 的最终结果转换为
`PaintFace` 后才交给 Canvas。

Renderer 必须：

- 使用 base Face 清空完整文本行；
- 使用 base Face 绘制文件末尾后的空白行；
- 使用 StatusBar base Face 填满整行；
- decoration 结束时恢复 base Face，而不是 `Face::default()`；
- render 完成和 TerminalGuard drop 时恢复终端 SGR 状态。

硬件 cursor shape 继续来自 Mode view policy。cursor 颜色不属于第一阶段的
可移植 TUI 能力。

## 16. Theme 文件格式

第一阶段使用无代码执行能力的 TOML。示例：

```toml
schema = 1
name = "catppuccin-mocha"
appearance = "dark"
inherits = "catppuccin-base"

[palette]
rosewater = "#f5e0dc"
mauve = "#cba6f7"
text = "#cdd6f4"
surface1 = "#45475a"
base = "#1e1e2e"
mantle = "#181825"

[faces]
"ui.editor" = { foreground = "text", background = "base" }
"ui.status-bar" = { foreground = "text", background = "mantle" }
"ui.selection" = { background = "surface1" }
"syntax.keyword" = { foreground = "mauve" }
"syntax.function.macro" = { foreground = "rosewater" }
```

属性接受：

- palette 名称；
- `#RRGGBB`；
- ANSI index；
- 布尔值；
- `{ reset = true }` 显式 reset。

`null` 或缺失字段等价于 `Unspecified`。TOML loader 不把空字符串、未知
palette 名或超出范围的 ANSI index 静默转换成默认值。

## 17. Catppuccin 组织

内建文件：

```text
runtime/themes/
├── terminal-default.toml
├── catppuccin-base.toml
├── catppuccin-latte.toml
├── catppuccin-frappe.toml
├── catppuccin-macchiato.toml
└── catppuccin-mocha.toml
```

`catppuccin-base` 是 `selectable = false` 的 fragment，定义共享 Face 到
palette 名的映射。四个 flavor 提供 palette 与 appearance。

第一阶段至少覆盖：

- `ui.editor`、`ui.selection`；
- active/inactive StatusBar；
- `syntax.*` 当前所有 capture；
- Markdown heading、link、raw、quote 和 list；
- `diagnostic.error/warning/info/hint` 的预留定义；
- `diff.plus/minus/delta` 的预留定义。

Theme 可以包含当前 UI 尚未使用的标准 Face，以便后续功能出现时保持视觉
一致性。

## 18. 高亮 capture 规则

Tree-sitter worker 不再把 capture 提前压扁。映射尽量保留完整语义：

```text
@function.macro     -> syntax.function.macro
@variable.parameter -> syntax.variable.parameter
@constant.builtin   -> syntax.constant.builtin
@type.builtin       -> syntax.type.builtin
@string.escape      -> syntax.string.escape
```

若 Theme 没有细粒度条目，Theme lookup 自动回退到较宽 scope。

不同高亮来源应尽量汇聚到相同标准词汇。LSP semantic token 的映射属于 LSP
presentation provider，不属于 Theme。Theme 不关心语义来自 Tree-sitter、
LSP 还是其他分析器。

## 19. Theme selection 与配置

第一阶段支持单个 ThemeName：

```text
vell --theme catppuccin-mocha file.rs
```

生产默认值可以是 `catppuccin-mocha`，测试构造器继续默认使用
`terminal-default`，减少既有字节断言迁移范围。

后续 `config.ts` 使用声明式接口：

```ts
editor.theme.use("catppuccin-mocha");
```

`load_user_configuration()` 返回完整的启动结果；`load_user_modes()` 保留为
只需要 Mode 的兼容入口：

```rust
pub struct LoadedEditorConfiguration {
    pub modes: Vec<Box<dyn Mode>>,
    pub theme: Option<ThemeName>,
    pub face_overrides: Vec<FaceOverride>,
}
```

V8 只返回 ThemeName。composition root 使用 `ThemeRegistry` 解析，不在脚本
host 中按 Catppuccin 名称硬编码分支。`LoadedEditorConfiguration` 只包含
protocol DTO；`FaceOverrideSet` 是 app/theme 内部索引，不进入 V8 crate 的
公开依赖。

## 20. 显示能力扩展点

当前 `Color` 支持 ANSI 与 RGB，但一个属性只保存其中一种。第一阶段可以明确
要求 Catppuccin 使用 true-color terminal。

后续若支持类似 Emacs 的 display-dependent spec，应扩展解析上下文，而不是让
Mode 判断终端：

```rust
pub struct DisplayProfile {
    pub color_depth: ColorDepth,
    pub appearance: Option<Appearance>,
    pub supports_italic: bool,
    pub supports_undercurl: bool,
}
```

ThemeDefinition 可以增加按 profile 匹配的 spec 列表。Frontend 报告
capability，Theme resolver 选择匹配项。该扩展不改变 FaceName、override、
remap 或 presentation contract。

## 21. Revision、cache 与切换

`FaceEnvironment` 持有单调 `Revision`。下列变化递增 revision：

- active Theme 切换；
- persistent/session override 变化；
- Face definition 集合变化；
- Content/View remap 增删；
- display profile 变化。

解析 cache 的逻辑 key 至少包含：

```text
(FaceRevision, FaceName, ContentId?, ViewId?)
```

可以按 scope 优化：没有局部 remap 的 Face 不必带 ContentId/ViewId。

Theme 切换只需要：

1. 替换 active `Arc<ResolvedTheme>`；
2. 增加 Face revision；
3. 清空或自然失效 resolved Face cache；
4. 请求 frontend redraw。

它不需要：

- 重新运行 Tree-sitter；
- 重建 Mode state；
- 刷新保存或 history 状态；
- 重新生成只含 FaceName 的 decoration layer；
- 调用 V8 callback。

当 presentation cache 保存的是 FaceName 而不是具体颜色时，这个性质自然成立。

## 22. 错误与诊断

Theme 与 Face 诊断至少包括：

```text
ThemeNotFound
ThemeInheritanceCycle
InvalidThemeColor
UnknownPaletteEntry
MissingRequiredFace
FaceDefinitionConflict
FaceInheritanceCycle
UnknownFaceReference
FaceRemapCycle
InvalidFaceRemapOwner
```

启动行为：

- 内建 Theme 错误阻止启动，表示安装或构建产物损坏；
- 显式选择不存在的 Theme 返回启动错误；
- 可选用户 Theme 解析失败记录 warning，并继续使用上次有效或 fallback Theme；
- 用户 config 的 Face draft 与 Mode draft 一起原子回滚；
- render path 遇到 unknown Face 使用 root Face，不 panic。

诊断接口应能回答：

- 当前 ThemeName 与 appearance；
- 一个 FaceName 最终命中了哪个点分 scope；
- 哪些 layer 修改了哪些属性；
- 当前 View/Content 是否存在 remap；
- Face definition 的 owner；
- inheritance/remap cycle 的完整路径。

## 23. 与当前实现的迁移

当前代码中的概念迁移如下：

| 当前概念 | 目标概念 |
|---|---|
| `FaceName` | 保留，移动到稳定 style contract |
| `Face` | 拆为 `FacePatch` 与 `PaintFace` |
| `Face::overlay` | 改为三态 patch composition |
| `FaceRegistry::faces` | 拆为 catalog、Theme layer 与 override |
| `FaceRegistry::set` | 删除，改为显式 Theme/override API |
| `FaceConflict` | 仅用于 FaceDefinition provider conflict |
| Mode `faces()` | 仅允许插件私有 FaceDefinition |
| Tree-sitter ANSI Face | 移入 `terminal-default` Theme |
| `AppQuery::faces` | 改为 `FaceEnvironment` |
| renderer `Face::default()` | 改为 presentation root Face |

迁移期间保留既有 public constructor：

- `App::new` 和 `App::with_modes` 使用 terminal-default；
- 新增接收 Theme 或 ThemeName 的 options constructor；
- 测试 helper 默认不切换到 Catppuccin；
- 既有 decoration ordering 与 Mode policy precedence 不变。

## 24. 实现阶段

### 阶段 A：基础 Theme

- 新增 `vell-theme`；
- 引入 `FaceValue`、`FacePatch` 与 `PaintFace`；
- 实现 Theme TOML、palette、单父 inheritance；
- 实现 terminal-default 与 Catppuccin 四个 flavor；
- ClientSession 持有 active Theme；
- AppQuery 解析 root、selection、status 和 decoration Face；
- Renderer 正确填充背景并恢复终端状态。

### 阶段 B：标准语义与 fallback

- 建立 host 标准 Face catalog；
- Tree-sitter 保留完整 capture；
- 实现点分 fallback；
- 从 Mode 移除标准 Face 颜色；
- 增加 active/inactive StatusBar Face。

阶段 A 与 B 可以在同一变更中完成，避免同时维护两套标准 Face 来源。

### 阶段 C：用户 customization

- persistent global override；
- per-Theme override；
- `config.ts` Theme selection 与 Face override；
- 配置 draft 原子发布；
- override 来源诊断。

### 阶段 D：扩展 Face 与局部 remap

- 插件私有 FaceDefinition；
- 显式 Face inheritance；
- Session、Content、View remap；
- token 化 relative remap；
- typed runtime operations；
- Mode detach 与 View close 自动清理。

### 阶段 E：显示能力

- Frontend `DisplayProfile`；
- true-color、ANSI256、ANSI16 或 mono spec；
- underline style、strikethrough、dim 等扩展属性；
- 必要时引入内部 FaceId cache。

## 25. 测试要求

### 25.1 `vell-protocol`

- `Unspecified`、`Value`、`Reset` 合成；
- bool 显式 `false` 不等于 unspecified；
- Reset 恢复 root 属性；
- PaintFace 转换确定。

### 25.2 `vell-theme`

- palette 引用与字面颜色；
- Theme inheritance 后再解析子 palette；
- Theme cycle 与未知父节点；
- 点分 fallback 选择最近 scope；
- Catppuccin 四个 flavor 的关键官方色值；
- abstract fragment 不出现在可选列表。

### 25.3 `vell-mode`

- 标准 namespace definition 被拒绝；
- 私有 definition 幂等注册；
- 不同 owner 冲突；
- presentation 只保存 FaceName。

### 25.4 `vell-app`

- layer 优先级逐属性生效；
- Theme 改颜色后保留 fallback italic；
- Theme 不参与 provider conflict；
- Theme 切换不刷新 Mode 或后台分析；
- Content/View remap 不泄漏到其他 scope；
- relative token 可独立删除；
- frame 失败不发布 remap；
- View close 和 Mode detach 清理 owner contribution。

### 25.5 `vell-tui`

- Buffer 空白行使用 `ui.editor` 背景；
- StatusBar 未写满部分使用 StatusBar base；
- syntax 前景与 selection 背景正确组合；
- Reset 恢复 root 而非较低 decoration；
- render 与退出恢复 SGR；
- terminal-default 保持既有基础字节行为。

### 25.6 `vell-plugin-v8`

- Face schema 与 `editor.d.ts` 同步；
- config 失败回滚 Face 与 Mode draft；
- runtime callback 不能保留 Face host handle；
- 非法跨 owner remove 返回结构化错误；
- TypeScript data 保持 JSON-compatible owned value。

## 26. 不变量

- Theme 不拥有 Content、View、Mode 或 Scene；
- Kernel 不持有客户端视觉配置；
- 标准 FaceName 没有 Mode provider；
- Mode 与分析器产生语义，不选择 active Theme；
- Theme 和用户覆盖按属性合成；
- local remap 只存于 ClientSession；
- Face 变化不进入文本 undo/redo；
- presentation cache 保存 FaceName，不固化 Theme 颜色；
- render path 不执行扩展代码或 Theme IO；
- Theme 切换只使 Face cache 与屏幕失效；
- relative remap 通过 token 独立管理；
- View/Content/Mode 生命周期结束时清理其 remap contribution；
- 未知 Face 降级到 root Face，不导致渲染 panic。

## 27. 参考资料

- [Zed Theme system][zed-theme]
- [Zed syntax theme source][zed-syntax]
- [Helix themes][helix-theme]
- [Helix built-in themes][helix-runtime]
- [Neovim highlight API][neovim-highlight]
- [GNU Emacs Defining Faces][emacs-defining-faces]
- [GNU Emacs Face Attributes][emacs-face-attributes]
- [GNU Emacs Face Remapping][emacs-face-remapping]
- [Catppuccin style documentation][catppuccin-style]

[zed-theme]:
  https://zed.dev/docs/themes
[zed-syntax]:
  https://github.com/zed-industries/zed/tree/main/crates/syntax_theme
[helix-theme]:
  https://docs.helix-editor.com/themes.html
[helix-runtime]:
  https://github.com/helix-editor/helix/tree/master/runtime/themes
[neovim-highlight]:
  https://neovim.io/doc/user/api.html#nvim_set_hl()
[emacs-defining-faces]:
  https://www.gnu.org/software/emacs/manual/elisp.html#Defining-Faces
[emacs-face-attributes]:
  https://www.gnu.org/software/emacs/manual/elisp.html#Face-Attributes
[emacs-face-remapping]:
  https://www.gnu.org/software/emacs/manual/elisp.html#Face-Remapping
[catppuccin-style]:
  https://github.com/catppuccin/catppuccin/tree/main/docs
