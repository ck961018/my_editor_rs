# 运行时 Mode 标识设计

**日期：** 2026-07-13
**状态：** 已实施

## 目标

- 命令、Content 默认值和未来脚本边界使用拥有所有权的 `ModeName` 与
  `ModeActionName`，不再要求名称具有 `'static` 生命周期。
- `ModeRegistry` 注册定义时分配数值型 `ModeId`，并在每个 Mode 内为 action 分配
  `ModeActionId`；同一个 registry 生命周期内，名称解析结果保持稳定。
- `ModeInstance` 保存已解析的运行时 ID，执行命令时校验目标 Mode 并解析 action 名称。
- 原生 Mode 继续使用 `Any` 状态，不把实现对象泄露到命令或协议边界。

## 名称与运行时 ID

```text
Content / keymap / future script or protocol
        ModeName + ModeActionName (owned)
                         |
                         v
                    ModeRegistry
                         |
                         v
             ModeId + ModeActionId (Copy)
                         |
                         v
                    ModeInstance
```

名称是边界数据，使用 `String` 保存；运行时 ID 是 registry 内的紧凑身份。Mode ID
按注册顺序单调分配且不复用。Action ID 在所属 Mode 内按声明顺序分配，必须与 Mode ID
共同解释，不作为全局身份。

`ContentCommand::Mode` 继续携带名称，因此未来远程或脚本调用不依赖某个进程内的数值 ID。
当前 App 不新增协议序列化层。

## Mode 定义

`Mode` 定义声明自己的 owned 名称和 action 名称列表。Registry 拒绝重复的 Mode 名称及
同一 Mode 内重复的 action 名称。注册后，`ModeInstance` 使用解析后的 action ID 取得
canonical action 名称，再调用原生定义；未知 action 返回未产生命令，不修改 Mode 状态。

原生 Mode 的 `ModeState: Any` 保持不变。未来脚本 adapter 可以把 opaque handle 放进自己
的原生状态对象，但本项不定义 runtime、handle 生命周期或脚本调用 ABI。

## 非目标

- 不选择脚本语言，不实现沙箱、热重载、脚本加载或 ABI。
- 不把运行时数值 ID 放进跨进程协议或持久化配置。
- 不增加 Mode stack、capability 查询或动态卸载。

## 验收

- 运行时构造的 `String` 可用于 Mode 和 action 名称。
- Registry 对同一名称稳定返回同一个运行时 ID，并拒绝重复注册名称。
- 现有 Vim 命令、View 独立状态和渲染行为保持不变。
- fmt、test、clippy 与 diff 检查通过。
