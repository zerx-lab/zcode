# Fork 变更记录

上游 `packages/*/CHANGELOG.md` 属上游所有，fork 侧改动只记在这里。

## [Unreleased]

### Added

- zcode 品牌字标：终端块字符版 `packages/coding-agent/src/modes/components/brand-logo.ts`（`ZCODE_LOGO`，12×5，仅用 `█ ▀ ▄` 以保证 setup splash 2x 放大不发虚）；SVG 版 `brand/logo/zcode-mark.svg`（品牌渐变）与 `brand/logo/zcode-mark-mono.svg`（深底单色）。
- `brand/hooks/pre-commit`：提交门禁，拦截被中断的 binary build 留下的 populated 构建占位文件（`embedded-addon.js` / `mupdf-wasm-embed.ts` / `embedded-client.generated.txt`）与 `embedded-addons.*.tar.gz`。判据是带分号的 `with { type: "file" };` —— mupdf 占位文件的注释里含不带分号的同一串，少分号会把纯占位形态误判成 populated 且无法通过 reset 解除。`brand/hooks/selftest.sh` 是双向探针（4 拒 + 4 放行），已接入 `brand/sync.sh` 门禁。`brand/sync.sh` 幂等安装 `core.hooksPath`；cherry-pick / rebase / 有冲突后的 `rebase --continue` 均实测不触发，不干扰上游同步。
- `packages/natives/native/.gitignore`（新文件，零冲突）：补上上游遗漏的 `embedded-addons.*.tar.gz` ignore 规则。
- `brand/baseline.sh`：基线差分。在 `upstream/main` 的 worktree（`../omp-baseline`，复用）里跑同一条命令并比对失败集，用来判定 rebase 后的报错是 fork 引入还是上游在本机平台的预存问题。内建两个必踩的坑：worktree 要 `bun install`（只在首次/lockfile 变更时跑），以及要拷 `packages/natives/native/*.node`（缺了会让每个测试文件秒挂、伪装成全量回归）。`bun check` 失败时 `brand/sync.sh` 会提示用它。
- `.omp/commands/sync-upstream.md`：同步流程的 slash 命令，编排脚本做不了的判断题——漂移审查取舍、按上游新结构收敛冲突、基线差分、补丁栈卫生、收口清单。

### Fixed

- `brand/sync.sh` 结束时 HEAD 停在派生分支 `release`：后续的 changelog / `--fixup` 提交会落在 `release` 上，被下次 `git branch -f release zcode` 无声丢掉。改为 `trap ... EXIT` 无条件送回 `zcode`（失败退出也送，因为要修的东西都在 `zcode`）。
- `brand/baseline.sh` 对非 `bun test` 命令退化为全量输出比对：基线 worktree 的 `target/` 是冷的，整批 `Compiling …` 与耗时行必然导致 exit=1 的「fork 引入」误判 —— 而 `sync.sh` 恰好在推荐 `baseline.sh bun run check:rs`。改为先抹平路径/耗时再只比信号行（`(fail)` / `error` / `warning:` / `panicked at`），退化到全量比对时明确警告并提示 `--filter`。夹具 `brand/baseline-probe.sh` 双向复验：纯噪声必须判「一致」，真错误必须单独报出。

### Changed

- `welcome.ts` 的 `PI_LOGO` 改为从 `brand-logo.ts` 取值（1 行 import + 1 行赋值）。名字保持 `PI_LOGO`，welcome 盒 / setup splash / outro / wizard header 四个消费点零改动。

### 同步

- 补丁栈 rebase 到 `upstream/main` @ `45e12e5bb`（193 个上游提交）。冲突 1 处：`crates/pi-natives/src/desktop/linux/wayland/portal.rs` — 上游把 `token_path(name)` 拆成 `omp_state_dir()` + `token_path`，品牌改动收敛为 `base.join("zcode")`。`chore: apply biome formatting to frontmatter.ts` 已进上游，rebase 自动丢弃。
- 从品牌 commit 中剔除误提交的构建产物 `packages/natives/native/embedded-addons.win32-x64.tar.gz`（27 MB）与被 `embed:native` 填充的 `packages/natives/native/embedded-addon.js`，后者恢复为上游 null stub——否则开发态 loader 会误判为 compiled-binary 模式。
