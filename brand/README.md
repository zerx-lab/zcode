<p align="center">
  <img src="https://github.com/{{REPO}}/blob/{{RELEASE_BRANCH}}/assets/hero.png?raw=true" alt="{{APP_NAME}}">
</p>

<p align="center">
  <strong>A coding agent with the IDE wired in.</strong><br>
  <sub>{{DISPLAY_NAME}} — a rebranded downstream fork of <a href="https://github.com/{{UPSTREAM_REPO}}">oh-my-pi</a></sub>
</p>

<p align="center">
  <a href="https://github.com/{{REPO}}/releases/latest"><img src="https://img.shields.io/github/v/release/{{REPO}}?style=flat&colorA=222222&colorB=3dc7ff&label=release" alt="Release"></a>
  <a href="https://github.com/{{REPO}}/actions/workflows/zcode-release.yml"><img src="https://img.shields.io/github/actions/workflow/status/{{REPO}}/zcode-release.yml?style=flat&colorA=222222&colorB=3FB950&label=release%20build" alt="Release build"></a>
  <a href="https://github.com/{{REPO}}/blob/{{RELEASE_BRANCH}}/LICENSE"><img src="https://img.shields.io/badge/license-MIT-58A6FF?style=flat&colorA=222222" alt="License"></a>
  <a href="https://www.typescriptlang.org"><img src="https://img.shields.io/badge/TypeScript-3178C6?style=flat&colorA=222222&logo=typescript&logoColor=white" alt="TypeScript"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-DEA584?style=flat&colorA=222222&logo=rust&logoColor=white" alt="Rust"></a>
  <a href="https://bun.sh"><img src="https://img.shields.io/badge/runtime-Bun-f472b6?style=flat&colorA=222222" alt="Bun"></a>
</p>

---

## 这是什么

`{{APP_NAME}}` 是 [{{UPSTREAM_REPO}}](https://github.com/{{UPSTREAM_REPO}}) 的**换品牌二次开发分支**。
Agent 能力、工具集、模型目录全部来自上游并**每周 rebase 跟进**；本 fork 只改品牌面与发布通道，
不分叉功能路线。

上游文档（工具、配置、SDK、架构）依然适用，只需把命令名 `omp` 读作 `{{APP_NAME}}`：
**[上游 README](https://github.com/{{UPSTREAM_REPO}}#readme)**。

| 面 | 上游 | 本 fork |
|---|---|---|
| CLI / 二进制 | `omp` | `{{APP_NAME}}` |
| 配置目录 | `~/.omp` | `~/{{CONFIG_DIR_NAME}}`（项目级 `.omp/` 仍可读，无需迁移） |
| 内部文档 scheme | `omp://` | `{{DOCS_SCHEME}}://`（`omp://` 保留为隐藏别名） |
| 环境变量前缀 | `OMP_*` | `{{ENV_PREFIX}}*`（`OMP_*` 保留兼容） |
| 发布通道 | npm + Homebrew + GitHub Releases | 仅 GitHub Releases（`{{REPO}}`） |
| 更新通道 | `omp update` | GitHub Releases，见下 |

npm 包名（`@oh-my-pi/*`）**刻意不改**：改名等于把数千行 import 路径变成永久 rebase 冲突面。
本 fork 不向 npm 发布。

## 安装

**macOS · Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/{{REPO}}/{{RELEASE_BRANCH}}/scripts/install.sh | sh
```

> **Alpine / musl:** 预编译 musl 二进制动态链接 `libstdc++`/`libgcc`，Alpine 默认不带：
> 先 `apk add libstdc++ libgcc`。

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/{{REPO}}/{{RELEASE_BRANCH}}/scripts/install.ps1 | iex
```

**直接下载**：[Releases](https://github.com/{{REPO}}/releases/latest) 里按平台取
`{{APP_NAME}}-linux-x64` / `{{APP_NAME}}-darwin-arm64` / `{{APP_NAME}}-windows-x64.exe` 等资产，
`SHA256SUMS.txt` 是同一次构建生成的校验和。

**从源码**（需要 bun ≥ 1.3.14）

```sh
git clone -b zcode https://github.com/{{REPO}}.git
cd zcode && bun install && bun run setup
```

跑起来：

```sh
{{APP_NAME}}
```

## 分支与发布

```
upstream/main        can1357/oh-my-pi，只读
  ├── main           纯镜像，从不提交
  ├── zcode          补丁栈：品牌 commit + fork 功能 commit，每周 rebase 上游
  └── {{RELEASE_BRANCH}}         zcode + overlay 产物（本 README / assets / install 脚本），每次同步重建
```

- 同步：`bash brand/sync.sh`（fetch → 漂移审查 → rebase → 重建 overlay → 门禁）。
- 发布：在 `zcode` 上打 `{{APP_NAME}}-v*` tag，[zcode-release](https://github.com/{{REPO}}/actions/workflows/zcode-release.yml)
  工作流构建全平台二进制并创建 GitHub Release。
- 策略细节（为什么这么分支、overlay 为什么不参与三方合并）：
  **[docs/fork/sync-strategy.md](docs/fork/sync-strategy.md)**。

## 贡献

通用改进请优先提给**上游** [{{UPSTREAM_REPO}}](https://github.com/{{UPSTREAM_REPO}})：
合并一个就少一处 fork 补丁。只与本 fork 品牌/发布相关的问题再开到本仓。

## License

MIT，同上游。See [LICENSE](LICENSE).

© 2025 Mario Zechner
© 2025-2026 Can Bölük
