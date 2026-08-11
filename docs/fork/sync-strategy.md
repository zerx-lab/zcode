# Fork 同步与品牌化策略

本 fork 基于 [can1357/oh-my-pi](https://github.com/can1357/oh-my-pi) 二次开发，需要：换品牌（名字 / logo / 内部 scheme 如 `omp://`）、长期低冲突地跟进上游（上游速度 ~138 commits/天）。

本文档是唯一的策略真相源。`docs/fork/` 是上游不存在的路径，永不冲突。

## 核心原则

**不要在冲突里取胜，要让冲突不可能发生。**

品牌内容按所有权分两类，机制完全不同：

| 类别 | 特征 | 归属 | 机制 |
|---|---|---|---|
| **Patch（内联补丁）** | 上游也在改同一文件，必须逐行交错 | `zcode` 栈内 commit | `rebase` + `rerere` |
| **Overlay（整文件独占）** | 内容 100% 由本 fork 决定，上游版本无价值 | 见下面两档 | 从模板重新生成，**不参与三方合并** |

判据：**只要有 TS/Rust 代码 import 它，就必须在 patch 里**（否则 `zcode` 栈单独过不了 `bun check`）。Overlay 只放"落在上游已有路径上的纯产物"，按归属再分两档（`apply.ts` 里 `MANIFEST` 每条都带 `scope`）：

|档|目标|在 `zcode` 上|在 `release` 上|
|---|---|---|---|
|`stack`|根 `README.md`|**必须是渲染态**|同左（内容一致）|
|`release`|`assets/**`、`scripts/install.{sh,ps1}`|必须**缺席**（保持上游原样）|必须已应用|

`README.md` 单独进 `stack` 档：默认分支是 `zcode`，GitHub 首页和一行流安装说明都从它读，等到 `release` 才品牌化就晚了。代价是它进补丁栈、参与 rebase —— 由 `zcode-readme` merge driver 兜底（见下），上游 README 的内容永不并入。其余产物留在 `release`：那条分支每次同步 `git branch -f` 重建，结构上不可能冲突。

明确禁止：
- ❌ 改任何 `package.json` 的 `name` 字段（top30 热点里有 10 个 package.json，发版全量触碰）
- ❌ 碰 `CHANGELOG.md`（3283 次触碰/90d，全仓最热文件）
- ❌ 全局 sed rename `omp`/`oh-my-pi`（数千行 import 路径，等于永久全量冲突）
- ❌ 往根 `package.json` 加 scripts（292 次触碰/90d）；直接 `bun brand/sync.sh` 调用
- ⚠️ merge driver：默认禁止（rebase 下 ours/theirs 方向反转，且版本内 `.gitattributes` 在重放引入它自身的 commit 之前尚未生效——静默丢失品牌文件）。**唯一例外**是 `README.md` 的 `zcode-readme`，两条反对意见都被单独拆掉了：driver 不取任何一侧，而是用 `brand/README.md` 重渲染（与 ours/theirs 方位无关）；挂载点写在 `.git/info/attributes`（由 `brand/git-setup.sh` 装，不随重放位置生效/失效）。行为探针 `brand/merge-readme-selftest.sh` 造真实 rebase 冲突验证，含"不注册 driver 时确实会冲突"的负对照。

## 分支拓扑

```
upstream/main (can1357, 只读)
  │
  ├── main        纯镜像。从不提交，只 git reset --hard upstream/main
  │
  ├── zcode       补丁栈（唯一会冲突的分支）。品牌 commit 在前、功能 commit 在后。
  │               必须独立通过 bun check。同步 = git rebase upstream/main
  │
  └── release     派生物，可随时丢弃。= zcode + release 档 overlay 产物。
                  每次同步 git branch -f release zcode 重建，从不 rebase。
```

- 功能与品牌**共栈**：拆成两条补丁分支不减少对上游的冲突集，反而额外引入 brand↔feat 集成合并，净负收益。
- `release` 上的 overlay commit 每次重建，结构上不可能冲突。
- 漂移基线：可动 ref `refs/brand/last-sync`（不用日期 tag：单一真相、不积垃圾、失败不推进）。

## 同步流程

```bash
bash brand/sync.sh
```

脚本步骤（详见 `brand/sync.sh`）：

0. `bash brand/git-setup.sh` —— 仓库级 git 配置（提交门禁 / rerere / README merge driver + 它在 `.git/info/attributes` 的挂载点）。幂等，CI 的 `sync-upstream.yml` 调的是同一份，两边配置不可能漂移
1. `git fetch upstream`
2. **漂移审查**（rebase 之前）：列出 `refs/brand/last-sync..upstream/main` 中新增的品牌硬编码候选（`"omp` / `oh-my-pi` / `can1357` / `.omp/` / `omp://`），人工裁决是否纳入补丁
3. `git checkout zcode && git rebase upstream/main` —— 唯一冲突点，rerere 自动重放已知解法；`README.md` 的冲突由 merge driver 直接解成重渲染
4. `git branch -f release zcode && git checkout release && bun brand/apply.ts`，产物 commit（空则跳过）。`apply.ts` 按当前分支分档写盘：在 `zcode` 上误跑只会刷新 `README.md`，不会把 `release` 产物撒进补丁栈
5. 门禁：`bun brand/verify.ts` + `bash brand/hooks/selftest.sh` + `bash brand/merge-readme-selftest.sh` + `bun check`
6. **全绿后**才 `git update-ref refs/brand/last-sync upstream/main`，并把 `.git/rr-cache` 快照发布到 `origin` 的 `refs/brand/rr-cache`（供 CI 重放）

### 自动同步（GitHub Actions）

`.github/workflows/sync-upstream.yml`（zcode 上的 fork 新文件）每天 UTC 22:00 无人值守执行：fetch upstream → 从 `refs/brand/rr-cache` 种入 rerere 解法 → `bash brand/git-setup.sh`（driver 必须在 rebase **之前**注册，否则 git 静默回落到三方合并）→ rebase `zcode`（带进度守卫：`REBASE_HEAD` 不前进即 abort，防非冲突失败被吞或无限重试）→ 门禁 `bun brand/verify.ts --skip-cli` + merge driver 探针 + `bun run check:ts` → `--force-with-lease=zcode` 只推 `zcode`；镜像分支 `main` 的快进是独立的 best-effort 步骤（`continue-on-error`），失败不阻断也不参与失败语义——**run 红 = `zcode` 没动，需人工**。前提：fork 默认分支须为 `zcode`（`schedule` 只读默认分支上的 workflow）。

冲突分工：

- **rerere 见过的冲突** → CI 自动重放、推送，无需人工。
- **新冲突** → CI `rebase --abort`、run 失败并邮件通知，绝不推半解决状态。人工本地跑 `bash brand/sync.sh` 解一次，脚本全绿后自动发布 `rr-cache` 快照，**同一冲突只需人工解一次**。

注意本地推送互动：CI 用 `--force-with-lease`，不会覆盖你刚推的新提交；反过来 CI 每天可能重写 `zcode` 历史，**本地开工前先 `git fetch origin && git rebase origin/zcode`**（或确认无分叉）。

节奏：CI 每天消化干净/可重放的部分；人工完整流程（漂移审查、overlay、`bun check` 全量、基线推进）在 CI 失败或每周例行时执行。

## 品牌补丁构成（待实施清单）

### 单一值源（新文件，零冲突）

`packages/utils/src/brand.ts` —— 导出 `BRAND_APP_NAME` / `BRAND_DISPLAY_NAME` / `BRAND_CONFIG_DIR_NAME` / `BRAND_DOCS_SCHEME` / `BRAND_ENV_PREFIX` / `BRAND_REPO`。

- `packages/utils` 的 `exports` 已声明 `./*` 通配 → 自动可经 `@oh-my-pi/pi-utils/brand` 访问，barrel 不用动。
- `dirs.ts` 对 `APP_NAME`/`CONFIG_DIR_NAME` 已是全仓唯一出口 → 改为 re-export `brand.ts`，全部调用点零改动。
- `brand/apply.ts` 直接 import 该文件渲染模板，值不做二次拷贝。
- **字标不在这里**：终端 logo 是 TUI 资产（依赖 `pi-tui` 的渲染约束），放 `packages/coding-agent/src/modes/components/brand-logo.ts`，见下文「品牌字标」。放进 `pi-utils/brand.ts` 会让底层包背上 TUI 语义。

### 内联替换（~40 行 / 11 文件，rerere 覆盖）

| 文件 | 行 | 内容 |
|---|---|---|
| `packages/utils/src/dirs.ts` | 20, 23 | `APP_NAME` / `CONFIG_DIR_NAME` 改为 re-export brand.ts |
| `packages/utils/src/logger.ts` | 61, 62, 214, 219 | 日志文件名/正则硬编码 `omp.`（未走 APP_NAME） |
| `packages/utils/src/env.ts` | ~186-191 | 已有 `OMP_*→PI_*` 镜像循环，复制 3 行加新前缀（`OMP_` 保留兼容） |
| `packages/coding-agent/src/cli/commands/init-xdg.ts` | 5 | 本地重复 `APP_NAME="omp"`，改 import |
| `packages/coding-agent/src/cli/update-cli.ts` | 20-25, ~150, ~1090, ~1130-1175 | `REPO` / `PACKAGE` / `HOMEBREW_FORMULA` / `MISE_TOOL`；fork 发布通道接线（import `update-fork-release.ts`、`releaseTag` 贯穿资产查找、`runUpdateCommand` 的发现/比较/通道门禁） |
| `packages/coding-agent/src/modes/components/welcome.ts` | 12, 454 | `PI_LOGO` 改为取自 `brand-logo.ts`（1 行 import + 1 行赋值） |
| `packages/coding-agent/src/modes/setup-wizard/scenes/splash.ts` | ~192 | 硬编码 `"O h   M y   P i"` |
| `packages/ai/src/utils/openrouter-headers.ts` | 5-7 | User-Agent / X-Title / Referer |
| `crates/pi-natives/src/crash_handler.rs` | 49, 54 | Rust 侧孪生常量（与 dirs.ts 手工同步） |
| `crates/pi-natives/src/desktop/linux/wayland/portal.rs` | 6-9 | XDG 子目录名 |
| `packages/coding-agent/scripts/build-binary.ts` | 79-80 | 二进制产物名 |
| `packages/coding-agent/package.json` | bin 字段 | CLI 名（远离 version 字段，冲突风险可接受） |

### 品牌字标（logo）

**值源**：`packages/coding-agent/src/modes/components/brand-logo.ts`（新文件，零冲突）导出 `ZCODE_LOGO`。`welcome.ts` 保留 `PI_LOGO` 这个**符号名不变**，只把值换掉 —— 四个消费点（welcome 盒、setup splash `LARGE_LOGO`、outro、wizard header）因此零改动，上游改这四处也不会波及品牌。

**艺术字硬约束**（改稿前必读，都是被消费方代码逼出来的）：

| 约束 | 来源 |
|---|---|
| 每行等宽、5 行 | `gradientLogo` 按 `x + (rows-1-y)` 算对角渐变，行宽不齐渐变错位 |
| 空格 = 透明 | `gradientLogo` 跳过空格不着色 |
| 只用全块 `█ ▀ ▄` | splash 的 `LARGE_LOGO` 横向复制每字符、纵向复制每行；半格/四分块（`▐ ▌ ▗ ▛`）加倍成 `▐▐`、`▗▗`，边缘发虚 |
| 宽度贴着 12 | welcome 是两栏布局，更宽挤压右栏 |

**当前字标**（Heavy Z）：

```
████████████
       ▄███▀
    ▄███▀   
 ▄███▀      
████████████
```

**配色：唯一色值真源是 `packages/utils/src/brand.ts` 的 `BRAND_RAMP`**（5 档 hex，单色相青蓝 hue ≈ 200°，只走明度变化）。想改品牌色，改这一个数组，然后 `bun brand/gen-logo.ts`。所有消费方都从它派生，仓库里没有第二份色值：

|消费面|怎么拿到色|
|---|---|
|终端 logo / splash 水面|`brand-logo.ts` 的 `BRAND_GRADIENT_STOPS` = `BRAND_RAMP_RGB`（模块加载时从 hex 解析），上游 `welcome.ts` 的两个同名常量收缩成两行取值|
|无 truecolor 兜底|`BRAND_RAMP_256`（xterm 索引，与 hex 独立但同为蓝）|
|OAuth 回调页字标 + 页面配色|`oauth-brand.ts` 运行时铺 stop、拼 `RRGGBBAA` 光晕|
|`brand/logo/*.svg`|`bun brand/gen-logo.ts` 生成的落盘产物|

落盘 SVG 是唯一「可能忘记同步」的缝，由 `packages/utils/test/brand-ramp.test.ts` 守住：它拿 `renderMarkSvg(BRAND_RAMP)` 和磁盘上的文件逐字节比对，改了色板没重跑脚本就红。同一个测试还钉住 hue 落在 185°–215°，防止有人往品牌里塞回第二个色相。

档数必须保持 5：`gradientEscape` 按 `t * (stops.length - 1)` 分段插值，改档数会变动 splash 水面与 logo 的色带节奏。

**SVG 侧**：`brand/logo/zcode-mark.svg`（品牌蓝渐变）、`brand/logo/zcode-mark-mono.svg`（深底纯白，不含品牌色故不随色板变）。`brand/logo/preview.html` 是本地对照页，用 `<img src>` 直引这两个真源——曾经三份内联副本，改色时必然只改一半。这两个 SVG 是 **overlay 源**，由 `brand/apply.ts` 拷到 `assets/icon.svg` 等上游路径 —— `assets/**` 属 overlay 管辖，禁止直接编辑。

**OAuth 回调页**：`packages/ai/src/registry/oauth/oauth-brand.ts` 是 web 侧品牌层。它按结构锚点（`<title>`、`.brand`、`</head>`）改写上游 `oauth.html`，上游那份文件一行不动。

### `omp://` scheme：改主名 + 保留 `omp` 隐藏别名

`InternalUrlRouter`（`packages/coding-agent/src/internal-urls/router.ts:37-54`）是 Map 注册表。注册两个 handler：新 scheme 为主，`omp` 保留为别名。

收益：上游 ~20 行测试（`omp-protocol.test.ts`、`grep-internal-urls.test.ts` 等）与 `docs/tools/*.md` 全部不用改。

需同步改 3 处：
- `src/prompts/system/system-prompt.md:72`（模型看到的 scheme，1 行）
- `src/tools/grep.ts:342` `OMP_ROOT_URL_RE` 放宽为匹配两个 scheme
- `src/tools/path-utils.ts:42` `INTERNAL_SCHEMES_WITH_SELECTORS` 加键

### Overlay（`brand/` 目录，全部在 patch commit 内）

```
brand/
  apply.ts                    # MANIFEST（scope + 三种生成方式），--check 审计 / --render 单目标
  verify.ts                   # 运行时门禁（见下），--skip-cli 供 addon 未就绪的前置门禁
  git-setup.sh                # 仓库级 git 配置：hooks / rerere / README merge driver + 挂载点
  merge-readme.sh             # merge driver 本体：README 冲突 → 用 brand/README.md 重渲染
  merge-readme-selftest.sh    # 上面那条的行为探针（真 rebase 冲突 + 负对照 + 回退路径）
  gen-logo.ts                 # BRAND_RAMP → brand/logo/*.svg
  gen-hero.ts                 # banner.html → brand/assets/hero.png（无头 Chromium 光栅化）
  sync.sh                     # 同步脚本
  baseline.sh                 # 上游基线差分
  hooks/pre-commit            # 提交门禁（见下）
  README.md                   # fork 的根 README 源（{{TOKEN}} 模板，stack 档）
  assets/                     # banner.html（模板）+ hero.png（checked-in 光栅产物）
  logo/                       # 字标定稿：zcode-mark.svg / -mono.svg / preview.html
```

manifest 就是 `apply.ts` 里的 `MANIFEST` 数组，不单独开 json：`rewrite` 条目要带函数式的锚点规则，拆成数据文件只会把规则和执行分家。每条带一个 `scope`（`stack` / `release`，见「核心原则」）和三种生成方式之一：

|方式|用于|失效行为|
|---|---|---|
|`copy`|整文件由 fork 拥有（`assets/icon.svg` ← `brand/logo/zcode-mark.svg`、`assets/hero.png`）|源文件缺失即报错|
|`render`|fork 拥有内容但色值/名字必须取自 `brand.ts`（`README.md`、`assets/banner.html`）|未知 `{{TOKEN}}` 即报错|
|`rewrite`|上游文件仍是主体，只做定量替换（`scripts/install.sh`、`scripts/install.ps1`）|锚点命中数低于 `min` 即报错|

`rewrite` 而不是「整文件模板」是刻意的：install 脚本上游还在持续改（musl 冒烟、Rosetta bun 检测都是近期加的），拷成模板等于把这些改进永久冻结在 fork 分叉的那一刻。定量锚点让上游改进自动跟随，只有锚点真的消失才需要人工裁决。`\bomp\b` 一条规则同时覆盖资产名、安装路径、提示文案；npm 包名 `@oh-my-pi/pi-coding-agent` 不含该词也不含 `can1357/oh-my-pi`，因此不会被误伤（fork 不发 npm，源码安装改为克隆 `zcode` 分支）。

`hero.png` 是 checked-in 光栅产物而不是 apply 时生成：光栅化要 Chromium，不能挂进每周同步路径。改了 `banner.html` 或 `BRAND_RAMP` 就 `bun brand/gen-hero.ts` 重跑并提交。

### `brand/hooks/pre-commit` 提交门禁

安装：`git config core.hooksPath brand/hooks`（`brand/sync.sh` 幂等执行，clone 后跑一次同步即生效）。

拦截的是**构建产物误提交**，不是代码风格。`scripts/build-binary.ts` 在编译期把三个 checked-in 占位文件就地改写成 populated 形态，`finally` 里再 reset 回去；构建被 Ctrl-C 或崩溃打断时 reset 不执行，随后一次 `git add -A` 就把产物提交进补丁栈。

| 受保护路径 | 占位形态 | populated 判据 |
|---|---|---|
| `packages/natives/native/embedded-addon.js` | `embeddedAddon = null` | 含 `with { type: "file" };` |
| `packages/coding-agent/src/utils/mupdf-wasm-embed.ts` | `return undefined` | 含 `with { type: "file" };` |
| `packages/stats/src/embedded-client.generated.txt` | 空文件 | 非零字节 |
| `packages/natives/native/embedded-addons.*.tar.gz` | 不该存在 | 被 staged 即拒绝 |

判据取「populated 形态特有的 import 行」而非逐字节比对 stub，上游改注释/typedef 不会误报。**分号是判据的一部分**：mupdf 占位文件自己的说明注释里就写着不带分号的 `` `with { type: "file" }` ``（`embed-mupdf-wasm.ts` 的 placeholder 模板原文），少一个分号就会把纯占位形态判成 populated，而且提示的 `gen:mupdf:reset` 写回来的还是同一份注释，用户无从解除。

`brand/hooks/selftest.sh` 是这条判据的探针，8 个用例覆盖两个方向（4 个 populated 必拒 + 4 个占位/无关必放行）。**两侧夹具都不写死**：占位形态取工作区原文，populated 形态从生成器的 generated 模板里现抽 import 行（`embed-native.ts` / `embed-mupdf-wasm.ts`，`${...}` 插值换成字面路径）。上游一改模板写法，marker 会静默失配而夹具跟着变 —— `*-populated` 用例立刻从 reject 翻成 accept 报错，不会两个方向一起假绿（实测：把 generated 模板的分号去掉，探针精确报 `FAIL mupdf-populated want=reject got=accept`）。

不直接跑 `gen:*` 取夹具：那要求探针所在 shell 的 PATH 上有 `bun`，WSL / 裸 Git Bash 常常没有（本机 WSL 就没有），拿不到夹具只能 SKIP，等于在最需要覆盖的环境退化成假绿。抽模板零依赖、不改工作区。改判据后必跑。

侧产物用 `.gitignore` 兜底：`packages/natives/native/.gitignore`（新文件，零冲突）补上 `embedded-addons.*.tar.gz`。这是上游遗漏（同类的 `*.node` 与 `src/utils/mupdf-wasm.wasm` 上游都已 ignore），可 PR 回上游。

**为什么这条门禁必要**：populated 的 `embedded-addon.js` 会让 `loader-state.js` 的 `detectCompiledBinary()` 在开发态返回 `true`（该函数以 embedded-addon 是否为 null 作为编译态的权威判据），`resolveLoaderCandidates()` 于是把 `~/.zcode/natives/<version>` 排在 `nativeDir` **之前** —— 陈旧的已发布 `.node` 静默抢在本地新构建之前被加载，且 `shouldStageNodeModulesAddon()` 会跳过 Windows 的 node_modules 暂存路径。已实际发生过一次（同时夹带 27 MB tar.gz 进历史）。

重放路径实测（git 2.54.0）：`cherry-pick`、无冲突 `rebase`、**以及有冲突后的 `rebase --continue`**，三者都不触发 `pre-commit`（最后一条单独造了冲突验证：populated 占位文件被原样重放，退出码 0）。门禁因此不干扰 `bash brand/sync.sh`；确需手工绕过用 `git commit --no-verify`。

### `brand/verify.ts` 运行时门禁

**不扫二进制字符串**（npm 包名 `@oh-my-pi/*` 刻意不改，必然残留，扫描只有噪声）。断言用户可见行为：

| 断言 | 期望 |
|---|---|
| 源码入口 `--version` | `zcode/<version>` |
| `getConfigRootDir()` | basename = `.zcode` |
| `zcode://docs` | 路由可受理 |
| `omp://docs` | **仍可受理**（别名兼容） |
| `ZCODE_*` / `OMP_*` env | 都映射到 `PI_*` |
| `welcome.ts` 的 `PI_LOGO` | === `ZCODE_LOGO`，且 5 行等宽、只含 `█ ▀ ▄` |
| overlay | 期望态成立：`stack` 档处处是渲染态；`release` 档只在 `release` 分支上应用，补丁栈上必须缺席 |

末条让门禁在两个分支上都有意义，判据是**分支 + 分档**而不是文件内容启发式。旧实现靠"README 里有没有 `BRAND_REPO`"嗅探自己在哪条分支上，README 进 `stack` 档之后这条嗅探恒真，于是把 `release` 档也拿到 `zcode` 上校验 —— `zcode-v17.2.12-z3` 的发布就是这么红的（而且红在 73 分钟的 addon 构建之后）。

## 发布（`.github/workflows/zcode-release.yml`）

新文件 → 零冲突。上游 `ci.yml` 一行不动：它只在 push `main` / PR 时触发，`v*` tag 的发布走 main 分支推送；本工作流只认 `zcode-v*` tag，两套流程不重叠。

```
zcode 上打 zcode-v<上游版本>-z<迭代>
  └─ gate       ubuntu：tag ↔ 迭代号一致性 + 品牌门禁(--skip-cli) + 两个 selftest。~1 min
       ├─ natives    ubuntu：串行建 6 个非 darwin addon（并发会 OOM）
       │    └─ binaries   ubuntu：完整品牌门禁 + bun --compile 交叉出 5 个非 darwin 二进制
       ├─ darwin×2   macos-14 / macos-15-intel：自带 bazel addon 构建
       └─ publish    omp-* → zcode-* 改名 + SHA256SUMS.txt + GitHub Release
```

`gate` 独立成作业并前置于其余三条腿：不依赖 native addon 的检查（tag 一致性、overlay 期望态、hooks 与 merge driver 探针）30 秒就能跑完，排在 addon 后面等于每次坏 tag 都先烧掉一小时机时（z3 实测 73 分钟）。需要 addon 才能起源码入口的那一项（`--version`）留在 `binaries` 作业里补跑。

与上游发布流的差异（fork 只有 GitHub Releases 一条通道）：不发 npm（`@oh-my-pi/*` 包名刻意不改，发布会撞名）、不更新 Homebrew tap、不做 Apple Developer ID 签名/公证（darwin 仍是 ad-hoc 签名）、全部跑 GitHub 托管 runner（上游的 `omp-kata` 自建池本 fork 没有，bazel 靠 `actions/cache` 磁盘缓存，首次冷构建慢是预期内的）。

资产名用 fork 命名（`zcode-linux-x64` / `zcode-windows-x64.exe` …）：overlay 后的 install 脚本正是按这个名字取文件，两边都由 `\bomp\b` 规则与 publish 步的改名保持一致。上游 `ci-release-build-binaries.ts` 因此零改动。

tag 打在 `zcode` 上（不是 `release`）：二进制内容与 overlay 无关，而 `release` 每次同步都会被 `git branch -f` 重建，tag 挂在派生分支上会指向被丢弃的历史。install 一行流指向 `release` 分支的 `scripts/install.*`，那里才有品牌化后的脚本。

## AI 开发规约（新增文件优先）

目标：让 AI 在本仓开发任何功能时**优先新增文件、上游文件只做最小接线**，把新增的冲突面压为零。

机制选型（为什么不是 AGENTS.md）：

| 载体 | 结论 | 理由 |
|---|---|---|
| 根 `AGENTS.md` | ❌ | 上游所有（15 commits/90d），任何追加都是永久补丁面 |
| `~/.zcode/agent/AGENTS.md`（用户级） | ❌ | 不进仓库，换机器/协作者即失效 |
| **`.omp/rules/`（项目级 rules）** | ✅ | 上游虽跟踪 `.omp/{commands,skills}` 但没有 `rules/` 子目录 → 新文件零冲突；`alwaysApply: true` 每个会话自动注入；进仓库、随 clone 生效 |

实现：单份 `.omp/rules/fork-low-conflict.md`。上游 omp 原生读取 `.omp/`；zcode 经 **`.omp` compat 发现**（`brand.ts` 的 `BRAND_COMPAT_PROJECT_CONFIG_DIRS`，接入 `config.ts` priorityList 与 `discovery/builtin.ts` getConfigDirs）同时发现 `.zcode/` 与 `.omp/`，按 name 去重——上游 tracked 的 `.omp/{commands,skills}` 更新对 zcode 免维护跟随，用户已有 `.omp/` 项目配置无缝兼容，且无需维护任何镜像文件。规则内容不是禁令清单而是**成本排序**：动手前三问 + 落点成本表（新增文件 → 注册表接线 → 上游内联 → 上游热文件）+ 5 条硬约束 + 扩展点速查表；目的是让每次改动自己算 rebase 账，而不是靠「禁止」把人卡死在需要改上游的场景里。

项目规则的祖先解析：`discovery/builtin.ts` 的 `loadRules` 按 cwd → repoRoot 走祖先（与同文件 skills 一致），否则从 `packages/coding-agent/` 这类子目录启动会话时，仓库根 `.omp/rules/` 整个不注入。边界先过 `isAncestorDir()` 校验再交给 `getAncestorDirs()`（后者只在完全相等时停，坏边界=扫到文件系统根）。

`.omp` compat 读取面（全部经 `BRAND_PROJECT_CONFIG_DIR_NAMES` / `getProjectAgentDirCandidates` 查表）：`config.ts` priorityList、`discovery/builtin.ts` getConfigDirs / 项目 mcp.json / RULES.md walk、`discovery/helpers.ts` 项目插件 registry anchor、`advisor/watchdog.ts`、`secrets/index.ts`、`discovery/omp-extension-roots.ts`、`discovery/ssh.ts`、`config/prompt-templates.ts`。

**用户级 compat 只覆盖声明式上下文**（`BRAND_COMPAT_USER_CONFIG_DIRS` + `getUserAgentDirCandidates()`，接线点 `builtin.ts` 的 `compatUserAgentPaths`）：`~/.omp/agent/` 下的 `rules/`、`RULES.md`、`AGENTS.md`、`SYSTEM.md`。**不接进通用 `getConfigDirs()`** —— 那张表同时喂 extensions / hooks / custom tools / `settings.json`，合并另一个品牌的 agent 目录等于启动时执行它的扩展代码、混进它的设置；sessions / auth / MCP / settings 一律 native-only。候选从 `getConfigRootDir()` 切分品牌段得出而不调 `os.homedir()`（`dirs.ts` 的 `RESOLVER_HOME` 在模块加载时锚死 home，mock 过 homedir 的调用方会让两者不一致、compat 静默消失）；只在默认布局 `agentDir === <configRoot>/agent` 下镜像。

**写路径 = 粘性解析**（`brand.ts` 的 `resolveProjectConfigFileIn`，接入 `dirs.ts` 的 `getMCPConfigPath`/`getSSHConfigPath`、`settings.ts` 项目 config.yml、`omfg-controller` 规则目录）：写到"文件/目录已存在的候选"（legacy 项目继续整体活在 `.omp/`），都不存在才用 native `.zcode`——避免"定义在 .omp、状态写进 .zcode"的数据分家。settings 的解析结果按 load 缓存，避免备份-重命名后写目标漂移。

测试影响（修正早前"无需改任何上游测试"的说法——该承诺只对 scheme 别名成立，对改名本身不成立）：
- 65 个品牌敏感测试文件全部实测；**8 个测试文件**做了品牌无关化补丁（断言/fixture 里的 `.omp` 字面量 → `CONFIG_DIR_NAME`/`getConfigDirName()`），改法在上游同样通过，可 PR 回上游：legacy-pi-cli-exports、pi-config-dir、settings-reload-cwd、omp-plugins、issue-4197、gh、omfg-controller、marketplace/{manager,project-scope}。
- 与 pre-brand 基线（`3e9bedbf1` worktree）逐文件对比，失败集完全一致；仅剩 2 个预存的 Windows 环境失败（EBUSY 清理、路径分隔符 toContain），与品牌无关。

已知残留（可接受）：帮助文案/注释中的 `~/.omp` 字样（纯展示，无功能影响）；`brand:drift` 只扫上游**新增**，预存字面量已通过一次性全树扫描 + 65 文件实测处置。

## 长期收敛

把"去硬编码"重构 PR 回上游（`logger.ts` 用 `APP_NAME`、`init-xdg.ts` 改 import、docs scheme 抽常量）。每合并一个，patch 少一处，最终收敛为「`brand.ts` 一个新文件 + 几行值替换」。

## 当前状态（2026-08-10）

- [x] `upstream` remote → can1357/oh-my-pi
- [x] `rerere.enabled` + `rerere.autoUpdate`
- [x] `zcode` 分支（补丁栈，原名 `fork`）= upstream/main (ab78d3091) + `82436b0e9`（Windows native build 修复，原 `fix/windows-native-build`）
- [x] `refs/brand/last-sync` = ab78d3091
- [x] 本文档 + `brand/sync.sh`
- [x] `brand.ts` + 内联替换补丁 —— 品牌值：产品/二进制 `zcode`、配置目录 `~/.zcode`、scheme `zcode://`（`omp://` 隐藏别名）、env 前缀 `ZCODE_`（`OMP_` 保留）、仓库 `zerx-lab/zcode`
- [x] AI 低冲突开发规约：`.omp/rules/fork-low-conflict.md`（alwaysApply；zcode 经 compat 发现，无镜像）
- [x] `.omp` compat 发现：`BRAND_COMPAT_PROJECT_CONFIG_DIRS` 接入 config.ts / discovery / watchdog；`agents unpack --project` 写路径改 `CONFIG_DIR_NAME`
- [x] logo 更换：`brand-logo.ts`（终端字标 Heavy Z）+ `welcome.ts` 2 行接线；SVG 定稿在 `brand/logo/`
- [x] `brand/apply.ts` + `verify.ts` + overlay 源（fork README / banner / install 脚本改写）
- [x] `assets/**` 图形替换：`icon.svg` ← `brand/logo/zcode-mark.svg`，`banner.html` ← `brand/assets/banner.html`（模板），`hero.png` ← `bun brand/gen-hero.ts` 光栅化产物
- [x] `release` 分支首次生成（`git branch -f release zcode` + overlay commit）
- [x] 发布流水线 `.github/workflows/zcode-release.yml`（`zcode-v*` tag 触发）
