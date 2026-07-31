# 基础代码编辑器 Roadmap

**状态：** 实施中（M1、M2 已完成）

**更新日期：** 2026-07-31

## 1. 目标

本路线图定义 Vell 从当前单 buffer 编辑器原型，
演进到“多 buffer 可日用”的基础代码编辑器所需工作。

评估分为两个层次：

1. Rust 与宿主基础能力；
2. TypeScript、Vim 和命令系统接线。

第一层是本路线图的主要交付目标。
第二层记录后续产品接线，
但命令输入 UI 不阻塞基础能力完成。

本路线图遵守现有架构文档：

- [Core 依赖方向][core-deps]；
- [Editor Kernel Architecture][kernel];
- [Command 与执行所有权][command];
- [TypeScript 脚本架构][typescript]。

[core-deps]: ../design/core-dependency-direction.md
[kernel]: ../design/editor-kernel-architecture.md
[command]: ../design/command-execution-ownership.md
[typescript]: ../design/typescript-scripting-architecture.md

## 2. 范围

### 2.1 包含

基础能力包括：

- grapheme-safe 光标、删除、选择和绘制；
- hard tab 输入与正确显示；
- tab width、indent width 和空格/tab 配置；
- 当前行和选中行的 indent/outdent；
- TypeScript 提供的缩进、注释和配对策略；
- 内部 copy/cut/paste；
- 系统剪贴板和 bracketed paste；
- 当前 buffer 的 literal 与 regex 查找替换；
- 多 buffer 的 new/open/list/switch/close；
- Save As、reload 和显式 force 操作；
- dirty close/quit 保护；
- 外部文件修改冲突保护；
- `DuplicateLines`；
- `MoveLinesUp`；
- `MoveLinesDown`。

### 2.2 不包含

以下能力不作为本路线图的完成条件：

- LSP、补全和智能提示；
- 鼠标；
- 矩形选择和多光标入口；
- 软换行；
- 行号和 gutter；
- 文件树；
- 项目级搜索和跨文件替换；
- session、autosave 和崩溃恢复；
- 非 UTF-8 编码；
- command palette、Ex 输入栏和命令补全；
- AST 重构和结构化选择。

UTF-8、LF 和 CRLF 是本阶段支持的文本格式边界。

## 3. 当前基础

Vell 已有以下可复用能力：

- Rope 文本存储；
- anchor/head selection；
- 多 selection 数据模型；
- `TextChangeSet` 和原子批量编辑；
- transaction、rollback、undo 和 redo；
- 多 View 共享 Content；
- `ContentStore` 多 Content 存储；
- Scene、split、focus 和 viewport；
- dirty 状态；
- 当前路径的异步原子保存；
- stale save completion 保护；
- TypeScript Mode 和 typed operation；
- 事务化 `applyEdits`；
- revision-safe Worker 结果发布。

当前主要缺口如下：

| 能力 | 状态 | 优先级 |
| --- | --- | --- |
| Grapheme 编辑 | M1 已完成 | P0 |
| Tab 输入和显示 | M1 已完成 | P0 |
| 多 buffer 生命周期 | 只有底层 Content 结构 | P0 |
| 文件安全 | 缺 dirty guard 与外部冲突检测 | P0 |
| Clipboard | 内部与系统能力均缺失 | P0 |
| Search/replace | 只有当前行单字符查找 | P1 |
| 语言编辑策略 | 有 `applyEdits` 底座，无正式契约 | P1 |
| Duplicate/move line | 缺失 | P2 |

## 4. 架构约束

### 4.1 Crate 所有权

| Crate | 本路线图中的职责 |
| --- | --- |
| `vell-protocol` | Tab 输入、paste event、呈现配置等中立 DTO |
| `vell-core` | grapheme、编辑计划、搜索和 clipboard 纯算法 |
| `vell-mode` | typed operation 与语言编辑策略契约 |
| `vell-app` | buffer 生命周期、history、文件任务和内部 clipboard |
| `vell-frontend` | 系统剪贴板能力接缝 |
| `vell-tui` | grapheme/tab 绘制、bracketed paste 和平台 clipboard |
| `vell-plugin-v8` | TypeScript schema、校验与 typed operation 映射 |
| `runtime/` | Vim 行为和具体语言策略 |

不得新增第二张 buffer 表。
`ContentStore` 继续是唯一 Content 表。

`vell-app` 不借出或匹配 `Buffer` 具体变体。
新的文件生命周期通过中立 Content input、query 和 outcome 执行。

`vell-core` 不依赖 Tokio、Mode、Frontend、终端或 V8。

`vell-app` 不依赖 V8、crossterm 或 Taffy。

### 4.2 执行约束

所有宿主 mutation 必须继续经过：

```text
Mode 或命令
-> OperationRequest
-> app target resolver
-> ExecutionFrame
-> Content / View / history / prepared effect
```

不得让 TypeScript 持有可变 Buffer、View 或 App。

语言策略由 TypeScript 决定，
并将 owned DTO 或 typed edit intent 交给 Rust。
core 和 app 不在编辑或渲染过程中反向调用 V8。

每个用户动作只产生一个 `ExecutionFrame`。
一个行操作、paste 或 replace-all 只产生一个 undo record。

## 5. 依赖关系

```text
Grapheme boundary
├── selection 和 cursor 正确性
├── clipboard range 正确性
├── search result selection
└── duplicate/move line selection

Tab 输入与显示
└── indentation 配置
    └── TypeScript indentation 策略

Buffer manager
├── open/new/switch/close
├── path identity
├── Save As 和 reload
└── dirty 与外部冲突保护
```

文本边界工作与 buffer 生命周期工作可以并行推进。
它们的 TypeScript API 应在 Rust 语义稳定后再公开。

## 6. 里程碑 M1：文本边界与 Tab

**状态：** 已完成（2026-07-31）

### 目标

建立统一的用户可见文本边界，
并让已有 hard tab 文件可以正确输入、显示和滚动。

### 交付

#### `vell-core`

- 新增跨 Rope chunk 的 grapheme boundary 算法；
- `TextOffset` 继续保存 char index；
- cursor 和 selection endpoint 必须位于 grapheme boundary；
- 左右移动按 grapheme 移动；
- backward/forward delete 删除完整 grapheme；
- 上下移动按 grapheme 列解析；
- reconcile 和 clamp 统一执行 boundary 规则；
- CRLF 规则并入统一边界处理。

绝对程序化 `TextChangeSet` 仍使用 char range。
它可以改变 grapheme 组成，
但编辑后所有 View selection 必须重新落到合法边界。

#### `vell-protocol`

- 新增 `KeyCode::Tab`；
- 新增 `KeyCode::BackTab`；
- 在文本呈现配置中加入 `tab_width`。

#### `vell-mode`

- View policy 可以声明 `tab_width`；
- 对配置值执行非零和上限校验。

#### `vell-tui`

- 正确翻译 Tab 与 BackTab；
- 按 grapheme cluster 绘制文本；
- 使用完整 cluster 的终端宽度；
- hard tab 按当前 cell column 展开；
- horizontal viewport 正确裁剪 tab 与宽字符。

### 验收

必须覆盖：

- combining mark；
- ZWJ emoji；
- flag 和 skin-tone sequence；
- grapheme 跨 Rope chunk；
- LF 与 CRLF；
- 行首和非行首 hard tab；
- tab 跨 horizontal viewport 边界；
- 多 View selection 映射；
- undo/redo 后的 boundary 不变量。

## 7. 里程碑 M2：Buffer 生命周期与文件安全

**状态：** 已完成（2026-07-31）

### 目标

让 Kernel 能完整管理多个 buffer，
即使暂时没有用户可见的命令输入 UI。

### 交付

#### Buffer manager

在 `vell-app` 增加单一生命周期 owner，负责：

- 分配生产 `ContentId`；
- 创建 untitled buffer；
- 异步打开路径；
- 列出 buffer metadata；
- 将 View 切换到指定 Content；
- 全局关闭 buffer；
- 清理最后一个 View 后的资源；
- 保留按 `ContentKind` 排序的默认 Mode profile；
- 维护 normalized path 到 `ContentId` 的索引。

`ContentStore` 增加中立的 remove 能力。
不得建立独立 buffer registry 保存第二份 Content。

#### Typed operation

在 `vell-mode` 定义以下宿主操作：

- new buffer；
- open path；
- list buffers；
- switch buffer；
- close buffer；
- Save As；
- reload；
- force save/reload/close。

这些 operation 是本里程碑交付物。
命令行、Ex UI 和参数输入界面后置。

#### Close 语义

保留两个不同概念：

- close pane：关闭一个 View/Space；
- close buffer：关闭一个 Content 及其全部 View。

普通 close buffer 遇到 dirty 内容必须拒绝。
只有显式 force 可以丢弃修改。

普通 quit 在任一 buffer dirty 时必须拒绝。

#### Save As

- 写入成功前不改变 buffer path；
- 成功后原子更新 path index 和 backing state；
- 目标路径已由其他 buffer 占用时拒绝；
- 失败后原 path 和 dirty 状态不变。

#### Reload

- 普通 reload 在 buffer dirty 时拒绝；
- force reload 替换全文；
- reload 产生规范 `ContentChange`；
- 所有 View selection 通过 change 重新映射；
- force reload 清空该 Content 的 undo/redo history。

#### 外部冲突

打开或成功保存时记录文件 baseline。
普通 Save 必须验证磁盘仍对应 baseline。

发生冲突时：

- 不覆盖磁盘；
- 不清除 dirty；
- 返回结构化 conflict；
- force Save 可以显式覆盖。

第一版不要求 filesystem watcher。
检查发生在 save 和 reload 边界。

普通 Save 会在异步写任务内再次检查 baseline，
然后立即执行原子替换。
这属于 best-effort 冲突保护：
不合作的外部进程若恰好在检查与替换之间写入，
跨平台 pathname API 无法提供绝对 CAS 保证。
若未来需要更强保证，再引入平台特定原语。

### 验收

必须覆盖：

- 同时打开多个文件；
- 同路径重复打开；
- untitled buffer；
- 不存在路径；
- Save As 成功与失败；
- dirty close/quit；
- force close/quit；
- 外部内容改变后的普通 Save；
- force Save；
- dirty reload；
- pending save 时 close；
- 一个 buffer 被多个 View 展示；
- 关闭后 Mode state、history、task 和 face 清理；
- Windows path 大小写和分隔符；
- symlink 与 normalized path 策略。

## 8. 里程碑 M3：通用代码编辑原语

### 目标

补齐与具体 Vim 快捷键无关的代码编辑操作。

### 配置模型

显示配置与编辑配置必须分开：

- `tab_width`：hard tab 的显示宽度，可属于 View policy；
- `indent_width`：一次 indent 的列数；
- `insert_spaces`：indent 时插入空格还是 hard tab。

不要用一个字段同时表达 tab 显示宽度和 indent 步长。

### `vell-core` 操作

新增 selection-aware 编辑命令：

- `IndentLines`；
- `OutdentLines`；
- `DuplicateLines`；
- `MoveLinesUp`；
- `MoveLinesDown`。

所有操作必须：

- 将 selection 解析为完整逻辑行 block；
- 合并重叠或相邻 block；
- 支持多个 selection；
- 保持 LF/CRLF；
- 正确处理无尾换行的最后一行；
- 返回明确的目标 selections；
- 只生成一个 `TextChangeSet`；
- 只形成一个 undo record。

### 验收

必须覆盖：

- collapsed cursor；
- 正向和反向 selection；
- visual-line 风格 selection；
- 空行；
- 首行与末行；
- 最后一行无换行；
- 相邻和重叠多 selection；
- 首行向上移动为 no-op；
- 末行向下移动为 no-op；
- undo/redo 恢复文本和 selection。

## 9. 里程碑 M4：语言编辑策略

### 目标

Rust 定义语言中立的编辑能力，
TypeScript 提供具体语言规则。

### Rust contract

`vell-mode` 定义 owned strategy DTO：

- indentation decision；
- line comment delimiter；
- block comment delimiters；
- open/close pair；
- operation 所需的 selection 和配置参数。

`vell-core` 只负责执行通用、原子的文本变换：

- 根据决定插入或移除缩进；
- toggle line/block comment；
- 插入配对字符；
- selection wrapping；
- 在空 pair 内执行配对退格。

Rust 不按扩展名硬编码 Rust、Markdown 或 TypeScript。

### TypeScript contract

`runtime/editor.d.ts` 暴露 typed primitive 和策略数据。
`vell-plugin-v8` 校验全部输入并产生 `OperationRequest`。

TypeScript 根据以下信息选择策略：

- `resourceName`；
- `resourcePath`；
- 当前文本 snapshot；
- selection；
- Mode content/view state。

第一版提供一个 Rust 语言参考实现，
并保留无语言策略时的普通换行和输入行为。

### 验收

必须覆盖：

- Enter 继承当前缩进；
- `{}` 上下文的 Rust 增减缩进；
- line comment toggle；
- 部分行与整行 selection；
- 空行 comment；
- quote/bracket pair；
- wrap selection；
- 输入 close character 时避免无条件重复；
- callback 或后续 operation 失败时完整 rollback；
- TS schema 与 Rust adapter 契约同步。

## 10. 里程碑 M5：Clipboard

### 目标

提供可靠的跨 buffer 内部 clipboard，
并通过 Frontend 接入系统剪贴板。

### 内部 clipboard

`vell-core` 定义 owned `ClipboardPayload`，至少包含：

- character-wise；
- line-wise。

core 提供：

- 从 selection 提取文本；
- cut edit plan；
- paste-before/paste-after edit plan；
- 多 selection paste 规则；
- paste 后 selection 计算。

`Kernel` 持有唯一内部 clipboard。
clipboard 不属于 Buffer、View 或 Mode state。

### 系统 clipboard

`vell-frontend::Frontend` 增加字符串级、fallible 的读写 seam。
它不得接受 `vell-core` 的 clipboard 类型。

`FrontendEvent` 增加 owned paste event。
`vell-tui` 映射 bracketed paste，
并提供选定平台的 clipboard provider。

### 安全规则

- internal clipboard 始终是可靠基线；
- system clipboard 失败不能清空 internal clipboard；
- cut 的文本 mutation 与内部 payload 更新保持原子；
- 外部 clipboard 写入是 prepared effect；
- 外部写入失败不能导致已经删除的文本不可恢复；
- 多行 paste 必须作为一次 history 操作。

### 验收

必须覆盖：

- character-wise copy/cut/paste；
- line-wise copy/cut/paste；
- 跨 buffer paste；
- 多 selection 数量相同与不相同；
- 空 selection；
- CRLF；
- system clipboard 不可用；
- clipboard write failure；
- bracketed multiline paste；
- rollback 和 undo/redo。

## 11. 里程碑 M6：当前 Buffer 查找替换

### 目标

提供与命令 UI 无关的当前 buffer 搜索服务。

### `vell-core` 搜索模型

新增：

- `SearchPattern::Literal`；
- `SearchPattern::Regex`；
- case sensitivity；
- forward/backward direction；
- wrap；
- `SearchMatch` char range；
- `find_from`；
- `replace_next`；
- `replace_all`；
- invalid regex 结构化错误。

Regex replacement 使用 Rust regex crate 的 capture replacement 语义。

第一版允许从 `TextSnapshot` 生成 owned `String` 后搜索。
没有性能数据前，不实现跨 Rope chunk 的自定义 regex engine。

### 执行规则

- regex byte range 必须安全转换为 char range；
- zero-width match 必须保证前进；
- replace-all 只生成一个 `TextChangeSet`；
- stale snapshot 必须拒绝；
- 搜索结果进入 View selection 时遵守 grapheme boundary 规则；
- pattern 和历史由后续 TS/Vim state 保存；
- Rust 不新增全局 search session。

### 验收

必须覆盖：

- literal forward/backward；
- wrap 与 no-wrap；
- case-sensitive/insensitive；
- invalid regex；
- capture replacement；
- zero-width regex；
- Unicode 和 grapheme；
- CRLF；
- replace next/all；
- stale snapshot；
- undo/redo 和 rollback。

## 12. 里程碑 M7：TypeScript 与产品接线

本里程碑依赖前述 Rust/宿主语义稳定。

### Vim 接线

后续内建 Vim Mode 应接入：

- Tab/Shift-Tab；
- Enter、`o/O` autoindent；
- `>>` 和 `<<`；
- comment 与 pair；
- `y/d/c/p/P` 和 registers；
- unnamed 与 system clipboard register；
- `/`、`?`、`n/N`、`*`、`#`；
- replace command；
- duplicate/move line action；
- buffer/file typed operation。

### 命令系统接线

命令系统实现后再提供：

- open/edit path；
- buffer list 和 switch；
- close buffer；
- Save As；
- reload；
- force variants；
- search/replace 参数输入；
- dirty、conflict 和 invalid regex 错误展示。

命令 palette、历史和补全不属于本路线图。

## 13. 完成定义

### 13.1 Rust/宿主基础完成

满足以下条件时，基础层完成：

- M1 至 M6 全部验收通过；
- typed operation 可表达全部能力；
- command UI 不存在时也能通过 Rust 集成测试调用；
- TypeScript schema 已暴露稳定 primitive；
- 无 mutation 绕过 `ExecutionFrame`；
- dirty 和外部冲突不能被普通关闭或保存绕过；
- crate 依赖方向保持不变。

### 13.2 产品接线完成

满足以下条件时，基础代码编辑器产品层完成：

- M7 的 Vim 行为可以通过生产输入路径触发；
- 多 buffer 文件操作可以通过命令系统触发；
- 所有拒绝和失败都有用户可见反馈；
- 不依赖测试代码手工注入 Content 或 command。

## 14. 验证门槛

每个里程碑按影响范围运行最小检查。
跨 crate contract 完成时运行完整门槛：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
npx tsc --noEmit -p runtime/type-tests/tsconfig.json
```

依赖边界改动额外检查：

```text
cargo metadata --no-deps
cargo tree -p vell-app -e normal
```

验收时确认：

- `vell-app` 普通依赖不含 V8、Taffy 和 crossterm；
- `vell-tui` 不依赖 app、core、mode 或 V8；
- `vell-core` 不依赖 Tokio、Mode、Frontend 或终端；
- `runtime/editor.d.ts` 与 Rust schema 同步；
- Markdown 行宽、链接和 `git diff --check` 通过。

## 15. 实施前决策门

开始对应里程碑前，需要固定以下细节：

1. 现有文件与不存在路径的 normalized path 规则；
2. Windows 大小写与 symlink 去重策略；
3. 外部文件 baseline 的可靠表示；
4. system clipboard 的本地与远程终端 provider；
5. 多 selection paste 数量不一致时的规则；
6. regex match 落在 grapheme 内部时的 selection 规则。

这些决策不得改变本路线图的 crate 所有权和安全不变量。
