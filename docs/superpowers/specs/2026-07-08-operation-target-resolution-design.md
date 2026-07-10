# Operation 目标解析设计

日期：2026-07-08

## 1. 背景

当前按键处理链路是：

```text
FrontendEvent::Key
-> Dispatcher::dispatch(key, focused, scene, contents)
-> Option<Operation>
-> App::execute_operation(op)
```

`Dispatcher` 查找命令时会沿捕获链查找：

```text
focused space -> parent space -> ... -> global keymap
```

但 `dispatch` 返回的是裸 `Operation`。进入 `execute_operation` 后，App 会重新
用 `focused_content_id()` 推导执行目标。这样会丢失“命令从哪个 keymap 命中”
以及“这个命令应该作用到哪个 view/content/app 范围”的上下文。

当前单 editor 场景大多可用，但后续加入 split、minibuffer、overlay、panel 或
同一 content 的多 view 后，这个模型会让命令查找目标和命令执行目标不一致。

## 2. 目标

- 让 dispatch 阶段负责产出完整的命令解析结果。
- 让 execute 阶段只执行已解析的目标，不再自行盲用 focused content。
- 保持 `Operation` 主要表达动作语义，避免把所有 variant 都改成携带
  `ContentId` 或 `SpaceId`。
- 明确 `SpaceId` 和 `ContentId` 的不同职责：编辑操作需要 view selection，
  因此需要 `SpaceId`；保存操作只需要 `ContentId`。
- 不改变现有 keymap 绑定风格：keymap 仍绑定相对动作，例如
  `Operation::Save` 或 `Operation::InsertText("x")`。

## 3. 非目标

- 不引入动态分发或 `Box<dyn Frontend>`。
- 不改变 `Frontend` trait。
- 不改变事件循环和后台任务管理设计。
- 不重写 keymap 数据结构。
- 不实现新的用户命令或新 UI。
- 不把 `ContentId`/`SpaceId` 直接塞进所有 `Operation` variant。

## 4. 核心模型

新增 dispatcher 层的解析结果：

```rust
pub(crate) struct ResolvedOperation {
    pub operation: Operation,
    pub source: OperationSource,
    pub target: OperationTarget,
}

pub(crate) struct OperationSource {
    pub sid: Option<SpaceId>,
    pub cid: Option<ContentId>,
}

pub(crate) enum OperationTarget {
    App,
    Content(ContentId),
    ViewContent { sid: SpaceId, cid: ContentId },
}
```

`Operation` 仍保留为动作枚举：

```rust
pub enum Operation {
    InsertText(String),
    Save,
    Quit,
    MoveLeftBy(usize),
    ...
}
```

`ResolvedOperation` 是运行时解析产物，不进入 keymap。它属于 app/dispatcher
边界，用来把 keymap 中的相对动作绑定到当前 scene、focused space 和捕获链。

## 5. Source 语义

`OperationSource` 表示命令从哪里命中：

- host keymap 命中：`sid = Some(matched_sid)`，`cid = Some(matched_cid)`。
- global keymap 命中：`sid = None`，`cid = None`。
- default binding 命中：`sid = Some(focused_sid)`，
  `cid = Some(focused_cid)`。

source 主要用于调试、测试和后续扩展。第一版执行逻辑不需要所有操作都读取
source，但必须保留它，避免以后再次丢失捕获链信息。

## 6. Target 语义

`OperationTarget` 表示执行时作用的对象：

- `App`：应用级操作，例如 `Quit`。
- `Content(cid)`：只需要 content 的操作，例如 `Save`。
- `ViewContent { sid, cid }`：同时需要 view selection 和 content 的编辑类操作，
  例如插入、删除、移动、扩展选择、取消选择。

第一版不加入 `Space(SpaceId)`。当前已有操作没有纯 space 目标；如果后续加入
split、close pane 或 layout 命令，再按实际语义补充。

## 7. Target 解析规则

dispatcher 命中 `Operation` 后，按 operation 类型和命中来源解析 target：

1. `Operation::Quit`
   - target = `OperationTarget::App`

2. `Operation::Save`
   - 如果从 host keymap 或 default binding 命中，target =
     `OperationTarget::Content(source_cid)`。
   - 如果从 global keymap 命中，target =
     `OperationTarget::Content(focused_cid)`。
   - 如果当前 focused space 不是 host，则返回 `None` 或保持未命中；第一版不
     默默 fallback 到 `ContentId(0)`。

3. 编辑类操作
   - 包括 `InsertText`、`Delete`、移动、选择扩展、`Cancel`。
   - 如果从 host keymap 或 default binding 命中，target =
     `OperationTarget::ViewContent { sid: source_sid, cid: source_cid }`。
   - 如果从 global keymap 命中，target =
     `OperationTarget::ViewContent { sid: focused_sid, cid: focused_cid }`。

4. 焦点类操作
   - `FocusNext`、`FocusPrev` 暂时 target = `OperationTarget::App`。
   - 当前仍为空实现，但解析语义先固定为 app 级。

5. 未来扩展操作
   - 若操作需要 view selection，使用 `ViewContent`。
   - 若操作只修改 buffer 或文件状态，使用 `Content`。
   - 若操作修改应用或 layout 状态，使用 `App`，未来必要时新增 `Space`。

## 8. Dispatcher 数据流

`Dispatcher::dispatch` 签名从：

```rust
pub fn dispatch(...) -> Option<Operation>
```

改为：

```rust
pub fn dispatch(...) -> Option<ResolvedOperation>
```

捕获链不再只返回 `&Keymap`，而是返回带 source 的条目：

```rust
struct CaptureEntry<'a> {
    keymap: &'a Keymap,
    source: OperationSource,
}
```

查找流程保持不变：

1. pending prefix 优先。
2. idle 时沿 capture chain 查 host keymap，再查 global keymap。
3. 全链未命中时查 focused content 的 `default_binding`。

变化点是：一旦命中 operation，dispatcher 立即根据 source、focused 和 scene
解析 target，并返回 `ResolvedOperation`。

## 9. Pending Prefix 语义

pending prefix 必须保留起始 keymap 的 source。否则第一键来自某个 host keymap，
第二键命中后仍会丢失 owner。

`Dispatcher` 的 pending 状态从：

```rust
pending: Option<Keymap>
```

改为类似：

```rust
pending: Option<PendingKeymap>

struct PendingKeymap {
    keymap: Keymap,
    source: OperationSource,
}
```

pending 子表继续 clone，保持当前简单模型。后续若需要减少 clone，可再改为
共享 keymap 结构；本设计不做这类优化。

## 10. App 执行规则

`App::execute_operation` 从：

```rust
fn execute_operation(&mut self, op: Operation) -> io::Result<()>
```

改为：

```rust
fn execute_operation(&mut self, resolved: ResolvedOperation) -> io::Result<()>
```

执行时按 target 分派：

- `OperationTarget::App`
  - `Quit` 触发 `tasks.cancel()`。
  - `FocusNext`/`FocusPrev` 保持当前空实现。

- `OperationTarget::Content(cid)`
  - `Save` 调用 `spawn_save(cid)`。
  - 非 content-only 操作如果错误落到这里，应在测试中暴露；第一版可以
    `debug_assert!` 或直接忽略。

- `OperationTarget::ViewContent { sid, cid }`
  - 从 `contents[cid]` 取 `ContentHandler`。
  - 从 `views[sid]` 取 selection。
  - 调用 `executor::execute(operation, content, view.selections_mut())`。

`focused_content_id()` 不再作为 execute 的通用 fallback。保留它只作为
dispatcher 解析 global 命令 target 时的辅助函数，且返回 `Option<ContentId>`。

## 11. 错误处理

- 如果 focused space 不是 host，global save 或 global 编辑命令不能解析目标，
  dispatcher 返回 `None`。
- 如果 source 指向的 content 不存在，dispatcher 返回 `None`。
- 如果 execute 阶段发现 `contents[cid]` 或 `views[sid]` 不存在，保持当前内部
  不变量风格，使用 `expect` 暴露 bug。
- `Save` 对非 buffer content 的行为保持现状：`spawn_save` 返回 `false`。

## 12. 测试

新增或调整以下测试：

- dispatcher 返回 `ResolvedOperation`，host keymap 命中时 source/target 指向
  host 的 `sid/cid`。
- global `Quit` 解析为 `OperationTarget::App`。
- global `Save` 解析为 focused content 的 `OperationTarget::Content(cid)`。
- default binding 的 `InsertText` 解析为
  `OperationTarget::ViewContent { focused_sid, focused_cid }`。
- pending prefix 保留第一键命中的 source。
- `execute_operation` 保存指定 `ContentId`，不再重新读取 focused content。
- `execute_operation` 编辑指定 `sid/cid`，使用指定 view 的 selections。

现有 app 行为测试应继续通过：

- 输入字符后退出。
- Ctrl+S 保存当前文件。
- prefix key sequence 保存。
- selection 编辑行为。

## 13. 接受标准

- `Dispatcher::dispatch` 不再返回裸 `Operation`。
- `App::execute_operation` 不再调用 `focused_content_id()` 作为通用执行目标。
- `Operation` variant 不被批量改造成携带 `ContentId`/`SpaceId`。
- pending prefix 命中后仍能知道原始 keymap source。
- 所有现有测试通过，并覆盖 source/target 解析。
