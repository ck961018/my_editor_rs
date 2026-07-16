# 前缀键与通用 Awaiting 输入设计

> 日期：2026-07-16
> 状态：已确认并实现

## 1. 目标

本设计统一处理三类输入：

- 固定序列，例如 `gg`、`Space f`、`Ctrl-X Ctrl-C`；
- mode 私有的动态等待，例如 Vim `f{char}`、count 和 operator；
- 未来全局 context、其他 native mode 和直接交互式脚本 mode 的等待输入。

输入框架只暴露 `Ready/Awaiting`、`Pass/Consumed/Emit` 等中立状态。Vim 的 count、operator、
字符参数等语法始终保存在 `VimModeState`，App、Dispatcher 和 Buffer 不读取这些状态。

本次不实现脚本 runtime、which-key UI、持久化配置、完整 Vim motion/operator、寄存器、宏和
递归 remap。

## 2. 模块边界

```text
core::keymap
  Keymap<A> / KeyNode<A> / leader 定义期展开

core::input
  InputContext<A> / InputStatus / InputDecision<A>
  InputCoordinator<S> / AwaitingEntry<S>
  多 trie 虚拟叠加、完整绑定查询、timeout 配置

core::mode
  ModeInstance 和 Vim 私有输入状态机

app::view
  ModeInstance 的完整 app 层边界

app::dispatcher
  构造 focused View + global 活动层
  将中立输入结果解析为 DispatchCommand

App::run
  在现有 tokio::select! 中等待最近 input deadline
  逐个执行 action、replay 和 unmapped fallback
```

Content 不再持有静态 keymap。活动固定 keymap 只有 focused View 的当前 mode keymap 和 global
keymap；未匹配输入最终交给 focused View 的 fallback。

## 3. 固定序列 trie

```rust
struct Keymap<A> {
    roots: HashMap<KeyEvent, KeyNode<A>>,
}

struct KeyNode<A> {
    action: Option<A>,
    children: HashMap<KeyEvent, KeyNode<A>>,
}
```

节点可以同时包含 action 和 children。`bind` 只替换目标节点的 action，保留 descendants；
`unbind` 只删除 action，并从下向上裁剪空节点。根节点没有 action，空序列不是合法绑定。

多个活动 keymap 不物理合并，也不维护 revision。匹配时并行遍历各自的 trie：

- 任一层存在 children 就继续等待；
- 完全相同的绑定按 layer 顺序选择，mode 高于 global；
- action 选择当前缓冲中消费按键最多的完整绑定；
- which-key 查询返回当前节点所有 layer children 的并集。

Leader 是定义期 alias，可出现在序列任意位置。构建 keymap 时使用 session 配置的 concrete
`KeyEvent` 展开；运行时 trie 不保留 Leader 字段。修改 Leader 需要重建绑定，本次没有
LocalLeader 或持久化配置。

## 4. Timeout 与 replay

```rust
enum TimeoutPolicy {
    After(Duration),
    Never,
}

struct KeySequenceConfig {
    default_timeout: Duration,
    overrides: HashMap<Vec<KeyEvent>, TimeoutPolicy>,
}
```

所有固定序列使用一个 global default timeout。用户可以给具体 prefix 显式设置新的 Duration
或 Never；当前路径上最近遇到的显式设置由 descendants 继承。每次成功扩展固定序列都重置
idle timeout。

Mismatch 或 timeout 时：

1. 如果存在完整绑定，直接执行消费按键最多的绑定；剩余缓冲键按普通新输入依次处理。
2. 如果不存在完整绑定，缓冲 prefix 作为 unmapped 输入处理，不重新进入固定 keymap；造成
   mismatch 的新键随后按普通输入处理。
3. action 必须先执行，再处理 replay，以便 mode 变化对后续键立即生效。

Escape 在固定序列层没有特殊语义；它可以是合法后缀，也可以触发普通 mismatch。

## 5. 通用 Awaiting

```rust
enum InputStatus {
    Ready,
    Awaiting(TimeoutPolicy),
}

enum InputDecision<A> {
    Pass,
    Consumed,
    Emit(A),
}

trait InputContext<A> {
    fn status(&self) -> InputStatus;
    fn capture(&mut self, key: KeyEvent) -> InputDecision<A>;
    fn on_timeout(&mut self);
    fn cancel(&mut self);
}
```

协调器只在 context 为 Awaiting 时调用 `capture`。`Pass` 传播原按键，不允许隐式转换；若未来
需要转换，应新增明确的 remap 机制。

等待项按激活时间组成 LIFO：

```rust
enum AwaitingEntry<S> {
    Context { source: S, idle_since: Instant },
    KeySequence(PendingSequence),
}
```

固定序列和动态 context 使用同一激活顺序，但各自拥有所需状态。`Pass` 不重置 `idle_since`；
`Consumed`、`Emit` 或执行 action 后仍 Awaiting 时重置。Context timeout 只调用
`on_timeout()` 更新私有状态，不直接产生 action。底层等待项的 timer 不因上层等待项激活而
暂停；同时到期时按 LIFO 处理。

focus 或 mode 生命周期变化会直接丢弃相关固定 pending，调用受影响 View 的 `cancel()` 并移除
其 Awaiting 项，不 replay。全局、与 focus 无关的 context 不被隐式取消。

## 6. 最小 Vim 验证切片

本次只实现足以验证架构的功能：

- `gg`、`{count}gg`：固定序列；
- `f{char}`、`F{char}` 及 count：动态字符参数；
- `h/j/k/l` count；
- `dd`、`{count}dd`、operator 前后 count 相乘。

Vim 私有 Awaiting 全部使用 Never。Escape 和非法输入取消并消费，不 replay。Count 使用
饱和整数运算；执行路径使用饱和加法或有界扫描，避免超大 count panic 或空转。

Mode 解析完成后只输出通用编辑命令：

```rust
MoveToLine { line_index }
MoveToChar { target, direction, occurrence }
DeleteLines { lines }
```

Buffer 不知道 `f`、`F`、`d`、count 或 operator-pending。

## 7. 后续扩展

- which-key UI 直接消费当前多层 trie children 并集；
- persistent config 和热重载在真实配置系统出现时增加；
- 完整 Vim motion/operator 另行设计 range shape，不在当前公共输入协议中预留字段；
- 未来采用可直接调用编辑器 host API 的嵌入式脚本语言，而不是 Wasm 沙箱；脚本 mode 最终与
  builtin mode 使用同一 input/action 接缝。本次仅保证泛型 action 和 opaque context 可承接该
  方向，不实现脚本 runtime 或 registry handle。
