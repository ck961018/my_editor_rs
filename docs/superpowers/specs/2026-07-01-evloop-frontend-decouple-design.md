# evloop（App）与前端解耦设计 — 借鉴 rsvim

> 本设计把 rsvim 的 evloop 架构与通讯机制应用到 my_editor_rs，核心目标：让 `App`（即 evloop）成为 `tokio::select!` 多路复用主循环，并通过中性 `Frame` 契约 + trait/enum 双重分发做到**完全不感知 tui/gui**。

**日期：** 2026-07-01
**前置：** 2026-06-30 架构重构（Document/View 分离，commit 8d1cd92）已落地。本设计在其基础上演进。

---

## 1. 背景与动机

### 1.1 现状（commit 8d1cd92 之后）

- `App`（`src/app.rs`）已是 evloop 雏形，通过 `Box<dyn Frontend>` 不感知 TUI。`Frontend` trait 把输入（`next_event`）和输出（`render`）都抽象了——这点比 rsvim 更彻底（rsvim 输入端硬绑 crossterm）。
- 但 `App::run` 是朴素 `while !should_quit { next_event().await; handle; render }`，**无 `tokio::select!` 多路复用**，无法接后台异步任务、无法并发响应。
- 渲染契约是 `frontend.render(&dyn ContentLookup, &ResolvedScene, ...)`——前端自己解释 scene + 拉 content + 写 VT，**无中性帧缓冲**契约。
- `Frontend::next_event` 用 `Pin<Box<dyn Future>>` 装箱，因 `Box<dyn Frontend>` 要求 object-safe（Rust 1.75 原生 async fn in trait 不可 dyn-dispatch）。`main.rs` 用 `current_thread` 运行时回避 Send 级联。

### 1.2 rsvim 可借鉴的机制

经分析 `D:\workspace\rsvim`（双 crate workspace，`rsvim_core::evloop`），要点：

1. **`tokio::select!` 多路复用主循环**（`evloop.rs:918-947`）：输入事件 + 多条 mpsc channel + 取消令牌。
2. **trait + enum 双重分发**（`StdoutWritable` trait + `StdoutWriterValue` 枚举，`evloop/writer.rs:26-95`）：零开销、编译期穷尽，优于 `Box<dyn>`。
3. **中性帧缓冲 Canvas**：核心建 `Canvas`，writer 只 `write(&Canvas)`。逻辑层不感知终端。
4. **后台任务 channel 回环**（`evloop.rs:224-239`）：慢任务 `tokio::spawn` 到线程池，完成后经 mpsc 回环唤醒 select!。

### 1.3 rsvim 的短板（本设计规避）

- rsvim **输入端硬绑 crossterm**（`EventStream` + `crossterm::event::Event` 直接进主循环），无输入抽象 trait。my_editor_rs 已有 `Frontend::next_event` + 中性 `FrontendEvent`，本设计保留并强化。
- rsvim 的 `ShaderCommand` 直接包装 `crossterm::Command`，GUI 复用不了——这是抽象泄漏。本设计用**不依赖任何前端**的 `Frame` widget 树契约替代。

### 1.4 已确认的设计选择（brainstorming 阶段）

经逐项澄清，确定：

| 决策点 | 选择 |
|---|---|
| 借鉴范围 | select! 事件循环 + 中性渲染契约(Canvas) + 后台 channel 回环；**不**引入 FSM/Operation（保留 `handle_key`） |
| 渲染契约形状 | 保留 widget 树描述（最 GUI 友好） |
| 分发模型 | trait + enum 双重分发（rsvim 风格，取代 `Box<dyn Frontend>`） |
| 运行时 | multi_thread（rsvim 风格） |
| Frame 构建 | App 调 `build_frame` 纯函数产出中性 `Frame`，前端只 paint |

---

## 2. 架构与模块布局

### 2.1 分层依赖

```
core ──► protocol ──► layout ──► frame ──► app ──► main
                                   │         │
                                   │         └─► tui (painter)
                                   └─────────►
terminal（input/output/lifecycle，被 tui 用）
```

### 2.2 模块职责与变更

| 模块 | 状态 | 职责 |
|---|---|---|
| `core/` | 微调 | buffer/edit/status。`handle_key` 对 Ctrl+S 改为返回 `EditAction::Save` 意图（不再同步落盘），由 App 决定执行方式 |
| `protocol/` | 新增 `frame` 子模块 | 中性契约层。新增 `protocol::frame`：`Frame`/`FrameItem`/`FrameContent`（widget 树类型，不依赖 crossterm）。其余 ids/cursor/viewport/edit_view/frontend_event/key_event/status 不变 |
| `layout/` | 不变 | space/scene/resolved/taffy_engine。Scene/Space/Content 设计保持 |
| `frame/`（新模块） | 新增 | 纯函数 `build_frame(scene, contents, focused) -> Frame`。吸收现 `tui/content.rs` 的「Document→rect 内文本」渲染逻辑，上移为中性 |
| `terminal/` | 不变 | input/output/lifecycle。`Output` 实现 `Canvas` trait 保留 |
| `tui/` | 重写为薄 painter | 删 `tui/content.rs`；`TuiFrontend` 退化为 `render(&Frame)→VT` 纯 painter + `next_event`。不再解释 scene、不再持 Content 注册表 |
| `app/`（拆分为模块） | 重写 | `app/mod.rs`：App select! evloop + channel；`app/frontend.rs`：`Frontend` trait（原生 async fn）+ `FrontendImpl` 枚举；`app/document.rs`：Document + ContentLookup impl |
| `main.rs` | 重写 | `#[tokio::main(flavor="multi_thread")]`，接线 `FrontendImpl::Tui` |

### 2.3 关键不变量

- `app` 只依赖 `Frontend` trait 与 `Frame`，**不 import `tui`/`crossterm`**。
- `tui` 依赖 `protocol::frame` + `terminal`，是 paint target。
- GUI 日后作为 `tui` 同级模块 + `FrontendImpl::Gui` 变体加入，`app` 零改动。

---

## 3. 前端契约（trait + enum + Frame）

### 3.1 `Frontend` trait（原生 async fn，无装箱）

```rust
// app/frontend.rs
pub trait Frontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>>;
    fn render(&mut self, frame: &Frame) -> io::Result<()>;
}
```

因 `App` 持具体枚举 `FrontendImpl`（非 `Box<dyn>`），原生 `async fn in trait` 直接可用，返回具体 future 类型——**彻底移除现有 `Pin<Box<dyn Future>>` 装箱**。

### 3.2 `FrontendImpl` 枚举（trait+enum 双重分发）

```rust
// app/frontend.rs
pub enum FrontendImpl {
    Tui(TuiFrontend<io::Stdout>),
    Headless(HeadlessFrontend),   // 测试 + 未来 headless 模式
    // Gui(...)  日后追加，app 零改动
}

impl Frontend for FrontendImpl {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        match self {
            Self::Tui(f) => f.next_event().await,
            Self::Headless(f) => f.next_event().await,
        }
    }
    fn render(&mut self, frame: &Frame) -> io::Result<()> {
        match self {
            Self::Tui(f) => f.render(frame),
            Self::Headless(f) => f.render(frame),
        }
    }
}
```

`App` 字段类型为 `frontend: FrontendImpl`，**只调 trait 方法、从不 match 变体**——这是「evloop 不感知 tui/gui」的落实点。

### 3.3 `Frame`（中性 widget 树契约）

```rust
// protocol/frame.rs
pub struct Frame {
    pub items: Vec<FrameItem>,              // 按 (layer, z_index, order) 排序后
    pub focused_content: ContentId,         // 焦点 item 的 content_id
    pub focused_cursor: Option<CursorPos>,  // 焦点光标（buffer 坐标，屏坐标由前端按焦点 item 的 rect + state.viewport 算）
}
pub struct FrameItem {
    pub content_id: ContentId,
    pub rect: Rect,
    pub state: SpaceState,
    pub content: FrameContent,
}
pub enum FrameContent {
    Editor { lines: Vec<String> },   // 已折行/裁剪到 rect 的可见行
    StatusBar { file_name: Option<String>, modified: bool, message: StatusMessage },
}
```

- `Frame` 是纯数据，**不依赖 crossterm/任何前端**，且派生 `Clone`（`HeadlessFrontend` 捕获帧需 clone）。
- 前端定位光标：用 `focused_content` 找到对应 `FrameItem`，取其 `rect` + `state.viewport` + `focused_cursor` 计算屏坐标。
- `build_frame`（`frame/` 模块）消费 `ResolvedScene` + `ContentLookup`，主动拉取文本并解析成 `FrameContent`——把「Document 怎么变成 rect 内可见行」的判断从 `tui/content.rs` 上移至此。
- `TuiFrontend::render(&Frame)`：遍历 `items`，按 `rect` 把 `FrameContent` 写成 VT；用 `focused_cursor` + 焦点 item 的 rect 定位光标。不再持 Content 注册表、不再查 ContentLookup、不再解释 scene。
- `HeadlessFrontend`：吃脚本事件 + 捕获 `Vec<Frame>` 供测试断言。

---

## 4. App 作为 select! 事件循环 + 后台 channel

### 4.1 channel 拓扑

```rust
// app/mod.rs
enum BgResult {
    SaveResult(ContentId, io::Result<()>),
}

pub struct App {
    contents: HashMap<ContentId, Document>,
    editor_content: ContentId,
    scene: EditorScene,
    engine: TaffyEngine,
    focused: SpaceId,
    should_quit: bool,
    frontend: FrontendImpl,
    bg_tx: mpsc::Sender<BgResult>,
    bg_rx: mpsc::Receiver<BgResult>,
    pending_save: Option<ContentId>,   // 在途保存，结果回来时更新 status
}
```

- `bg_rx` 是 select! 的一个分支。后台任务 clone `bg_tx` 进闭包，`tokio::spawn` 到 worker 线程做 IO，完成后发 `BgResult` 回主循环。
- 后台任务**只拿 owned 数据**（`PathBuf` + 文本快照），不碰 App 状态——所以**不需要 rsvim 那套 `Arc<Mutex>` 共享**。这是比 rsvim 更轻的地方。

### 4.2 主循环

```rust
pub async fn run(&mut self) -> io::Result<()> {
    self.render()?;
    loop {
        tokio::select! {
            ev = self.frontend.next_event() => {
                if let Some(e) = ev? { self.handle_event(e).await?; }
            }
            res = self.bg_rx.recv() => {
                if let Some(r) = res { self.handle_bg_result(r)?; }
            }
        }
        if self.should_quit { break; }
        self.render()?;
    }
    Ok(())
}
```

- `next_event` 借 `&mut self.frontend`，`bg_rx.recv()` 借 `&mut self.bg_rx`——不相交字段，select! 无借用冲突。
- 退出靠 `should_quit`（handle_event 置位），循环顶检查后 break。不引入 `CancellationToken`（rsvim 有，但当前无后台任务需取消的语义；保存任务即发即弃，退出时直接丢）。

### 4.3 具体落地：异步保存

现状 `handle_key` 对 Ctrl+S 同步调 `buffer.save()`。改为：

1. `core::edit` 的 `handle_key` 对 Ctrl+S 返回 `EditAction::Save`（新增变体），**不再直接落盘**。
2. `App::handle_event` 收到 `EditAction::Save` → 调 `self.spawn_save(content_id)`：

```rust
fn spawn_save(&mut self, id: ContentId) {
    let path = self.contents[&id].buffer.path().map(|p| p.to_path_buf());
    let bytes = self.contents[&id].buffer.slice().to_string();  // owned 快照
    let tx = self.bg_tx.clone();
    self.pending_save = Some(id);
    tokio::spawn(async move {
        let res = match path {
            Some(p) => tokio::fs::write(p, bytes).await.map_err(Into::into),
            None => Err(io::Error::other("no path")),
        };
        let _ = tx.send(BgResult::SaveResult(id, res)).await;
    });
}
```

3. `handle_bg_result(SaveResult(id, res))` → 清 `pending_save`，按 `res` 设 `doc.status`（Saved / SaveFailed）。

保存不再阻塞主循环——select! 在保存期间仍能响应输入/其它事件。channel 机制被真正用上。

### 4.4 `pending_save` 语义

同一时刻只允许一个在途保存（`Option<ContentId>`）。保存完成前再发 Ctrl+S：**忽略**（`pending_save.is_some()` 时 `spawn_save` 直接 return，不重复发起）。这避免并发写同一文件的竞态。多文档并发保存日后需要时再放宽为 `HashSet`，当前 YAGNI。

---

## 5. 错误处理

| 层 | 错误来源 | 策略 |
|---|---|---|
| 后台 IO | `tokio::fs::write` 失败 | 不 panic，走 `BgResult::SaveResult(_, Err)` 回主循环，转 `Status::SaveFailed` 显示，主循环继续 |
| `next_event` | crossterm 读失败 | `io::Error` 上抛 `run()`，由 main 打印 + 恢复终端后退出 |
| `render` | 写 stdout 失败 | 同上上抛（终端断了无意义继续） |
| `build_frame` | 纯函数，只做索引/切片 | 用 `saturating_sub`/边界检查，不返回 Result；越界不可能（layout 已裁剪） |
| 退出 | 主循环 break | 不等在途保存（即发即弃）；若需「退出前刷盘」语义，日后加 `blocked_tracker`（rsvim 模式），当前 YAGNI |

`run()` 仍是 `async fn run(&mut self) -> io::Result<()>`，错误传播路径与现状一致。

---

## 6. multi_thread + Send 风险与对策

切到 `multi_thread` 后，`App::run` future 必须 `Send`。逐项评估：

1. **`App` 字段跨 await 点**：`contents`/`scene`/`engine` 都 `Send`（ropey `Rope` Send、taffy 类型 Send）。✅
2. **`FrontendImpl` 必须 Send** → `TuiFrontend` 必须 Send → 其字段 `Output<W>`（`W: Send`，`io::Stdout` Send ✅）+ `Input`。**风险点：`Input`（crossterm `EventStream`）及其 `next()` future 在 Windows 的 Send 性**。crossterm 0.28 `EventStream` 本体应 Send（内部 `tokio::sync::Notify` + 独立读线程），但 `next()` future 的 Send 性需**实测编译验证**——这是计划的第一个技术验证点。
   - **退路**：若不 Send，保存用 `spawn_blocking` 读 stdin 的旧同步路径包一层，或回退 `Box<dyn Frontend + Send>` + 装箱 future `+ Send`（部分回退现状）。
3. **`async fn next_event` 的 future**：`FrontendImpl` 是具体枚举（非 dyn），future 是具体类型，Send 性由各变体 body 决定，不需显式 `+ Send` bound。Tui 变体 Send 性取决于第 2 点。✅（条件性）
4. **后台任务**：`tokio::spawn` 要求 `Send + 'static`。保存闭包持 `PathBuf`+`String`+`Sender`，全 Send ✅。
5. **`bg_rx.recv()` future**：`mpsc::Receiver` Send，recv future Send ✅。

**结论**：multi_thread 可行，**唯一硬风险是 crossterm `EventStream` future 在 Windows 的 Send 性**。计划中以「先写最小 select! 主循环 + 真实 TuiFrontend，`cargo check` 验证 Send」作为早期关卡；若失败按退路处理。

---

## 7. 测试策略

三层测试，对应三个可测边界：

**1. `build_frame` 单元测试（`frame/`，纯函数）**
- 不需前端/运行时。构造 `ResolvedScene` + `ContentLookup` stub，断言 `Frame`：Editor 内容已折行裁剪、StatusBar 字段正确、`focused_cursor` 正确。
- 改断言 `Frame` 数据而非 VT 字符串——更稳、不绑终端转义。

**2. `TuiFrontend::render(&Frame)` 单元测试（薄 painter）**
- `Output::new(Vec<u8>)` 捕获 VT。喂预构造 `Frame`，断言输出含正确文本、光标定位转义（`\x1b[r;cH`）、`show_cursor`。
- 只测「Frame→VT 映射」，与「Document→Frame」解耦。

**3. `App` 集成测试（`HeadlessFrontend` 驱动 select!）**
- `HeadlessFrontend`：脚本事件队列 + 捕获 `Vec<Frame>`。
- `multi_thread` runtime 跑 `App::run`，断言：
  - 插入字符 → 最后一帧 `FrameContent::Editor` 含该字符。
  - Ctrl+S → 触发 `spawn_save`，`bg_rx` 收 `SaveResult`，status 转 Saved（`tempfile` 真实读写验证落盘）。
  - 保存完成前再发 Ctrl+S → 忽略（`pending_save` 语义）。
  - Resize → 帧 scene 尺寸变化。
  - Quit → `run` 返回。

**测试覆盖映射：**

| 设计决策 | 被哪层测试覆盖 |
|---|---|
| App 不感知前端 | 层 3 用 `HeadlessFrontend` 跑通 → 证明前端可换 |
| Frame 中性契约 | 层 1+3 断言 `Frame` 数据，不碰 VT |
| select! 多路复用 | 层 3 保存测试：保存 future 在途时主循环仍推进 |
| trait+enum 分发 | 层 3 切 `Headless`/`Tui` 同一 App 跑 |
| async fn 无装箱 | 编译即验证（层 3 multi_thread 编译过 = Send 满足） |
| crossterm Send 风险 | 层 3 用真实 `TuiFrontend` + multi_thread 编译过 = 风险解除 |

**现有 61 个测试的迁移**：`app.rs` 的 4 个集成测试改用 `HeadlessFrontend` + 断言 `Frame`；`tui/content.rs` 渲染测试拆入层 1（`build_frame`）和层 2（painter）。总数持平或略增。

---

## 8. 不在本设计范围（YAGNI）

- **FSM/Operation 按键层**：保留现有 `handle_key` 单函数，不引入 rsvim 的多 mode 状态机。
- **JS 运行时 / ex 命令 / 语法高亮**：rsvim 有，本设计不涉及。
- **`CancellationToken` / `TaskTracker`**：当前无后台任务需取消语义，退出即发即弃。
- **`Arc<Mutex>` 共享状态**：后台任务只拿 owned 数据，不需要。
- **GUI 前端实现**：只保证契约就绪（`Frame` 中性 + `FrontendImpl` 可扩展），不实现 Gui 变体。
- **「退出前刷盘」语义**：日后需要再加 `blocked_tracker`。

---

## 9. 与现有记忆的关系

现有项目记忆 `frontend-boxed-future-runtime` 记录：「Frontend trait 用 boxed future 非 async fn；main 用 current_thread runtime——切多线程需重审 Send 约束」。

本设计**取代**该记忆的核心结论：
- 改用 trait+enum 分发后，`App` 持具体 `FrontendImpl`（非 `Box<dyn>`），`Frontend::next_event` 用原生 `async fn`，**移除装箱 future**。
- 切到 `multi_thread` runtime，Send 约束经第 6 节逐项评估可行（唯一风险 crossterm EventStream Send 性，早期编译验证）。

实现合并后需更新该记忆文件。
