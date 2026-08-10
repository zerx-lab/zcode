# Sync Upstream

把 `upstream/main`（can1357/oh-my-pi）的更新同步进 fork 的补丁栈分支 `zcode`。

策略真相源是 `docs/fork/sync-strategy.md`，低冲突开发铁律在 `.omp/rules/fork-low-conflict.md`。本文件只管**这次同步怎么走**。

节奏：每周一次。一周 ~950 commits 批量解一次，rerere 命中率高；每天同步只是把同样的冲突拆成七份。

## 1. 跑脚本

```bash
bash brand/sync.sh
```

它按顺序做：安装 `core.hooksPath` → fetch → 漂移审查 → `git rebase upstream/main` → 重建 `release` → `brand/hooks/selftest.sh` → `bun check` → 全绿才推进 `refs/brand/last-sync`。

脚本中途会切到派生分支 `release` 做 overlay，但**退出时（含失败退出）一定把 HEAD 送回 `zcode`**。后面所有步骤都默认你在 `zcode` 上；万一发现自己在 `release`，先 `git checkout zcode` 再动手 —— 落在 `release` 上的提交会被下次 `git branch -f release zcode` 无声丢掉。

脚本能做的到此为止。**下面全是它做不了的判断题**，也是这条命令存在的理由。

## 2. 漂移审查：只挑真的品牌串

脚本会列出 `refs/brand/last-sync..upstream/main` 里新增的 `"omp` / `oh-my-pi` / `can1357` / `.omp/` / `omp://` 命中行。绝大多数是噪声，不要一股脑纳入补丁：

- **忽略**：测试里的临时目录名（`mkTempDir("omp-…")`）、`OMP_*` 环境变量、npm 包名 `@oh-my-pi/*`（刻意不改）、注释里的 `~/.omp` 字样。
- **纳入**：用户可见文案、写盘路径的目录名、新的内部 scheme、新的 `getConfigDirs` 类扫描点。判据是「用户能看见或者数据会落到哪」，不是「字符串里有没有 omp」。

纳入的话，接线点优先找注册表（`InternalUrlRouter.register()`、capability provider、dispatch table），值一律从 `packages/utils/src/brand.ts` 取。

## 3. 解冲突：先读上游改这块的意图

`rerere` 会自动重放已知解法。新冲突逐个看，**先 `git log -p upstream/main -3 -- <冲突文件>` 搞清上游为什么动它**，再决定品牌改动怎么落。

上次的例子：上游把 `token_path(name)` 拆成 `omp_state_dir()` + `token_path()`，品牌补丁原本是 `base.join("zcode").join(name)`，正确解不是保留任一侧，而是顺着上游的新结构收敛成 `base.join("zcode")`。

原则：**跟上游的新结构走，只把品牌值塞回去**。函数名、文档注释保持上游原样，别顺手改名——那是白送的冲突面。

## 4. 报错先做基线差分，别急着改代码

rebase 后 `bun check` / `bun test` 报错时，第一个问题永远是「fork 引入的，还是上游本来就在本机这个平台上挂」。**不要靠读 diff 猜**。

```bash
bash brand/baseline.sh bun test --cwd packages/coding-agent test/discovery/ test/marketplace/
bash brand/baseline.sh bun run check:ts
bash brand/baseline.sh --filter '^error' bun run check:rs
```

脚本在 `upstream/main` 的 worktree 里跑同一条命令，**只比对信号行**（`(fail)` / `error` / `warning:` / `panicked at`），路径与耗时先抹平 —— 基线 worktree 的 `target/` 是冷的，直接比全量输出等于保证误判。每次会打印「比对口径」，看一眼是不是你要的：

- 「两侧一致」→ 上游预存问题，**不阻塞同步**，记进 `docs/fork/CHANGELOG.md` 即可。
- 有差异且 `>` 侧有独有条目 → fork 引入的回归，必须修完再收口。
- 口径显示「全量输出（两侧都没有信号行）」→ 结论不可信，用 `--filter '<正则>'` 指定这条命令的错误行特征后重跑。

上次同步靠这一步认定：`crates/pi-walker` 的 8 个 clippy 错（`cfg(windows)` 分支，上游 Linux CI 照不到）和 coding-agent 的 31 个测试失败，两侧逐行完全一致，全是上游预存，与补丁栈零交集。

worktree 复用在 `../omp-baseline`，`bun install` 只在首次或 lockfile 变了时跑。用完 `bash brand/baseline.sh --clean`。改过过滤规则要用 `brand/baseline-probe.sh` 复验（用法写在文件头）。

## 5. 补丁栈卫生

rebase 完检查补丁面有没有变胖：

```bash
git diff --name-only upstream/main..zcode
```

- 出现**本次没打算改的上游文件** → 多半是误提交，`git checkout upstream/main -- <path>` 撤回，`--fixup` 归位到对应 commit 后 `rebase --autosquash`。
- 有 commit 「patch contents already upstream」被自动 drop → 正常，说明改动被上游合了，补丁栈少一处。
- 构建产物（populated 占位文件、`embedded-addons.*.tar.gz`）→ `pre-commit` 会拦；真拦到了说明上次构建被打断，跑对应的 `gen:*:reset`。

## 6. 收口

1. `bun run src/cli.ts --version` 输出品牌名、`--smoke-test` 通过（在 `packages/coding-agent` 下跑）。
2. `docs/fork/CHANGELOG.md` 的 `[Unreleased]` 下加 `### 同步` 条目：rebase 到哪个 commit、几个上游提交、冲突处怎么解的、drop 了什么、认定为上游预存的失败。
3. `git update-ref refs/brand/last-sync upstream/main`（脚本全绿时已做）。
4. `git branch -f main upstream/main`；`git push --force-with-lease origin zcode`。

**别做**：不要因为旧 blob 还在 GitHub 服务端就跑 `filter-repo`。`--fixup` + `rebase --autosquash` 之后 blob 已经不在任何可达 tree 里（`git rev-list --objects upstream/main..zcode` 验证），残留只在服务端不可达对象（等 GC）和旧 clone，filter-repo 对这两者同样无能为力。
