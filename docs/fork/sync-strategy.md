# Fork 同步与品牌化策略

本 fork 基于 [can1357/oh-my-pi](https://github.com/can1357/oh-my-pi) 二次开发，需要：换品牌（名字 / logo / 内部 scheme 如 `omp://`）、长期低冲突地跟进上游（上游速度 ~138 commits/天）。

本文档是唯一的策略真相源。`docs/fork/` 是上游不存在的路径，永不冲突。

## 核心原则

**不要在冲突里取胜，要让冲突不可能发生。**

品牌内容按所有权分两类，机制完全不同：

| 类别 | 特征 | 归属 | 机制 |
|---|---|---|---|
| **Patch（内联补丁）** | 上游也在改同一文件，必须逐行交错 | `fork` 栈内 commit | `rebase` + `rerere` |
| **Overlay（整文件独占）** | 内容 100% 由本 fork 决定，上游版本无价值 | `release` 上的生成产物 | 每次同步重新生成，**不参与三方合并** |

判据：**只要有 TS/Rust 代码 import 它，就必须在 patch 里**（否则 `fork` 栈单独过不了 `bun check`）。Overlay 只放"落在上游已有路径上的纯产物"（根 `README.md`、`assets/**`、`scripts/install.{sh,ps1}`）。

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
  ├── fork        补丁栈（唯一会冲突的分支）。品牌 commit 在前、功能 commit 在后。
  │               必须独立通过 bun check。同步 = git rebase upstream/main
  │
  └── release     派生物，可随时丢弃。= fork + overlay 产物。
                  每次同步 git branch -f release fork 重建，从不 rebase。
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
3. `git checkout fork && git rebase upstream/main` —— 唯一冲突点，rerere 自动重放已知解法（`rerere.enabled` + `rerere.autoUpdate` 已配置）
4. `git branch -f release fork && git checkout release && bun brand/apply.ts`，产物 commit（空则跳过）
5. 门禁：`bun brand/verify.ts` + `bun check`
6. **全绿后**才 `git update-ref refs/brand/last-sync upstream/main`

节奏：**每周一次**，不要每天。一周 ~950 commits 批量解一次，rerere 命中率高；每天同步只是把同样的冲突拆成七份。

## 品牌补丁构成（待实施清单）

### 单一值源（新文件，零冲突）

`packages/utils/src/brand.ts` —— 导出 `APP_NAME` / `DISPLAY_NAME` / `CONFIG_DIR_NAME` / `DOCS_SCHEME` / `REPO` / `NPM_PACKAGE` / `LOGO`。

- `packages/utils` 的 `exports` 已声明 `./*` 通配 → 自动可经 `@oh-my-pi/pi-utils/brand` 访问，barrel 不用动。
- `dirs.ts` 对 `APP_NAME`/`CONFIG_DIR_NAME` 已是全仓唯一出口 → 改为 re-export `brand.ts`，全部调用点零改动。
- `brand/apply.ts` 直接 import 该文件渲染模板，值不做二次拷贝。

### 内联替换（~40 行 / 11 文件，rerere 覆盖）

| 文件 | 行 | 内容 |
|---|---|---|
| `packages/utils/src/dirs.ts` | 20, 23 | `APP_NAME` / `CONFIG_DIR_NAME` 改为 re-export brand.ts |
| `packages/utils/src/logger.ts` | 61, 62, 214, 219 | 日志文件名/正则硬编码 `omp.`（未走 APP_NAME） |
| `packages/utils/src/env.ts` | ~186-191 | 已有 `OMP_*→PI_*` 镜像循环，复制 3 行加新前缀（`OMP_` 保留兼容） |
| `packages/coding-agent/src/cli/commands/init-xdg.ts` | 5 | 本地重复 `APP_NAME="omp"`，改 import |
| `packages/coding-agent/src/cli/update-cli.ts` | 19-22 | `REPO` / `PACKAGE` / `HOMEBREW_FORMULA` / `MISE_TOOL` |
| `packages/coding-agent/src/modes/components/welcome.ts` | ~453 | `PI_LOGO` art（splash/wizard 全部派生自它） |
| `packages/coding-agent/src/modes/setup-wizard/scenes/splash.ts` | ~192 | 硬编码 `"O h   M y   P i"` |
| `packages/ai/src/utils/openrouter-headers.ts` | 5-7 | User-Agent / X-Title / Referer |
| `crates/pi-natives/src/crash_handler.rs` | 49, 54 | Rust 侧孪生常量（与 dirs.ts 手工同步） |
| `crates/pi-natives/src/desktop/linux/wayland/portal.rs` | 6-9 | XDG 子目录名 |
| `packages/coding-agent/scripts/build-binary.ts` | 79-80 | 二进制产物名 |
| `packages/coding-agent/package.json` | bin 字段 | CLI 名（远离 version 字段，冲突风险可接受） |

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
  assets/           # icon.svg / hero.png 等
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

## 长期收敛

把"去硬编码"重构 PR 回上游（`logger.ts` 用 `APP_NAME`、`init-xdg.ts` 改 import、docs scheme 抽常量）。每合并一个，patch 少一处，最终收敛为「`brand.ts` 一个新文件 + 几行值替换」。

## 当前状态（2026-08-07）

- [x] `upstream` remote → can1357/oh-my-pi
- [x] `rerere.enabled` + `rerere.autoUpdate`
- [x] `fork` 分支 = upstream/main (ab78d3091) + `82436b0e9`（Windows native build 修复，原 `fix/windows-native-build`）
- [x] `refs/brand/last-sync` = ab78d3091
- [x] 本文档 + `brand/sync.sh`
- [x] `brand.ts` + 内联替换补丁 —— 品牌值：产品/二进制 `zcode`、配置目录 `~/.zcode`、scheme `zcode://`（`omp://` 隐藏别名）、env 前缀 `ZCODE_`（`OMP_` 保留）、仓库 `zerx-lab/oh-my-pi`。logo 暂不换（`PI_LOGO` 原样）
- [ ] logo 更换（`welcome.ts` `PI_LOGO` + `assets/`，后续）
- [ ] `brand/apply.ts` + `verify.ts` + overlay 源（README / install 脚本）
- [ ] `release` 分支首次生成
