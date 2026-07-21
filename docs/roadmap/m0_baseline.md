# M0 基线与名称检查

**状态：** 已完成

**日期：** 2026-07-21

## 1. 环境

```text
OS       Windows NT 10.0.26200.0
host     x86_64-pc-windows-msvc
rustc    1.96.1 (31fca3adb 2026-06-26)
cargo    1.96.1 (356927216 2026-06-26)
pnpm     11.9.0
tsc      5.9.3
profile  Cargo test profile, unoptimized + debuginfo
```

这些数字用于同一机器、同一命令和同一 profile 下的前后对比，不代表发布
构建性能。

## 2. 可重复命令

```text
cargo test m0_performance_baseline -- --ignored --nocapture --test-threads=1
cargo test
cargo clippy --all-targets --all-features
cargo fmt -- --check
pnpm typecheck
git diff --check
```

`m0_performance_baseline` 是 ignored test，不进入普通测试耗时。它复用现有
`ScriptedFrontend`、真实 Mode execution frame 和 presentation cache，未新增
benchmark 依赖。

Mode state clone 指标只在 `cfg(test)` 下收集，因此生产路径没有计数或计时
开销。`inline_bytes` 是 `size_of_val` 得到的内联大小下界，不包含容器引用的
堆内存。`clone time` 包含时钟读取和 allocation 开销，只适合用相同命令做
相对比较。

## 3. 2026-07-21 测量结果

```text
cold model startup
  iterations     1
  total          24,310 us
  per iteration  24,310.000 us

warm model startup
  iterations     5
  total          86,797 us
  per iteration  17,359.480 us

native input
  iterations     500
  total          61,535 us
  per iteration  123.070 us
  state clones   3,000
  clone time     78,400 ns
  inline bytes   0

script input
  iterations     500
  total          120,842 us
  per iteration  241.686 us
  state clones   3,000
  clone time     307,300 ns
  inline bytes   480,000

large document visible decorations
  document rows       10,001
  cached decorations  10,001
  visible rows        50
  iterations          100
  total               4,512 us
  per iteration       45.128 us
```

当前样本中，Mode state clone 不是主要输入耗时来源。M5 仍应保留测量入口，
但没有依据在 M1 前重写 draft 策略。

## 4. 验证结果

```text
cargo test
  470 passed; 0 failed; 1 ignored

cargo clippy --all-targets --all-features
  passed

cargo fmt -- --check
  passed

pnpm typecheck
  passed
```

项目此前依赖全局 `tsc.cmd`，不能在当前环境复现。M0 增加了最小
`package.json`、锁定的 TypeScript 开发依赖和 `pnpm typecheck`，使该检查
成为仓库内定义的命令。

## 5. Eido 名称检查

截至 2026-07-21 的初步冲突检查结果：

- crates.io 没有精确名为 `eido` 的 crate；
- npm 已存在精确名为 [`eido`][npm-eido] 的 package，当前版本为
  `4.66920160.1`；
- GitHub 的 [`eido`][github-eido] 用户命名空间已被占用；
- 美国存在仍有效的 [`EIDO` 软件服务商标][uspto-eido]，注册号 6804899；
- 另有多家正在使用 Eido/Eidō 名称的软件企业和产品。

结论：`Eido` 不通过项目正式名称门槛。即使 crates.io 名称暂时可用，npm、
托管平台命名和软件商标冲突已经足以造成长期识别与发布风险。M3 必须重新
选择候选名称并重复相同检查，当前 package 继续使用 `my_editor_rs`。

本节只是工程命名冲突筛查，不构成法律意见。

[github-eido]: https://github.com/eido
[npm-eido]: https://www.npmjs.com/package/eido
[uspto-eido]: https://tsdr.uspto.gov/#caseNumber=90495304
