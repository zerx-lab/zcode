---
description: 提交commit
model: "@commit"      # 模型选择器，推荐角色别名
subtask: true         # 可选：显式改为当前会话执行
---

# git-cliff 规范化 Commit

分析变更，直接创建一条符合 git-cliff 默认配置的 Conventional Commit。不符合格式的 commit 会被 changelog 直接丢弃。

## 硬性规则

1. **直接提交**：分析后立即 `git commit`，不输出 message 征求确认。
2. **默认一条 commit** 覆盖全部变更；仅当变更明显互不相关、拆开能让 changelog 更清晰时才拆分。
3. **最少操作**：看 diff → 写 message → 提交。不通读文件、不跑测试/构建/lint。
4. **语言**：description 与 body 一律**简体中文**；type/scope/`!`/`BREAKING CHANGE:` 等结构标记保持英文小写；代码标识符与专有名词（API 名、crate 名、HLS/aria2 等）保留原文，不翻译。

## 流程

1. `git --no-pager diff --cached`；无暂存则先 `git --no-pager status` + `git --no-pager diff`，再 `git add` 需要提交的文件。
2. 按 diff 选定 type 与 scope（scope = 受影响模块/目录名，小写，可省略）。
3. `git commit -m "<subject>"`；带 body 时用多个 `-m`，每个 `-m` 一段。

## 格式

```
<type>(<scope>)!: <description>
```

| type | changelog 分组 |
|------|----------------|
| `feat` | 🚀 Features |
| `fix` | 🐛 Bug Fixes |
| `docs` | 📚 Documentation |
| `perf` | ⚡ Performance |
| `refactor` | 🚜 Refactor |
| `style` | 🎨 Styling |
| `test` | 🧪 Testing |
| `chore` / `ci` | ⚙️ Miscellaneous Tasks |
| `revert` | ◀️ Revert |

- type 优先级：能归入 `feat`/`fix` 的优先；纯重构用 `refactor`；其余杂项才用 `chore`
- 以下合法但**不会出现在 changelog**（skip 规则），仅在确实想隐藏时使用：`chore(release): prepare for ...`、`chore(deps*)`、`chore(pr)`、`chore(pull)`。依赖升级想进 changelog → 写 `fix(deps): ...`
- breaking change：type/scope 后加 `!`，或 footer 写 `BREAKING CHANGE: <说明>`
- 安全修复：body 中包含 `security` 一词 → 自动归入 🛡️ Security 分组

## 风格

- subject 尽量 ≤ 50 字符（硬上限 72），不以标点结尾
- body 与 subject 空一行，每行 ≤ 72 字符；只写动机与权衡，不复述 subject，无必要则省略

## 示例

```sh
git commit -m "feat(downloader): 新增 HLS AES-128 解密支持"
```

```sh
git commit -m "fix(db): 修复 WAL checkpoint 期间分段丢失" \
           -m "checkpoint 开始到事务提交之间写入的分段，在应用随即退出时会丢失。"
```

```sh
git commit -m "fix(bt): 修复从零续传并新增磁力链元数据超时" \
           -m "- 启动清理改为 clear_stale_session_state()，只删 session.json，
  保留 .bitv fastresume 文件
- 磁力链元数据获取加 5 分钟超时，超时转入 error 状态并提示用户"
```
