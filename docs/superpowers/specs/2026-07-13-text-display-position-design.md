# 文本位置与显示位置分离设计

**日期：** 2026-07-13
**状态：** 已实施

## 目标

- Selection 只长期保存文档 `TextOffset`，不同时缓存 offset、row 与 column。
- `Buffer` 按当前内容从 TextOffset 派生逻辑 `TextPoint { row, col }`。
- TUI 将 TextPoint 与 viewport/layout 组合为 `DisplayPoint`，后端不感知屏幕坐标。

## 类型与数据流

```text
Selection { anchor: TextOffset, head: TextOffset }
                    │ ContentQuery::TextPoints
                    ▼
             TextPoint { row, col }
                    │ TUI viewport + layout rect
                    ▼
          DisplayPoint { row, col } (screen cell)
```

`TextOffset` 使用 Rope 的 char offset，与 UTF-8 byte offset 不同。`TextPoint` 是一次查询的
owned 派生值，不写回 View selection。DisplayPoint 只存在于 TUI。

Text presentation 继续携带 offset selections。渲染器按需用 `TextPoints(Vec<TextOffset>)`
查询 primary selection 的 anchor/head；Buffer 在当前 revision 上计算逻辑行列。未来若性能
测量证明需要缓存，缓存必须同时携带 Content revision 并在不匹配时失效。

## 显示宽度边界

本项建立逻辑位置与显示位置的边界，但不提前选择 Unicode width/grapheme 或软换行算法。
当前 TUI 的逻辑 col 到 cell col 仍是一对一适配器；tab、全角、组合字符和软换行可以在该
适配器内演进，而无需修改 Selection 或 Buffer 编辑模型。

## 非目标

- 不实现 grapheme cluster selection、tab stop、Unicode wcwidth 或软换行 DisplayMap。
- 不缓存 TextPoint，不增加依赖。
- 不改变编辑命令、selection anchor/head 方向语义或多光标约束。

## 验收

- 持久 Selection 不再包含 row/col。
- Buffer 能从任意合法/越界 TextOffset 派生并钳制 TextPoint。
- TUI 只用 TextPoint 跟随 viewport，并显式计算 DisplayPoint。
- 文本移动、编辑、selection 高亮和 viewport 测试保持通过。
- fmt、test、clippy 与 diff 检查通过。
