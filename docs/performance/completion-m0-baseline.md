# Completion M0 性能基线

**采集日期：** 2026-08-13

## 1. 环境

- OS：Microsoft Windows NT 10.0.19045.0；
- CPU：x86-64，Intel Family 6 Model 183，24 个逻辑处理器；
- 可用内存：约 31.8 GiB；
- Rust：`rustc 1.97.1 (8bab26f4f 2026-07-14)`；
- Cargo：`cargo 1.97.1 (c980f4866 2026-06-30)`；
- profile：release；
- 电源、温度与其他系统负载未固定，结果用于本机回归，不用于跨机器比较。

环境权限未开放 CPU 商业型号与物理核心数，因此不推测这两个字段。

## 2. 复现命令

```text
cargo test -p vell-app --release m0_performance_baseline --offline \
  -- --ignored --nocapture
cargo test -p vell-tui --release m0_terminal_render_baseline --offline \
  -- --ignored --nocapture
cargo bench -p vell-completion --bench completion --features benchmarking \
  --offline -- --noplot
cargo test -p vell-completion --release \
  hundred_thousand_item_session_stays_below_memory_budget \
  --features benchmarking --offline -- --nocapture
cargo test -p vell-completion --release m0_matcher_latency_distribution \
  --features benchmarking --offline \
  -- --ignored --nocapture
cargo test -p vell-app --release m2_completion_performance_gate \
  --offline -- --ignored --nocapture
```

Criterion 组使用 1 秒 warm-up、2 秒 measurement 和 10 个 sample。其结果用于
优化趋势。预算表另用独立计时的 nearest-rank percentile。M2 端到端门槛
使用 100 个独立 session。

## 3. 既有编辑路径

| 路径 | 结果 |
| --- | ---: |
| unknown key 空闲输入 | p50 0.4 us，p95 0.5 us，p99 0.7 us |
| 普通单字符插入 | p50 6.4 us，p95 9.1 us，p99 12.3 us |
| 一个 native Mode 的输入链 | p50 8.7 us，p95 10.4 us，p99 12.7 us |
| 一个 TypeScript Mode 的输入链 | 平均 147.576 us |
| app 到 no-op Frontend render | p50 0.0 us，p95 0.1 us，p99 0.2 us |
| 50 行 decoration pull | 平均 22.635 us |
| 120 x 42 终端完整 render | 平均 416.552 us |

冷启动为 14.999 ms；5 次 warm startup 平均 7.471 ms。这不是 completion
预算，但保留作为后续 crate 接线的回归参照。

## 4. Completion microbenchmark

Matcher 对所有候选进行 Unicode-aware subsequence scan，并取稳定 top 100。
下表是 Criterion 的 95% confidence interval，不是延迟分布 p95。

| corpus | 1k | 10k | 100k |
| --- | ---: | ---: | ---: |
| ASCII | 90.850-91.620 us | 484.36-486.58 us | 5.390-5.557 ms |
| Unicode | 238.63-240.26 us | 2.425-2.445 ms | 25.252-25.641 ms |
| 长标识符 | 64.794-65.342 us | 781.88-807.52 us | 7.676-8.051 ms |

10k batch conversion 从预建 provider 字符串构造 owned item 和 batch：ASCII
1.481-1.497 ms，Unicode 1.498-1.503 ms，长标识符 1.571-1.586 ms。100k
item 的内存 accounting 扫描为 156.40-160.79 us。

100 个顺序 chunk、每 chunk 1,000 项的完整 streaming 安装为
530.76-535.32 ms。候选分块追加不搬移旧 item；该数仍包含每批全量重筛，留作
M3 增量筛选的对照基线。

独立测试进程用 tracking allocator 测得 100k 长标识符 session 的增量峰值为
28,829,024 bytes，低于 64 MiB。这个数包括 corpus 构造、engine 安装和匹配
临时分配，因而比 session 稳态值更保守。

## 5. 延迟分布与预算解释

| corpus | 1k p95 | 10k p95 | 100k p95 |
| --- | ---: | ---: | ---: |
| ASCII | 62.1 us | 509.8 us | 5.773 ms |
| Unicode | 261.7 us | 2.628 ms | 26.099 ms |
| 长标识符 | 66.4 us | 736.5 us | 8.107 ms |

- ASCII 与长标识符满足 10k 小于 4 ms、100k 小于 16 ms；
- Unicode 10k 满足 4 ms，但 Unicode 100k 未满足 16 ms；
- 100k session 增量峰值满足小于 64 MiB；
- M0 尚无 source 调度与 TUI menu，首批可见候选、输入新增工作和下一 tick
  render 只能在 M1/M2 做端到端测量。

因此 M0 记录的是事实基线，不宣称 M2 所有预算已经通过。Unicode 100k 是
M2 进入最终验收前必须解决的已知性能缺口。

CI 的 `completion-performance` job 对 ASCII、Unicode、长标识符的 10k 绝对
预算和 10k -> 100k 的 15 倍 scaling 设门禁，同时对 ASCII 与长标识符执行
100k 绝对门禁。PR 会先在同一 runner 测 base commit，再限制当前分支 p95 回退
不超过 25%；base 尚未包含 M0 时使用版本化 CSV，允许 100% 的一次性跨机器
差异。M2 的 matcher 快路径把 Unicode 100k p95 从约 26 ms 降到约
10.2 ms，因此现在也执行 16 ms 绝对门禁。优化保留 Unicode 原字符位置；
常见单字符大小写映射预计算 query variant，复杂映射仍走有界 fold cache。

## 6. M2 端到端门槛

native buffer-word source 使用零 debounce。Kernel 对零值不进入 timer，避免
Windows timer quantum 把本地来源人为推迟一帧。本机 release 采样结果：

| 路径 | percentile | 结果 | 预算 |
| --- | ---: | ---: | ---: |
| trigger 同步工作 | p99 | 8.0 us | < 500 us |
| trigger 到首批候选安装 | p95 | 16.9 us | < 8 ms |
| 低唯一度冷 revision 到可见首批 | p95 | 81.5 us | < 8 ms |
| 100k cache-hit retrigger 到可见首批 | p95 | 16.1 us | < 8 ms |
| 100k cache 跨 View 到可见首批 | p95 | 11.8 us | < 8 ms |
| 100k cache 末尾匹配到可见首批 | p95 | 4,981.8 us | < 8 ms |
| 4 KiB query 无匹配 cache poll | max | 154.2 us | < 500 us |
| 100k-word 完整 source lifecycle | total | 38.0 ms | < 100 ms |
| 100k final batch source poll | max | 101.8 us | < 500 us |
| 完整 lifecycle 中输入 fallback | max | 4.9 us | < 500 us |
| populated popup 普通键 fallback | p99 | 0.1 us | < 500 us |
| populated popup selection 导航 | p99 | 2.6 us | < 500 us |
| 100 个 1 MiB 字段 popup render | p99 | 2,235.1 us | < 8 ms |

该门槛从 owned request 创建开始，到 source batch 经过 AppMessage、identity
校验、engine 安装和 presentation 刷新为止。交互门槛另安装 100 个候选，每个
候选有 4,096 个 match position，验证 fallback 使用轻量 session state，导航
只更新 selection identity 而不重建 rows。它不包含下一次终端绘制；TUI 绘制
由独立 release 门禁覆盖。cold revision 数据集以不匹配词开头，在早期放置
匹配词，随后接 100,000 个重复词，并在每轮改动 revision。source 通过 request
携带的 opaque probe 确认 preview 会被中央 matcher 接受，完成扫描后只再发布
一次完整 Replace。独立完整 lifecycle 数据集包含 100,000 个唯一词；它同时
建立 cache，随后验证同 revision retrigger 和共享 Content 的第二个 View 都先
收到可见 preview。另一个 query 让唯一匹配项处于 cache 末尾；4 KiB 无匹配
query 则验证预编译 probe 和分块 yield，使每次 task poll 仍可取消并低于输入
预算。完整扫描固定产生三条 host message；最终 Replace 每构造 1,024 个 item
让出执行权并检查取消，同时测量 source poll 与 message 之间的输入 fallback。
render 门禁让 100 个候选的五个字段共享合法的 1 MiB 零宽字符串，验证绘制按
可见行、terminal cell 宽度和 4 KiB 字段扫描上限短路。
