---
description: fork 低冲突开发规约：新功能优先新增文件，上游文件只做最小接线
alwaysApply: true
---

# Fork 低冲突开发规约

本仓库是 can1357/oh-my-pi 的二次开发 fork（补丁栈分支 `zcode`，上游速度 ~138 commits/天，每周 `bash brand/sync.sh` rebase 同步）。**每一行对上游文件的修改都是永久的 rebase 冲突面**。完整策略见 `docs/fork/sync-strategy.md`。

## 铁律（按优先级）

1. **新功能 = 新文件**。实现主体必须放在新增文件/新增目录中，绝不通过"增强"上游已有文件来实现功能。判断方法：`git ls-tree upstream/main -- <path>` 有输出的就是上游文件。
2. **上游文件只允许"最小接线"**：一行 import + 一行注册/调用。接线点优先选注册表/表驱动结构（`InternalUrlRouter.register()`、capability provider、dispatch table、`getConfigDirs` 类扫描目录），能注册就不要内联。
3. **品牌字面量一律取自 `packages/utils/src/brand.ts`**，禁止在任何调用点硬编码 `zcode` / `.zcode` / `ZCODE_` 等品牌串。
4. **禁改文件**（除非用户明确要求）：
   - 任何 `CHANGELOG.md`（全仓最热，3283 触碰/90d）——fork 变更记录写 `docs/fork/CHANGELOG.md`
   - 任何 `package.json`（发版全量触碰）
   - `src/prompts/**`、`docs/tools/**`（模型面文档，570 commits/90d）
   - 根 `README.md`、`assets/**`、`scripts/install.*`（overlay 管辖，改 `brand/` 下的源）
   - 根 `AGENTS.md`（上游所有；fork 规约就写在本规则文件里）
5. **commit 纪律**：品牌类改动与功能类改动分开 commit；每个 commit 独立通过 `bun check`。修上游 bug 的通用改动优先考虑能否 PR 回上游（合并一个补丁就少一处）。
6. **fork 专属产物的归属**：文档 → `docs/fork/`；品牌资产/脚本 → `brand/`；本规则 → `.omp/rules/`（zcode 经 `BRAND_COMPAT_PROJECT_CONFIG_DIRS` 兼容发现 `.omp/`，单份即可）。

## 扩展点速查

| 要加什么 | 去哪注册（不改上游逻辑） |
|---|---|
| 内部 URI scheme | `internal-urls/router.ts` 构造函数 +1 行 `register(...)`，handler 放新文件 |
| slash 命令 / 规则 / 技能 / 自定义工具 | `.omp/{commands,rules,skills,tools}/` 新文件，零代码接线 |
| 扩展逻辑（hooks） | `.omp/extensions/` 或 `~/.zcode/agent/extensions/`，loader 自动发现 |
| 品牌常量 | `packages/utils/src/brand.ts` 加导出 |
| 终端字标 / logo | `modes/components/brand-logo.ts` 改 `ZCODE_LOGO`（约束见 `docs/fork/sync-strategy.md`「品牌字标」）；SVG 源在 `brand/logo/` |
| 同步/构建脚本 | `brand/` 或 `scripts/` 新文件（不进根 package.json scripts） |
