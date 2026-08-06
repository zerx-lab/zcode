---
description: 按序跑本仓库的质量闸门（fmt → clippy → test → doctest → rustdoc → 交叉 lint → 依赖审计 → 索引检查），首个失败即停并修到通过
---

按下面顺序执行质量闸门。**首个失败即停下修**，修完从失败那一步重跑，不要一路跑到底再汇总。

先确认前置条件：仓库根是否存在 `Cargo.toml`。

- **不存在** → 跳过全部 cargo 步骤，只跑索引检查那一步，并在结论里说明"Cargo 项目尚未落盘，仅执行索引检查"。
- **存在** → 从第 1 步开始。

1. `cargo fmt --all`（先修格式，避免后续步骤的输出被格式噪声污染）
2. `cargo check --workspace --all-targets`
3. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
4. `cargo nextest run --workspace`
5. `cargo test --doc --workspace`
6. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`（rustdoc 的
   `private_intra_doc_links` / `redundant_explicit_links` / 歧义链接**只在这一步**暴露，
   clippy 与 test 都看不见）
7. **交叉 lint**：先 `rustup target add <另一平台的 triple>`（幂等；新机器上不装会先报
   "标准库未安装"而不是报告告警），再跑
   `cargo clippy -p zcode-utils --lib --all-features --target <同一 triple> -- -D warnings`。
   Windows 主机用 `x86_64-unknown-linux-gnu`（`cfg(unix)` 分支的告警只有这一步能看到），
   Unix 主机用 `x86_64-pc-windows-msvc`。整个 workspace 换 target 会卡在 ring / sqlite 的
   交叉 C 工具链上，所以只挑有 `cfg` 分叉的 crate 跑。
8. `bun .omp/checks/index-guard.check.ts`
9. `cargo deny check` 与 `cargo machete`（这两条允许在结论里标注为"待安装"，但不要静默跳过）

规则：

- 绝不用 `#[allow(...)]` 抑制 clippy 来让第 3 步变绿，除非同行注释写清了理由；
- 测试失败先判断是**测试错**还是**代码错**，不要改测试去迁就实现；
- 第 8 步的违规按 `rule://agents-index` 处置：路径过时就删条目，超长就搬进 `.omp/rules/`；
- 不提交 commit。

结论只写：每步的通过/失败状态、失败的修法、以及被跳过的步骤及原因。

附加关注点（可选）：$ARGUMENTS
