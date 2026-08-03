# Vell Context

Vell 是一个终端文本编辑器。本文档定义其核心领域
语言：Content（可打开关闭的数据对象）与 View
（聚焦与呈现单位）的边界和关系。

## Language

**Content**：
持有自身数据、拥有独立生命周期、可被用户直接
操作（聚焦/切换/编辑）的对象。场景中唯一获得
ContentId 的实体。
_Avoid_: buffer（buffer 只是 Content 的一种）、
对象、元素

**View**：
交互会话与聚焦单位。绑定一个 Content，持有独立
view state 与 viewPolicy。同一 Content 可被多个
View 绑定。焦点落在 View 上，切换目标是 Content。
_Avoid_: 窗口、pane（vim 中 window 的角色）

**Listed content**：
可切换的 Content。进入切换列表（如 `:buffers`），
有用户可见编号（按 ContentId 排序的 1..n，纯派生）。
_Avoid_: 可切换 buffer

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
- 展示 Mode 的锚点在 View 上，不在 Content 上。
- 用户可见编号只对 listed content 派生；插件 API
  使用不透明 ContentId。
