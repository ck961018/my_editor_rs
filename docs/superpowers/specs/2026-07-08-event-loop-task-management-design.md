# 事件循环任务管理设计

日期：2026-07-08

## 1. 背景

当前 `App<F: Frontend>` 已经是应用主循环，并通过 `tokio::select!`
同时等待前端事件和后台保存结果：

```text
frontend.next_event()
bg_rx.recv()
```

这个模型对当前功能可用，但后台任务生命周期仍然偏临时：

- `BgResult` 只服务保存，命名不能承接后续语法解析、搜索、文件 watcher
  等内部消息。
- `pending_save: Option<ContentId>` 限制全局只有一个在途保存，和
  `ContentId` 建模不一致。
- 退出依赖 `should_quit`，退出前只按 `pending_save` 额外等待一次
  `bg_rx.recv()`，没有统一取消信号和任务分级。
- 后续如果加入可丢弃后台任务，现有结构无法表达“退出时等待保存，但不等待
  语法解析/搜索预计算”。

rsvim 的事件循环有更完整的任务管理思路：主循环统一接收输入、内部消息和
取消信号，后台任务通过消息回主循环，退出时区分可取消任务和必须等待任务。
本设计只吸收这些通用事件循环能力，不引入 JS runtime、脚本任务或 rsvim 的
前端绑定方式。

## 2. 目标

- 保留 `App<F: Frontend>` 静态分发和独立 `frontend` 层。
- 将 `BgResult` 升级为可扩展的内部消息 `AppMessage`。
- 将后台结果通道改为 app 内部消息总线。
- 引入统一取消信号，替代 `should_quit`。
- 引入任务分级：可丢弃任务和退出必须等待任务。
- 当前只把保存任务接入新任务管理骨架，不新增用户可见功能。
- 保存退出语义更明确：退出时必须等待在途保存完成并处理保存结果。

## 3. 非目标

- 不引入 JS runtime、脚本系统、定时器 API 或 import/fs/proc 请求协议。
- 不恢复 `FrontendImpl`、全局 `HeadlessFrontend` 或 dyn frontend。
- 不修改 `Frontend` trait。
- 不修改 `tui`、`terminal`、keymap、dispatcher 或 operation 语义。
- 不把 FSM/dispatcher 大规模消息化。
- 不实现语法解析、搜索、文件 watcher 等新后台任务。
- 不在退出等待期间做最终 render。

## 4. 模块结构

新增两个小模块，避免继续膨胀 `src/app/mod.rs`：

```text
src/app/
  mod.rs       App 主循环、事件处理、业务操作、渲染接线
  message.rs   AppMessage 定义
  tasks.rs     取消令牌、任务 tracker、关闭/等待逻辑
```

`src/app/mod.rs` 继续拥有 `App<F: Frontend>`，但内部字段从保存专用状态改为
通用消息和任务管理：

```rust
message_tx: mpsc::UnboundedSender<AppMessage>,
message_rx: mpsc::UnboundedReceiver<AppMessage>,
tasks: AppTasks,
pending_saves: HashSet<ContentId>,
```

`AppTasks` 封装：

```rust
cancel: CancellationToken,
detached_tasks: TaskTracker,
critical_tasks: TaskTracker,
```

其中 `CancellationToken` 和 `TaskTracker` 来自 `tokio-util`。

## 5. 消息模型

`BgResult` 删除，替换为 `AppMessage`：

```rust
pub(crate) enum AppMessage {
    SaveCompleted {
        content: ContentId,
        result: io::Result<()>,
    },
}
```

消息通道使用 unbounded channel：

```rust
let (message_tx, message_rx) = mpsc::unbounded_channel::<AppMessage>();
```

选择 unbounded 的原因：

- 后台任务完成后不能因为 bounded channel 已满而挂住。
- App 内部消息量当前很小，只有保存完成结果。
- 后续如果某类任务可能产生高频消息，应在该任务自身做合并或节流，而不是让
  完成通知阻塞。

`AppMessage` 目前只包含 `SaveCompleted`。不提前加入
`SyntaxCompleted`、`SearchCompleted` 等未实现消息。

## 6. 任务分类

任务分两类：

1. `critical_tasks`

   退出时必须等待的任务。当前只有保存文件。保存任务如果被静默丢弃，可能
   损坏用户对数据是否落盘的判断，因此必须等待完成。

2. `detached_tasks`

   退出时可丢弃的任务。当前不接具体任务，只作为后续语法解析、搜索预计算、
   文件 watcher 等能力的承载点。

退出时：

- cancel token 进入 cancelled 状态。
- 关闭 `detached_tasks` 和 `critical_tasks`，不再接受新任务。
- 不等待 `detached_tasks`。
- 等待 `critical_tasks.wait().await`。
- drain 内部消息队列中已经完成的保存结果。

## 7. 保存流程

`pending_save: Option<ContentId>` 改为：

```rust
pending_saves: HashSet<ContentId>
```

保存按 `ContentId` 去重：

```text
Operation::Save
  -> spawn_save(content)
  -> pending_saves 已包含 content 时忽略
  -> 从 buffer 抽取 path 和 bytes
  -> pending_saves.insert(content)
  -> critical_tasks.spawn(async move {
       let result = tokio::fs::write(path, bytes).await;
       let _ = message_tx.send(AppMessage::SaveCompleted {
           content,
           result,
       });
     })
```

保存任务只持有 path、bytes、content id 和 `message_tx`，不持有 `App`、
buffer、scene、frontend 或 renderer。

保存完成消息在主循环处理：

```text
message_rx.recv()
  -> handle_app_message(AppMessage::SaveCompleted)
  -> pending_saves.remove(content)
  -> result Ok:
       buffer.mark_saved()
       buffer.set_status(StatusMessage::Saved)
     result Err:
       buffer.set_status(StatusMessage::SaveFailed)
```

如果 `SaveCompleted` 指向不存在或非 buffer content，继续保持当前内部
invariant，用 `expect("saved buffer exists")` 暴露错误，而不是吞掉。

## 8. 主循环

`App::run()` 变为三路 select：

```rust
pub async fn run(&mut self) -> io::Result<()> {
    self.render()?;

    loop {
        tokio::select! {
            ev = self.frontend.next_event() => {
                match ev? {
                    Some(event) => self.handle_event(event).await?,
                    None => self.tasks.cancel(),
                }
            }

            message = self.message_rx.recv() => {
                if let Some(message) = message {
                    self.handle_app_message(message)?;
                } else {
                    self.tasks.cancel();
                }
            }

            _ = self.tasks.cancelled() => {
                self.shutdown_tasks().await?;
                break;
            }
        }

        if !self.tasks.is_cancelled() {
            self.render()?;
        }
    }

    Ok(())
}
```

`FrontendEvent::QuitRequest` 和 `Operation::Quit` 都调用 `self.tasks.cancel()`。
`should_quit` 删除，退出意图只由 cancel token 表达。

普通事件或内部消息处理后继续 render。收到取消信号后进入 shutdown，不做最终
render。

## 9. 关闭流程

关闭流程集中在 `shutdown_tasks()`：

```text
shutdown_tasks()
  -> tasks.cancel()
  -> tasks.close_detached()
  -> tasks.close_critical()
  -> tasks.wait_critical().await
  -> drain message_rx.try_recv()
  -> 对每个 AppMessage 调 handle_app_message()
```

等待 critical tasks 之后再 drain 消息，确保保存任务完成后发出的
`SaveCompleted` 能落地。

不等待 detached tasks。detached tasks 后续实现时必须遵守一个约束：退出后
即使它们尝试发送 `AppMessage`，发送失败也不能 panic。

## 10. 错误处理

- `frontend.next_event()` 返回 `Err`：继续上抛 `io::Error`。
- `tokio::fs::write` 返回 `Err`：通过 `SaveCompleted` 交给主循环，状态设为
  `SaveFailed`。
- `message_tx.send(...)` 失败：后台任务忽略，因为 App 已经退出或消息通道已
  关闭。
- `message_rx.recv()` 返回 `None`：触发 cancel。
- `SaveCompleted` 指向错误 content：保持 `expect`，暴露内部 invariant 破坏。

## 11. 测试策略

继续使用 `src/app/mod.rs` 测试模块内的局部 `ScriptedFrontend`，不恢复全局
headless 前端。

App 集成测试：

- 普通输入路径仍通过 `run()` 修改 buffer 并退出。
- `Ctrl+S` 保存成功后经 `AppMessage::SaveCompleted` 设置 `Saved`。
- 同一 `ContentId` 在 `pending_saves` 中时，再次保存被忽略。
- 退出时如果保存任务仍在路上，`run()` 等待 critical task 完成再返回。

消息处理测试：

- `SaveCompleted Ok` 会移除 pending、`mark_saved()`、设置 `Saved`。
- `SaveCompleted Err` 会移除 pending、设置 `SaveFailed`。

任务生命周期测试：

- `shutdown_tasks()` 会等待 critical task。
- detached task 不阻塞退出，可用 `tokio::time::timeout` 验证。

## 12. 实施边界

预期改动文件：

- `Cargo.toml`
- `Cargo.lock`
- `src/app/mod.rs`
- `src/app/message.rs`
- `src/app/tasks.rs`

不改动：

- `src/frontend/mod.rs`
- `src/tui/**`
- `src/terminal/**`
- `src/protocol/key_event.rs`
- `src/app/dispatcher.rs`
- `src/app/executor.rs`

## 13. 验收标准

- `rg "BgResult|bg_tx|bg_rx|pending_save|should_quit" src/app` 无当前实现残留。
- `rg "FrontendImpl|HeadlessFrontend|Box<dyn Frontend>" src` 无回退。
- 保存成功和保存失败状态行为与当前一致。
- 重复保存同一 content 不会并发写同一路径。
- 退出时在途保存完成后 `run()` 才返回。
- detached task 不阻塞退出。
- `cargo test` 通过。
