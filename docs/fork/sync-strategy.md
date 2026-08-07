# Fork 同步与品牌化策略

本 fork 基于 [can1357/oh-my-pi](https://github.com/can1357/oh-my-pi) 二次开发，需要：换品牌（名字 / logo / 内部 scheme 如 `omp://`）、长期低冲突地跟进上游（上游速度 ~138 commits/天）。

本文档是唯一的策略真相源。`docs/fork/` 是上游不存在的路径，永不冲突。

## 核心原则

**不要在冲突里取胜，要让冲突不可能发生。**

品牌内容按所有权分两类，机制完全不同：

| 类别 | 特征 | 归属 | 机制 |
|---|---|---|---|
| **Patch（内联补丁）** | 上游也在改同一文件，必须逐行交错 | `zcode` 栈内 commit | `rebase` + `rerere` |
| **Overlay（整文件独占）** | 内容 100% 由本 fork 决定，上游版本无价值 | `release` 上的生成产物 | 每次同步重新生成，**不参与三方合并** |

判据：**只要有 TS/Rust 代码 import 它，就必须在 patch 里**（否则 `zcode` 栈单独过不了 `bun check`）。Overlay 只放"落在上游已有路径上的纯产物"（根 `README.md`、`assets/**`、`scripts/install.{sh,ps1}`）。

明确禁止：
- ❌ 改任何 `package.json` 的 `name` 字段（top30 热点里有 10 个 package.json，发版全量触碰）
- ❌ 碰 `CHANGELOG.md`（3283 次触碰/90d，全仓最热文件）
- ❌ 全局 sed rename `omp`/`oh-my-pi`（数千行 import 路径，等于永久全量冲突）
- ❌ 往根 `package.json` 加 scripts（292 次触碰/90d）；直接 `bun brand/sync.sh` 调用
- ❌ merge driver（`merge=ours` 等）：rebase 下 ours/theirs 方向反转，且 `.gitattributes` 在重放引入它自身的 commit 时尚未生效——静默丢失品牌文件

## 分支拓扑

```
upstream/main (can1357, 只读)
  │
  ├── main        纯镜像。从不提交，只 git reset --hard upstream/main
  │
  ├── zcode       补丁栈（唯一会冲突的分支）。品牌 commit 在前、功能 commit 在后。
  │               必须独立通过 bun check。同步 = git rebase upstream/main
  │
  └── release     派生物，可随时丢弃。= zcode + overlay 产物。
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

1. `git fetch upstream`
2. **漂移审查**（rebase 之前）：列出 `refs/brand/last-sync..upstream/main` 中新增的品牌硬编码候选（`"omp` / `oh-my-pi` / `can1357` / `.omp/` / `omp://`），人工裁决是否纳入补丁
3. `git checkout zcode && git rebase upstream/main` —— 唯一冲突点，rerere 自动重放已知解法（`rerere.enabled` + `rerere.autoUpdate` 已配置）
4. `git branch -f release zcode && git checkout release && bun brand/apply.ts`，产物 commit（空则跳过）
5. 门禁：`bun brand/verify.ts` + `bun check`
6. **全绿后**才 `git update-ref refs/brand/last-sync upstream/main`

节奏：**每周一次**，不要每天。一周 ~950 commits 批量解一次，rerere 命中率高；每天同步只是把同样的冲突拆成七份。

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
| `packages/coding-agent/src/cli/update-cli.ts` | 19-22 | `REPO` / `PACKAGE` / `HOMEBREW_FORMULA` / `MISE_TOOL` |
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

**SVG 侧**：`brand/logo/zcode-mark.svg`（品牌渐变，配色与 `welcome.ts` 的 `GRADIENT_STOPS` 5 段一致）、`brand/logo/zcode-mark-mono.svg`（深底单色）。`brand/logo/preview.html` 是本地对照页。这两个文件是 **overlay 源**，由 `brand/apply.ts` 拷到 `assets/icon.svg` 等上游路径 —— `assets/**` 属 overlay 管辖，禁止直接编辑。

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
  apply.ts          # 幂等：按 manifest 渲染/复制到目标路径
  verify.ts         # 运行时门禁（见下）
  sync.sh           # 同步脚本
  manifest.json     # 源 → 目标路径映射
  README.md         # fork 的根 README 源
  assets/           # icon.svg / hero.png 等（源见 brand/logo/）
  logo/             # 字标定稿：zcode-mark.svg / -mono.svg / preview.html
  templates/        # install.sh / install.ps1 模板
```

### `brand/verify.ts` 运行时门禁

**不扫二进制字符串**（npm 包名 `@oh-my-pi/*` 刻意不改，必然残留，扫描只有噪声）。断言用户可见行为：

| 断言 | 期望 |
|---|---|
| `--version` 输出 | fork 产品名 |
| `getConfigRootDir()` | `~/.<forkname>` |
| TUI 首屏 | fork logo，不含 `Oh My Pi` |
| `<scheme>://docs` | 可解析 |
| `omp://docs` | **仍可解析**（别名兼容） |
| `<PREFIX>_*` env | 正确映射到 `PI_*` |

## AI 开发规约（新增文件优先）

目标：让 AI 在本仓开发任何功能时**优先新增文件、上游文件只做最小接线**，把新增的冲突面压为零。

机制选型（为什么不是 AGENTS.md）：

| 载体 | 结论 | 理由 |
|---|---|---|
| 根 `AGENTS.md` | ❌ | 上游所有（15 commits/90d），任何追加都是永久补丁面 |
| `~/.zcode/agent/AGENTS.md`（用户级） | ❌ | 不进仓库，换机器/协作者即失效 |
| **`.omp/rules/`（项目级 rules）** | ✅ | 上游虽跟踪 `.omp/{commands,skills}` 但没有 `rules/` 子目录 → 新文件零冲突；`alwaysApply: true` 每个会话自动注入；进仓库、随 clone 生效 |

实现：单份 `.omp/rules/fork-low-conflict.md`。上游 omp 原生读取 `.omp/`；zcode 经 **`.omp` compat 发现**（`brand.ts` 的 `BRAND_COMPAT_PROJECT_CONFIG_DIRS`，接入 `config.ts` priorityList 与 `discovery/builtin.ts` getConfigDirs）同时发现 `.zcode/` 与 `.omp/`，按 name 去重——上游 tracked 的 `.omp/{commands,skills}` 更新对 zcode 免维护跟随，用户已有 `.omp/` 项目配置无缝兼容，且无需维护任何镜像文件。规则内容：新功能=新文件、接线只走注册表、品牌串只取 `brand.ts`、禁改热点文件清单、扩展点速查表。

`.omp` compat 读取面（全部经 `BRAND_PROJECT_CONFIG_DIR_NAMES` / `getProjectAgentDirCandidates` 查表）：`config.ts` priorityList、`discovery/builtin.ts` getConfigDirs / 项目 mcp.json / RULES.md walk、`discovery/helpers.ts` 项目插件 registry anchor、`advisor/watchdog.ts`、`secrets/index.ts`、`discovery/omp-extension-roots.ts`、`discovery/ssh.ts`、`config/prompt-templates.ts`。

**写路径 = 粘性解析**（`brand.ts` 的 `resolveProjectConfigFileIn`，接入 `dirs.ts` 的 `getMCPConfigPath`/`getSSHConfigPath`、`settings.ts` 项目 config.yml、`omfg-controller` 规则目录）：写到"文件/目录已存在的候选"（legacy 项目继续整体活在 `.omp/`），都不存在才用 native `.zcode`——避免"定义在 .omp、状态写进 .zcode"的数据分家。settings 的解析结果按 load 缓存，避免备份-重命名后写目标漂移。

测试影响（修正早前"无需改任何上游测试"的说法——该承诺只对 scheme 别名成立，对改名本身不成立）：
- 65 个品牌敏感测试文件全部实测；**8 个测试文件**做了品牌无关化补丁（断言/fixture 里的 `.omp` 字面量 → `CONFIG_DIR_NAME`/`getConfigDirName()`），改法在上游同样通过，可 PR 回上游：legacy-pi-cli-exports、pi-config-dir、settings-reload-cwd、omp-plugins、issue-4197、gh、omfg-controller、marketplace/{manager,project-scope}。
- 与 pre-brand 基线（`3e9bedbf1` worktree）逐文件对比，失败集完全一致；仅剩 2 个预存的 Windows 环境失败（EBUSY 清理、路径分隔符 toContain），与品牌无关。

已知残留（可接受）：帮助文案/注释中的 `~/.omp` 字样（纯展示，无功能影响）；`brand:drift` 只扫上游**新增**，预存字面量已通过一次性全树扫描 + 65 文件实测处置。

## 长期收敛

把"去硬编码"重构 PR 回上游（`logger.ts` 用 `APP_NAME`、`init-xdg.ts` 改 import、docs scheme 抽常量）。每合并一个，patch 少一处，最终收敛为「`brand.ts` 一个新文件 + 几行值替换」。

## 当前状态（2026-08-07）

- [x] `upstream` remote → can1357/oh-my-pi
- [x] `rerere.enabled` + `rerere.autoUpdate`
- [x] `zcode` 分支（补丁栈，原名 `fork`）= upstream/main (ab78d3091) + `82436b0e9`（Windows native build 修复，原 `fix/windows-native-build`）
- [x] `refs/brand/last-sync` = ab78d3091
- [x] 本文档 + `brand/sync.sh`
- [x] `brand.ts` + 内联替换补丁 —— 品牌值：产品/二进制 `zcode`、配置目录 `~/.zcode`、scheme `zcode://`（`omp://` 隐藏别名）、env 前缀 `ZCODE_`（`OMP_` 保留）、仓库 `zerx-lab/oh-my-pi`
- [x] AI 低冲突开发规约：`.omp/rules/fork-low-conflict.md`（alwaysApply；zcode 经 compat 发现，无镜像）
- [x] `.omp` compat 发现：`BRAND_COMPAT_PROJECT_CONFIG_DIRS` 接入 config.ts / discovery / watchdog；`agents unpack --project` 写路径改 `CONFIG_DIR_NAME`
- [x] logo 更换：`brand-logo.ts`（终端字标 Heavy Z）+ `welcome.ts` 2 行接线；SVG 定稿在 `brand/logo/`
- [ ] `assets/**` 图形替换（icon.svg / hero.png / banner.html）—— 依赖 `brand/apply.ts`
- [ ] `brand/apply.ts` + `verify.ts` + overlay 源（README / install 脚本）
- [ ] `release` 分支首次生成
