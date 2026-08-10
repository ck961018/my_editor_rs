# Vell Context

Vell 是一个终端文本编辑器。本文档定义升级目标的
核心领域语言：Content（数据模型）、View（完整交互
单元）与 Pane（View 的显示区域）的边界和关系。
它不表示所有阶段均已实现；当前差距与迁移顺序见
[View 中心架构路线图](docs/roadmap/view-centered-architecture.md)。

## Language

**Content**：
持有自身数据并拥有独立生命周期的数据模型。
View 可以按角色持有或借用 Content；Content 不是
焦点或通用切换的目标。
_Avoid_: buffer（buffer 只是 Content 的一种）、
对象、元素

**View**：
完整的展示、交互、状态、行为与生命周期单元。
View 可以绑定零个或多个 Content、控制多个直属
Pane，并可组合其他完整 View。
_Avoid_: 窗口、pane（vim 中 window 的角色）

**Content binding**：
View 以命名角色使用某个 Content 的关系。改变绑定
保留原 View，不属于 View switch。
_Avoid_: 切换 buffer、切换 Content

**Pane**：
由 View 控制的显示与事件区域。Pane 不拥有独立数据、
状态、行为或生命周期。
_Avoid_: 子 View、组件、Content

**Switchable View**：
允许被通用 View switch 替换的 View。焦点 Pane 所属
View 的最近 switchable 祖先是默认切换目标。
_Avoid_: 可切换 Buffer、可切换 Content

**View switch**：
用另一个完整 View 替换 Switchable View 的操作，包含
其状态、直属 Pane 与子 View；不表示改变 Content binding。
_Avoid_: buffer switch、Content switch

**Listed content**：
进入用户可见 Content 清单的 Content，可被选择为
View 的数据绑定，但不是通用切换目标。
_Avoid_: 可切换 Content、可切换 buffer

**Unlisted content**：
有 ContentId 与生命周期，但不在切换列表的 Content
（未来：文件树、侧边栏面板）。对应 vim 的
unlisted buffer 概念。
_Avoid_: 面板（易与派生呈现混淆）

**Derived presentation**：
从 Content 数据、View 设置与 Mode 状态计算出的
显示，不持有数据、无独立生命周期。状态栏、gutter、
行号属于此类；它们是 View 的属性，不是对象。
_Avoid_: 状态栏、行号栏、gutter（当作对象指称时）

## Rules

- Content 是"可以打开和关闭的东西"；派生呈现永远
  打不开也关不上。
- ContentKind 是封闭枚举、静态分派；当前只保留
  Buffer，未来按需扩展（Terminal、Web、面板）。
- Buffer 是 ContentKind，不是用户操作或命令的目标。
- 通用切换只替换最近的 Switchable View；改变 Content
  binding 必须由具体 View 的行为表达。
- Mode attachment 位于 View；多个 attachment 可以按
  Content 共享 Mode state。
- 用户可见编号只对 listed content 派生；插件 API
  使用不透明 ContentId。
