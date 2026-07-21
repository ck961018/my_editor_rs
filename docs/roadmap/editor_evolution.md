# 编辑器演进 Roadmap

**状态：** 执行中（M0 至 M6 已完成）

**更新日期：** 2026-07-21

**当前 package：** `modeleaf`

**名称状态：** M3 已采用 `Modeleaf`；`Eido` 未通过 M0 冲突检查

## 1. 评估结论

对话中给出的路线图方向总体合理，但不应原样执行。当前仓库已经完成旧
Roadmap 的 R1 至 R10，具备 Content/View 分离、Mode adapter、typed
operation、事务回滚、增量 presentation 和 TypeScript 后台 analysis。
后续不需要重新设计这些模型。

源码核对确认了以下真实问题：

- 主 `ScriptHost` 在 UI 线程同步执行 module、state factory、input、action
  和 apply callback；
- 后台 worker 已有取消与超时，但主 isolate 没有统一 watchdog；
- `script.rs` 同时承担 host、module、schema、bridge 和 Mode adapter；
- `ModeState` 使用 `Any + clone_box`，首次写入和部分 checkpoint 会复制完整
  状态；
- 当前只有 `Buffer` 和 `StatusBar` 两种封闭 Content；
- 所有逻辑边界仍处于一个 binary crate，V8 会进入全部构建路径。

因此保留原路线图的目标，但调整实施方式：

1. 脚本执行预算先于品牌重命名和 workspace 拆分；
2. 只拆安全改造和依赖边界实际需要的模块，不按文件长度机械拆分；
3. 项目重命名先在单 crate 中完成；
4. workspace 按真实依赖逐个提取，不预建空 crate；
5. Mode state 先测量复制成本，再决定是否引入 journal 或结构共享；
6. 第三种 Content 出现前，不引入声明宏或动态 Content registry。

## 2. 全程不变项

所有阶段必须保持以下边界：

- `App<F: Frontend>` 使用泛型静态分发；
- `app` 与 `tui` 不互相依赖，具体接线只位于 composition root；
- `core` 不依赖终端、布局、异步运行时或 V8；
- `protocol` 只保存中立数据和共享契约；
- Content 与 View state 分离，同一 Content 的多个 View 独立持有
  selections；
- `Content` 保持 Rust 内核拥有的封闭集合；
- Mode 只产生有序 operation，不直接借用可变 App、Content 或 View；
- 一次输入只使用一个 `ExecutionFrame`，失败时不提交部分 operation 或
  Mode state；
- render path 只读取缓存，不调用 Mode、V8 或 worker；
- selection 继续使用 `anchor/head`，collapsed cursor 等于 primary head；
- v1 TypeScript 配置在明确的版本迁移决定前继续可用。

依赖方向在单 crate 和未来 workspace 中保持同一语义：

```text
frontend -> protocol
app      -> frontend + core + protocol
tui      -> frontend + terminal + protocol
main     -> app + tui + terminal
terminal -> protocol
core     -> protocol/std
protocol -> std
```

## 3. M0：固定基线与决策门槛

**优先级：** P0

**状态：** 已完成（2026-07-21）

**记录：** [`m0_baseline.md`](m0_baseline.md)

**目标：** 为安全改造、重命名和边界提取建立可比较的基线。

工作项：

1. 记录当前测试、Clippy、格式化和严格 TypeScript 检查结果；
2. 增加启动、普通输入、脚本输入和大文件 presentation 的基准；
3. 记录 Mode draft clone 次数、耗时和可估算的状态大小；
4. 在脚本设计文档中明确：worker 已受预算约束，主 `ScriptHost` 尚未；
5. 正式决定重命名前，检查候选名在目标 registry、仓库托管平台和商标
   场景中的可用性；M0 已排除 `Eido`。

验收：

- 基线命令可在 CI 或本地重复运行；
- 后续性能判断有数字依据；
- 设计文档不把主 isolate 的预算描述为已实现；
- 名称检查已排除 `Eido`，M3 已对 `Modeleaf` 重复相同检查。

## 4. M1：主 ScriptHost 执行预算与恢复

**优先级：** P0

**依赖：** M0

**状态：** 已完成（2026-07-21）

**记录：** [`m1_script_safety.md`](m1_script_safety.md)

这是当前最高优先级。目标不是把同步 callback 改成异步，而是保证用户脚本
不能无限占用 UI 线程。

### 4.1 统一调用入口

为以下执行统一建立 invocation 边界：

- module evaluation；
- content/view state factory；
- input 和 Mode-local command；
- content changed；
- analysis input 和 apply；
- microtask checkpoint。

业务代码不得继续散布 `Function::call` 和无预算的 microtask checkpoint。
第一步只收敛调用入口并保持行为不变，下一步再加入预算。

### 4.2 执行与输出预算

预算至少覆盖：

- wall-clock deadline；
- isolate heap 上限和超限后的恢复策略；
- TypeScript/module 输入大小；
- staged operation 数量；
- state 和 callback result 的序列化大小；
- decoration 数量；
- module startup 时间。

主 isolate 使用可终止的 handle 和 watchdog。超时后必须清理 terminate 状态；
只有确认 V8 允许继续执行时才复用 isolate。

### 4.3 原子失败

异常、超时、转换失败或超过输出预算时，同时丢弃：

- content/view Mode state draft；
- staged operations；
- staged presentation；
- staged background job；
- 本次 viewport effect。

错误至少包含 Mode、callback、阶段和错误类别。

当前所有脚本共享一个 isolate，因此本阶段不承诺真正的恶意插件隔离。最低
保证是 native 编辑能力和退出流程仍可用。若 isolate 无法安全恢复，则禁用
脚本层并报告错误；只有未来需要自动运行不受信任插件时，才评估 per-plugin
isolate 或进程隔离。

验收测试：

- `while (true) {}` 不会永久冻结编辑器；
- 无限 microtask 不会永久冻结编辑器；
- module startup 超时可以终止；
- 超大 module 或 heap 压力不会终止整个编辑器进程；
- 超时不提交 Mode state、operation 或 presentation；
- 超大 state、result 和 decoration 被拒绝；
- 脚本层故障后仍能保存、退出或继续 native 编辑；
- 正常 callback 的已有行为和顺序不变。

## 5. M2：按真实接缝拆分物理模块

**优先级：** P1

**依赖：** M1 的统一 invocation 边界

**状态：** 已完成（2026-07-21）

**记录：** [`m2_script_modules.md`](m2_script_modules.md)

优先拆分 `script.rs`，因为安全边界已经提供自然接缝。建议起始结构为：

```text
src/app/script/
├── mod.rs
├── host.rs
├── invocation.rs
├── module.rs
├── schema.rs
├── bridge.rs
├── mode_adapter.rs
├── primitives.rs
└── worker.rs
```

目录可以继续按实际职责增长，但不预建空文件。强制边界：

- 只有 invocation 层可以直接执行 JavaScript callback；
- host 层拥有 isolate、context 和 module 生命周期；
- bridge 层负责 Rust 与 V8 值转换；
- schema 层只解析插件定义；
- Mode adapter 不直接管理 isolate 生命周期。

`mode.rs` 只有在类型安全改造或 crate 提取需要稳定接缝时再拆。`kernel.rs`
当前没有仅因文件长度而拆分的必要。移动代码与修改行为必须分开提交。

验收：

- 直接 callback 调用只存在于统一 invocation 层；
- 模块移动提交不修改公共行为；
- `app` 对外 API 和现有测试语义不变；
- 不新增与现有生命周期平行的 Script Mode 系统。

## 6. M3：确定正式名称并完成单 crate 重命名

**优先级：** P1

**依赖：** M1、名称检查通过

**状态：** 已完成（2026-07-21）

**记录：** [`m3_modeleaf_rename.md`](m3_modeleaf_rename.md)

本阶段只做命名迁移，不同时建立 workspace。

名称确认后的目标映射：

```text
显示名称       Modeleaf
仓库名称       modeleaf
Cargo package  modeleaf
Rust crate     modeleaf
二进制命令     modeleaf
环境变量       MODELEAF_*
配置目录       modeleaf
```

迁移原则：

- 新名称成为文档、日志、artifact 和默认路径的唯一主名称；
- 旧环境变量和配置目录只保留一个有截止版本的兼容窗口；
- 首次读取旧路径时输出一次迁移诊断；
- TypeScript 中的 `editor` 是领域对象，不随品牌迁移重命名；
- 仓库托管平台重命名与源码提交分开执行。

验收：

```text
cargo install --path .
modeleaf some_file.rs
```

- 新命令、配置路径和日志名称正常工作；
- 旧配置在兼容窗口内可迁移；
- 除兼容代码、迁移说明和 changelog 外，不再出现旧品牌；
- 该里程碑仍是单 package。

## 7. M4：有门槛地建立 workspace

**优先级：** P1/P2

**依赖：** M2、M3

**状态：** 已完成（2026-07-21）

**记录：** [`m4_workspace.md`](m4_workspace.md)

workspace 不是仅由代码行数触发。满足下列至少一项时再开始提取：

- V8 构建显著拖慢不涉及脚本的核心测试；
- 模块约定不足以阻止已发生的反向依赖；
- 某个边界需要独立 feature、测试或复用；
- CI 需要明确的无 V8 核心构建。

先绘制当前 import graph，再一次提取一个真实 crate。候选顺序为：

1. `modeleaf-protocol`：ID、几何、输入、Scene 和查询 DTO；
2. `modeleaf-core`：Content、Buffer、文本事务和编辑领域逻辑；
3. `modeleaf-frontend`：保持极小，只提供 Frontend 行为接缝；
4. `modeleaf-mode`：只有 command、operation 和 presentation 契约已从 app
   编排细节中解耦后才提取；
5. `modeleaf-app`：Kernel、Session、执行帧和生命周期协调；
6. `modeleaf-plugin-v8`：ScriptHost、module、schema、bridge 和 worker；
7. `modeleaf-tui`：现有 terminal 与 TUI 实现；
8. `modeleaf`：只保留 CLI 和 composition root。

每次提取都必须移动真实代码并独立通过测试，不创建空壳 crate。
`modeleaf-terminal` 暂不单独建立；只有第二个消费者真实复用 terminal
adapter 时再拆。`modeleaf-plugin-api` 也暂缓，直到 schema 需要被非 V8
宿主复用或需要独立版本策略。

目标依赖方向：

```text
                        modeleaf
                    /      |       \
          modeleaf-app  modeleaf-tui  modeleaf-plugin-v8
                |          |          |
                +----- modeleaf-mode ---+
                |          |
          modeleaf-core  modeleaf-frontend
                 \        /
              modeleaf-protocol
```

验收：

- `modeleaf-protocol`、`modeleaf-core` 和无脚本的 app 测试不编译 V8；
- `modeleaf-app` 不依赖 crossterm、Taffy 或 V8；
- `modeleaf-tui` 不依赖 `modeleaf-app`；
- `modeleaf-plugin-v8` 不向其他 crate 暴露 V8 类型；
- 每个提取提交可以独立 revert。

## 8. M5：收紧 Native Mode 类型与状态事务

**优先级：** P2

**依赖：** M0 指标；可以在 M4 前后独立实施

**状态：** 已完成（2026-07-21）

**记录：** [`m5_typed_mode.md`](m5_typed_mode.md)

先为 native Mode 提供带关联类型的构造接口，再在 registry 边界适配到
object-safe `Mode`。业务实现应直接使用自己的 content state、view state
和 job output 类型；`Any` 与 downcast 只留在统一 erased adapter 内。

后台任务的字符串 slot 先收敛为结构化 newtype。只有动态插件边界继续使用
字符串名称。

Mode draft 的优化遵循测量结果：

- 小状态继续完整 Clone；
- 只有 clone 成为可测瓶颈时，才增加 journal、copy-on-write 或结构共享；
- 无语义变化的 draft 不提交，也不增加 revision；
- 缓存类大对象优先移出事务状态或延迟重建。

验收：

- native Mode 业务实现没有裸 downcast；
- 状态类型错配在统一 adapter 处产生领域错误；
- 无变化 callback 不增加 revision；
- 事务失败的回滚语义不变；
- 没有为未测量的状态成本引入通用 draft 框架。

## 9. M6：插件 API 与诊断成熟度

**优先级：** P2

**依赖：** M1

**状态：** 已完成（2026-07-21）

**记录：** [`m6_plugin_api.md`](m6_plugin_api.md)

工作项：

1. 让 Rust schema、`.d.ts`、示例和文档拥有一个可校验的真相源；
2. 为 v1 schema 指定版本化移除条件，不在每条 callback 路径分叉；
3. 为 Mode chain、policy、decoration 和 Face 合成记录来源；
4. 提供只读诊断入口，说明最终值来自哪个 Mode；
5. 为插件作者提供不启动真实 TUI 的 headless 测试入口。

在出现独立宿主或公开版本需求前，真相源可以继续位于现有 crate，不为形式
完整单独建立 `modeleaf-plugin-api`。

验收：

- TypeScript 类型与 Rust parser 的不一致由自动检查发现；
- v1 迁移有明确版本、警告和可执行示例；
- policy 与 Face 冲突可以定位到具体 Mode；
- 插件测试能够覆盖 state、operation、decoration、异常和超时。

## 10. M7：按真实需求扩展 Content

**优先级：** P3

**触发条件：** 出现第三种 Content，或出现插件注册 Content 的真实用例。

第三种内建 Content 出现时，先评估平行枚举是否已经导致漏改。只有实际重复
维护成为问题时，才引入唯一声明源来生成或校验 `Content`、`ContentKind`、
`ContentViewState`、query 和 presentation 映射。

插件 Content 是另一项独立设计。没有至少两个真实插件用例前，不引入
`Box<dyn Content>`、动态 factory registry、property bag 或通用依赖图。

验收：

- 新增 Content 的所有穷尽分支仍由编译器或单一声明源覆盖；
- Content/View state 错配继续返回结构化错误；
- 没有为了理论开放性牺牲当前封闭模型的类型安全。

## 11. M8：发布质量

**优先级：** P2

CI 至少覆盖：

- Windows、Linux 和 macOS；
- `cargo fmt -- --check`；
- `cargo clippy --all-targets --all-features`；
- `cargo test`；
- 严格 TypeScript 类型检查；
- 无 V8 核心构建；
- 完整 V8 构建。

按风险补充 benchmark、property test 或 fuzz：

- 普通输入、native Mode 和 Script Mode 延迟；
- Mode state clone 和大文件 presentation；
- `TextChangeSet`、UTF-16 range 和 operation chain；
- 脚本 schema、module 路径逃逸、超时与取消；
- Content/View identity 和事务失败回滚。

发布前确认：

- 正式名称和 registry 归属；
- 旧配置迁移窗口与移除版本；
- 插件 API compatibility matrix；
- 核心无 V8 构建与完整发行构建均可复现。

## 12. 推荐顺序

```text
M0 baseline and decision gates
 -> M1 main ScriptHost budget and recovery
 -> M2 targeted module split
 -> M3 choose the brand and rename the single crate
 -> M4 incremental workspace extraction
 -> M5 typed native Mode and measured draft optimization
 -> M6 plugin API and diagnostics
 -> M7 Content expansion only when triggered
 -> M8 release quality
```

硬性顺序约束：

- M1 完成前，不把脚本宿主描述为受控执行环境；
- M3 与 M4 不放在同一个 PR；
- M4 每次只提取一个 crate，不顺便重写 Mode API；
- M5 不改变 TypeScript state 的事务语义；
- M7 未满足触发条件时保持延期；
- 所有行为修复先增加能够失败的回归测试。

建议的首批 PR：

```text
PR 01  固定基线并补充主 ScriptHost 超时回归测试
PR 02  收敛 module 与 callback invocation 入口
PR 03  加入 watchdog、terminate 清理和原子失败
PR 04  加入输出预算与脚本层恢复策略
PR 05  按已形成的接缝移动 script 子模块
PR 06  单独完成正式品牌迁移
PR 07  绘制 crate 依赖图并决定首个提取边界
PR 08+ 每次提取一个 crate，保持行为不变
```

每个 PR 必须独立通过相关测试，能够单独 revert，并更新受影响的当前设计文档。
