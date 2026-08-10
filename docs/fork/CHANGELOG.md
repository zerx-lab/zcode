# Fork 变更记录

上游 `packages/*/CHANGELOG.md` 属上游所有，fork 侧改动只记在这里。

## [Unreleased]

### Added
- zcode 品牌字标：终端块字符版 `packages/coding-agent/src/modes/components/brand-logo.ts`（`ZCODE_LOGO`，12×5，仅用 `█ ▀ ▄` 以保证 setup splash 2x 放大不发虚）；SVG 版 `brand/logo/zcode-mark.svg`（品牌渐变）与 `brand/logo/zcode-mark-mono.svg`（深底单色）。
- `brand/hooks/pre-commit`：提交门禁，拦截被中断的 binary build 留下的 populated 构建占位文件（`embedded-addon.js` / `mupdf-wasm-embed.ts` / `embedded-client.generated.txt`）与 `embedded-addons.*.tar.gz`。判据是带分号的 `with { type: "file" };` —— mupdf 占位文件的注释里含不带分号的同一串，少分号会把纯占位形态误判成 populated 且无法通过 reset 解除。`brand/hooks/selftest.sh` 是双向探针（4 拒 + 4 放行），已接入 `brand/sync.sh` 门禁。`brand/sync.sh` 幂等安装 `core.hooksPath`；cherry-pick / rebase / 有冲突后的 `rebase --continue` 均实测不触发，不干扰上游同步。
- `packages/natives/native/.gitignore`（新文件，零冲突）：补上上游遗漏的 `embedded-addons.*.tar.gz` ignore 规则。
- `brand/baseline.sh`：基线差分。在 `upstream/main` 的 worktree（`../omp-baseline`，复用）里跑同一条命令并比对失败集，用来判定 rebase 后的报错是 fork 引入还是上游在本机平台的预存问题。内建两个必踩的坑：worktree 要 `bun install`（只在首次/lockfile 变更时跑），以及要拷 `packages/natives/native/*.node`（缺了会让每个测试文件秒挂、伪装成全量回归）。`bun check` 失败时 `brand/sync.sh` 会提示用它。
- `.omp/commands/sync-upstream.md`：同步流程的 slash 命令，编排脚本做不了的判断题——漂移审查取舍、按上游新结构收敛冲突、基线差分、补丁栈卫生、收口清单。
- `packages/ai/src/registry/oauth/oauth-brand.ts`：OAuth 回调页（`/login` 登录后浏览器落地的那一页）的品牌层。上游 `oauth.html` 里的 `<title>oh my pi · authentication</title>` 与 `.brand` 头部（π 字标 SVG + `oh my pi` wordmark）被换成 zcode Z 字标与 wordmark。按**结构**匹配（`<title>` 元素、`.brand` 容器）而不是按上游文案字面量，上游改文案仍能命中；锚点真的漂移到匹配不上时只 `logger.warn` 并原样返回——登录流程与品牌无关，绝不能因换字标而挂掉回调页。`callback-server.ts` 只接线 2 行（1 行 import + 1 行模块级 `BRANDED_TEMPLATE`），`__OAUTH_STATE__` 注入契约不变。哨兵测试 `packages/ai/test/oauth-brand.test.ts`。

### Changed
- `welcome.ts` 的 `PI_LOGO` 改为从 `brand-logo.ts` 取值（1 行 import + 1 行赋值）。名字保持 `PI_LOGO`，welcome 盒 / setup splash / outro / wizard header 四个消费点零改动。
- 状态栏与终端标签页的紧凑字标 `π` → `Z`：新增 `BRAND_MARK`（`packages/utils/src/brand.ts`），`modes/theme/symbols.ts` 的 `icon.pi` 三档预设（unicode / nerd / ascii）与 `utils/title-generator.ts` 的 `DEFAULT_TERMINAL_TITLE` 改为取该常量，poimandres 明暗主题的 `icon.pi` 覆盖同步为 `Z`。nerd 档不再用 `\ue22c`（该 glyph 就是 π）以免在装了 nerdfont 的终端上变回上游品牌。相关测试的 `π` 字面量改引 `BRAND_MARK`。
- 品牌配色由「粉→紫→青→薄荷」五段渐变改为**单色相青蓝**（hue ≈ 200°，只走明度变化），并收敛为**单一色值真源** `BRAND_RAMP`（`packages/utils/src/brand.ts`，5 档 hex）。三个消费面全部派生、零副本：终端 logo 走 `BRAND_RAMP_RGB`（`brand-logo.ts` 的 `BRAND_GRADIENT_STOPS`，上游 `welcome.ts` 的 10 行调色板收缩成 2 行取值，冲突面反而变小），OAuth 回调页运行时铺 stop / 拼光晕，`brand/logo/*.svg` 由新增的 `brand/gen-logo.ts` 生成。改色板 + 重跑脚本即全站生效，已实测（把 5 档填成同值 → 终端 escape、OAuth stops/chrome、SVG 同时变平）。终端四个消费点与 `gradientEscape` 插值、shine 高光逻辑零改动。
- OAuth 回调页的页面配色一并收敛到蓝：`oauth-brand.ts` 新增第三个锚点 `</head>`，在上游 `<style>` 之后追加覆盖样式（同特异性、后来者胜），改掉 `--magenta`/`--iris` 与 body 的两团径向光晕（上游把颜色硬编码在 `background` 里、不走变量，只能整条重写；用 `RRGGBBAA` 八位 hex 承载透明度），wordmark 从中性白改为品牌蓝。`--success`/`--error` 信号色不动——成功/失败态必须仍然一眼可辨。上游 `oauth.html` 依旧零改动。
- `brand/logo/preview.html` 从三份手工同步的内联 SVG 副本改为 `<img src>` 直引 `zcode-mark.svg` / `zcode-mark-mono.svg`；`zcode-mark-mono.svg` 删掉从未被 path 引用的死渐变 defs。改色时不会再只改一半。
- 新增 `brand/gen-logo.ts`（从 `BRAND_RAMP` 生成两个 SVG 字标）与哨兵 `packages/utils/test/brand-ramp.test.ts`：落盘 SVG 是唯一「可能忘记同步」的缝，测试拿 `renderMarkSvg(BRAND_RAMP)` 与磁盘文件逐字节比对，改了色板没重跑脚本就红；同时钉住 hue ∈ (185°, 215°)，防止有人往品牌里塞回第二个色相。

### Fixed
- `brand/sync.sh` 结束时 HEAD 停在派生分支 `release`：后续的 changelog / `--fixup` 提交会落在 `release` 上，被下次 `git branch -f release zcode` 无声丢掉。改为 `trap ... EXIT` 无条件送回 `zcode`（失败退出也送，因为要修的东西都在 `zcode`）。
- `brand/baseline.sh` 对非 `bun test` 命令退化为全量输出比对：基线 worktree 的 `target/` 是冷的，整批 `Compiling …` 与耗时行必然导致 exit=1 的「fork 引入」误判 —— 而 `sync.sh` 恰好在推荐 `baseline.sh bun run check:rs`。改为先抹平路径/耗时再只比信号行（`(fail)` / `error` / `warning:` / `panicked at`），退化到全量比对时明确警告并提示 `--filter`。夹具 `brand/baseline-probe.sh` 双向复验：纯噪声必须判「一致」，真错误必须单独报出。