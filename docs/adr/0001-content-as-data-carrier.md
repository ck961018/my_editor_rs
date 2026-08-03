# Content 是数据承载者，不是可切换对象

Content 的定义收敛为"持有自身数据、拥有独立生命
周期、可被用户直接操作的对象"。状态栏与 gutter
是 View 的派生呈现，不是 Content；为此删除
`ContentKind::StatusBar`，展示 Mode 的锚点从
Content 移到 View。

此前状态栏以一个空壳 `StatusBar` Content 存在，
唯一作用是给 Mode 链当锚点——这是实现迁就架构，
不是领域本质。删掉后"Content"回归本义，启动时
的 ContentId 编号困惑（状态栏占 1）也随之消失。
