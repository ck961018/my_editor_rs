# TypeScript Background Analysis Design

**状态：** 已实施

**日期：** 2026-07-21

## 1. 目标

R10 将后台派生计算从普通 TypeScript Mode adapter 的生命周期字段中隔离。
普通 Mode 继续只需要理解 state、viewState、commands、keys、input 和 changed；
需要 worker 的插件通过命名 analysis 能力声明后台计算。

本阶段由内建 Tree-sitter 高亮这一真实用例驱动，不引入没有调用入口的全局
命令表。

## 2. 用户 API

Buffer adapter 可以声明一个或多个命名 analysis：

```ts
analysis: {
  syntax: {
    worker: "worker.ts",
    snapshot: "text",
    input(ctx) {
      return { language: ctx.state.language };
    },
    apply(ctx) {
      return {
        contentDecorations: {
          revision: ctx.revision,
          spans: ctx.arguments.spans,
        },
      };
    },
  },
}
```

字段语义：

- analysis 名称是 Mode 内稳定的后台任务名称；
- `worker` 指向插件资源目录中的 worker module；
- `snapshot: "text"` 要求宿主向 message 添加当前文本快照；
- `input` 是纯函数，捕获深只读 state 和当前 Content 元数据，返回 worker
  message 或 `void`；返回值同时作为该 analysis 的输入签名；
- `apply` 在结果仍属于当前 revision 时运行，可以更新 state 和
  presentation。

v2 不再暴露 `job`、`applyJob`、`slot`、`version` 和 `includeText`。v1 parser
在删除前继续兼容旧字段。

## 3. 宿主不变量

宿主负责以下行为：

1. 以 Mode、Content 和 analysis 名组成稳定 job key；
2. 一次 poll 先计算所有 analysis 的 input，再原子发布全部新签名；
3. 为每个新请求分配单调递增、宿主管理的 generation；
4. 请求同时捕获 Content revision、输入 epoch 和 input message；
5. 同一 analysis 的新 generation 或 `void` 会取消或替换旧任务；
6. revision、epoch 或 message 已过期的结果不会进入 `apply`；
7. `input` 对 state 的意外修改不会发布；
8. `apply` 与其他 Mode callback 共用 Mode draft 和提交语义；
9. callback、message 或 presentation 验证失败时不发布部分 state；
10. 当前 slot 的 `apply` 后 input 被视为已接受，不能自触发循环；
11. 其他 slot 仅在各自 input message 变化时重跑，缓存互不覆盖。

为让完成结果定位到正确 analysis，Mode background apply contract 需要携带
job slot。该 slot 是宿主内部标识，不进入 TypeScript API。

## 4. 独立命令决策

R8 已提供稳定的 Mode-local 限定命令和 `ctx.commands.invoke()`。当前没有命令
面板、外部 RPC 命令调用或不依附 Mode 的键表，因此本阶段不开放
`editor.commands.define`。

未来出现真实独立调用入口时，独立命令必须复用同一 command registry、
Content adapter context、operation queue 和事务帧。不得把它实现成绕过 Mode
runtime 的第二套脚本 action 系统。

## 5. 验收

- Tree-sitter 只使用 `analysis.syntax`，不再使用 v2 raw worker lifecycle；
- 普通 v2 Buffer adapter 类型中不再出现 raw worker lifecycle 字段；
- StatusBar adapter 拒绝 analysis；
- schema 拒绝空 analysis 名、未知 snapshot 和不完整 definition；
- stale result 不调用 apply；
- 只改变 analysis 输入 state 也会替换同 slot 的旧任务；
- 一次 state 变化会在同一 poll 中使所有受影响 slot 失效；
- `apply` 改变自己的输入 state 不会形成后台反馈循环；
- invalid input 不发布 state；
- 多 analysis 的 slot 路由有 Rust 回归测试；
- 内建插件继续通过严格 TypeScript 类型检查和现有高亮集成测试。
