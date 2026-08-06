---
description: ZCode 交付流程约束：GitHub 行为限制、commit/merge 信息格式、changelog 维护、发布流程。提交、写 changelog、发版、在 GitHub 上行动前必读。
---

# ZCode 交付流程约束

## GitHub 限制

除非用户明确告知需要发布的具体内容：

- 绝不在 GitHub 上发表评论（issue、PR、discussion 均不例外）。
- 绝不在 GitHub 上创建 issue。

## Commit

- 除非用户明确要求，绝不提交 commit。
- 采用 Conventional Commits 格式。
- Merge commit（维护者合并 PR 时）格式固定为：`Merge PR #<number>: <conventional PR subject> (@<author>)`。例如：`Merge PR #6386: feat(catalog): add native Meta Model API provider (@eggpeat)`。

## Changelog

位置：`crates/*/CHANGELOG.md`，每个 crate 各自维护一份。

**格式**——`## [Unreleased]` 下按固定顺序排列的 section：

1. `### Breaking Changes`（存在则必须排最前）
2. `### Added`
3. `### Changed`
4. `### Fixed`
5. `### Removed`

**规则：**

- 新条目一律加到 `## [Unreleased]` 下。
- 已发布 section（如 `## [0.12.2]`）不可变，绝不能回头修改。
- Code review / PR 里不要挑 changelog 的 section 顺序或格式问题——发布脚本会跑 `fix-changelogs` 自动规范化，人工挑格式是浪费。

**归属标注**——两种形式，格式固定：

- 内部贡献（来自 issue）：`Fixed foo bar ([#123](https://github.com/zerx-lab/zcode/issues/123))`。
- 外部贡献（来自 PR）：`Added feature X ([#456](https://github.com/zerx-lab/zcode/pull/456) by [@username](https://github.com/username))`。

## 发布

1. 确保自上次发布以来的所有改动，都已写进各受影响 crate 的 `[Unreleased]` section——发布前检查这一步，不要等发布脚本报错才补。
2. 发布入口：`cargo xtask release` (planned)——xtask 尚未落盘，落盘前不要手动 `cargo publish` 替代它。

该任务会做的事：版本号递增并同步 workspace 内部依赖引用、把 `[Unreleased]` 定稿为带版本号的 section、创建 commit 与 tag、执行 `cargo publish`、并在 CHANGELOG 顶部新开一个空的 `## [Unreleased]` section。
