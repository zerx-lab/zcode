---
description: fork 低冲突开发规约：每次动手先算 rebase 冲突成本，优先零冲突落点
alwaysApply: true
---

# Fork 低冲突开发规约

本仓库是 can1357/oh-my-pi 的二次开发 fork（补丁栈分支 `zcode`，上游 ~138 commits/天）。同步双轨：CI（`.github/workflows/sync-upstream.yml`）每天自动 rebase `zcode` 并强推（rerere 解法经 `refs/brand/rr-cache` 从本地发布给 CI）；CI 失败或每周例行时人工 `bash brand/sync.sh`。**每一行对上游文件的修改都是永久的 rebase 冲突面**，且 CI 每天可能重写 `zcode` 历史——本地开工前先 `git fetch origin && git rebase origin/zcode`。

这条规约不限制你能改什么。它的作用是让每次动手都带着一个问题：**这行改动下周要不要我再解一次冲突？** 完整策略见 `docs/fork/sync-strategy.md`。

## 动手前三问

1. **这是上游文件吗？** `git ls-tree upstream/main -- <path>` 有输出就是。
2. **有零冲突落点吗？** 实现主体能否放进新增文件，上游文件只留「一行 import + 一行注册」？
3. **这个冲突面值不值？** 值就改——但要清楚自己在花什么，并把理由写进 commit message，下次解冲突的人（大概还是你）需要它。

## 落点成本排序

同一件事优先往上面选：

|落点|冲突成本|
|---|---|
|新增文件 / 新增目录|零。上游没有这个路径，永不参与三方合并|
|上游注册表接线|一行。`InternalUrlRouter.register()`、capability provider、dispatch table、`getConfigDirs` 类候选表——表驱动结构的新增行几乎不冲突|
|上游文件内联逻辑|每周可能解一次冲突。能拆成「新文件 + 接线」就拆|
|上游热文件|几乎必冲突。`CHANGELOG.md`（3283 触碰/90d）、`src/prompts/**` 与 `docs/tools/**`（570 commits/90d）、`package.json`（发版全量触碰）。动之前先确认前三档真的没有替代|

## 硬约束

这几条没有判断空间，因为违反它们会造成静默的数据/品牌不一致：

- **品牌字面量一律从 `packages/utils/src/brand.ts` 取**，禁止在调用点硬编码 `zcode` / `.zcode` / `ZCODE_`。
- **fork 变更记录写 `docs/fork/CHANGELOG.md`**，不写上游 `packages/*/CHANGELOG.md`（全仓最热文件，且 fork 不发 npm，写上游那份没有读者只有冲突）。
- **上游/overlay 管辖的文件改它们的源**：根 `AGENTS.md` 属上游（fork 规约就写在本文件里）；根 `README.md`、`assets/**`、`scripts/install.*` 全部由 `brand/apply.ts` 生成，改 `brand/` 下的模板。其中 `README.md` 是 `stack` 档（补丁栈上就是渲染态，冲突由 merge driver 解成重渲染），`assets/**` 与 `scripts/install.*` 是 `release` 档（**只在 `release` 分支上存在**，出现在 `zcode` 上就是事故）。
- **品牌类与功能类改动分开 commit**，每个 commit 独立通过 `bun check`。
- **修上游 bug 的通用改动先考虑 PR 回上游**——合并一个补丁就永久少一处冲突面。

## 扩展点速查

|要加什么|去哪落地（不改上游逻辑）|
|---|---|
|内部 URI scheme|`internal-urls/router.ts` 构造函数 +1 行 `register(...)`，handler 放新文件|
|slash 命令 / 规则 / 技能 / 自定义工具|`.omp/{commands,rules,skills,tools}/` 新文件，零代码接线|
|扩展逻辑（hooks）|`.omp/extensions/` 或 `~/.zcode/agent/extensions/`，loader 自动发现|
|品牌常量 / 目录候选表|`packages/utils/src/brand.ts`、`brand-dirs.ts` 加导出|
|终端字标 / logo|`modes/components/brand-logo.ts` 改 `ZCODE_LOGO`（约束见 `docs/fork/sync-strategy.md`「品牌字标」）；SVG 源在 `brand/logo/`|
|界面文案翻译|`packages/coding-agent/src/i18n/locales/<locale>/` 补词条；上游文件只在渲染点取词（约束见 `docs/fork/i18n.md`）|
|同步/构建脚本|`brand/` 或 `scripts/` 新文件（不进根 `package.json` scripts）|

## fork 产物归属

文档 → `docs/fork/`；品牌资产与脚本 → `brand/`；AI 规则/命令 → `.omp/`（zcode 经 `BRAND_COMPAT_PROJECT_CONFIG_DIRS` 同时发现 `.zcode/` 与 `.omp/`，所以单份即可，不需要镜像）。
