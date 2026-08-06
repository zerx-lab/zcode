# `.omp/` — ZCode 的 agent 协作层

这份目录是**给 agent 的运行时配置**，不是文档。本文件是唯一的例外：它解释每个文件为什么在这里、
被谁加载、以及踩过哪些坑。改 `.omp/` 之前读它。

## 目录职责

| 路径                             | 谁加载 / 何时生效                                                        | 职责                                       |
| -------------------------------- | ------------------------------------------------------------------------ | ------------------------------------------ |
| `../AGENTS.md`                    | `agents-md` provider，会话开场全文注入一次                                | 记忆索引：坐标、命令、导航（≤120 行）        |
| `RULES.md`                        | native provider，转成 always-apply 规则，**贴近当前 turn 反复重挂**       | 行为红线，长会话也不能忘（≤25 行）           |
| `rules/*.md`                      | 系统提示只列 `name` + `description`，模型按需 `read rule://<name>`        | 领域细则，按需加载                          |
| `rules/rust-forbidden-patterns.md` | 有 `condition`，进 TTSR 桶：正则命中流式内容时注入全文，**默认每会话只触发一次** | 写 `.rs` 时的即时替代方案（必须极短）        |
| `agents/*.md`                     | `task` 工具按 `name` 精确匹配                                            | 项目专用 subagent                           |
| `commands/*.md`                   | `/<文件名>` 展开为普通用户 prompt（**不执行 shell**）                      | 固化的多步流程                              |
| `extensions/index-guard.ts`       | native provider 自动发现 `.omp/extensions/`，Bun 加载                     | 确定性守卫：行数、路径存在性、顶层目录坐标、锚新鲜度；收尾前自动对账 |
| `checks/index-guard.check.ts`     | **不被任何 loader 扫描**，供人工 / CI 直接 `bun` 运行                      | 守卫检测逻辑的命令行入口（复用同一份纯函数） |
| `WATCHDOG.md`                     | 追加到**每个** advisor 的 system prompt                                   | 项目审查关注点                              |
| `WATCHDOG.yml`                    | advisor 名册，与用户级同名文件一起加载                                    | 项目特化的对抗式审查者                      |
| `config.yml`                      | 项目 settings 层，叠在用户级之上                                          | 项目级策略（刻意极简，见下）                |
| `lsp.json`                        | 合并到内置 LSP defaults 之上                                             | rust-analyzer 能力开关与就绪超时            |

## 为什么索引在仓库根，而不是 `.omp/AGENTS.md`

native provider 优先级 100，`agents-md` 只有 10。**如果创建 `.omp/AGENTS.md`，它会在 depth 0
把根 `AGENTS.md` 整份 shadow 掉** —— 根文件不再进上下文，而且不报错。

本项目选择根 `AGENTS.md`：它同时被 Codex、Cursor、Copilot 等工具识别，是跨工具的单一事实来源。
代价是必须**永远不创建** `.omp/AGENTS.md`。`.omp/` 非空但缺 `AGENTS.md` 时，native 不贡献 context file，
根文件正常存活 —— 这正是当前布局。

## 为什么细则在 `rules/` 而不是塞进索引

索引每个会话全文注入。指令数量上升时模型对**全部**指令的遵循率下降，超预算的截断还会砍掉中段。
`rules/` 的内容只在系统提示里占一行 `name: description`，正文按需 `rule://` 拉取 —— 同样的知识量，
上下文成本差一个数量级。

**注意**：`@path` import 虽然可用，但它是**内联展开**，等于把分片文件的全文搬进索引，
省不了预算。分片要用 `rules/`，不要用 `@` import。

## 三个层次不能互相复制

- `AGENTS.md`：坐标与命令；
- `RULES.md`：红线；
- `rules/*.md`：细则。

同一条约束只允许存在一份。这不是洁癖：always-apply 规则（`RULES.md`）的内容若已出现在
**context file**（本项目里就是 `AGENTS.md`）中，会被去重逻辑**静默跳过注入**，而且没有任何提示 ——
在 `AGENTS.md` 里复述一条红线，等于让 `RULES.md` 里那条彻底失效。
按需读取的 `rules/*.md` **不**参与这个去重（它们不是 context file），但同一约束散落多份 rule
依然会造成改一处漏一处。

## `config.yml` 为什么这么短

settings 层的合并规则是：对象深合并，**标量与数组整层替换**。所以项目级文件里写数组键，
会把用户级的同名数组整表覆盖。已知会出事的键：

`disabledProviders`、`enabledModels`、`cycleOrder`、`modelProviderOrder`、
`skills.customDirectories`、`bashInterceptor.patterns`

（`bash.patterns` 是唯一的例外：项目确实需要它做危险命令护栏，且用户级通常不设。它只写 deny/prompt
条目、不写 `match: "*"` 兜底，以免劫持使用者自己的 approval 偏好。）

另外不写 `tools.approvalMode`、`theme`、`modelRoles`：那些是使用者的人机偏好，不是项目属性。

## 已知陷阱

- **项目配置不向上走祖先目录**：必须在含本 `.omp/` 的目录启动 omp，`config.yml` / `rules/` /
  `commands/` / `extensions/` 才生效。
- **`RULES.md` 会被用户级同名文件 shadow**：两者都合成规则名 `RULES`，去重按名字来，通常用户级胜。
  当前用户级没有该文件，所以项目版生效；一旦使用者自己加了 `~/.omp/agent/RULES.md`，项目红线会静默失效。
- **别把普通规则命名为 `rules/RULES.md`**：它会同时 shadow 掉项目和用户的 sticky 规则。
- **rule 没有 `description` 也没有 `condition` = 废文件**：既不进 rulebook 列表，也不能 `rule://` 寻址。
- **`globs` 对 rulebook 只是提示**，不会按文件自动加载规则；只有 TTSR 用它做路径闸门。
- **`commands/*.md` 不执行 shell**，只是 prompt 模板；`$ARGUMENTS` 由调用时的参数替换。
- **改了 `commands/` 里的文件需要刷新/重启会话**才会被发现，没有文件监听。
- **`lsp.json` 一旦提供非空 `servers`，就不再是纯自动检测**：override 会先合并到内置 defaults，
  然后按 root marker 匹配、二进制可解析、非 disabled 过滤。当前只覆盖 `capabilities` 与
  `workspaceReadyTimings`，其余继承内置定义。
- **rust-analyzer 的 root marker 只看启动目录一层**：必须在含根 `Cargo.toml` 的目录启动 omp，LSP 才会拉起。
- **`autolearn` 那条路走不通**：`learn` / `manage_skill` 只在 `autolearn.enabled` 为真时注册，
  且 managed skill 落在**用户**目录；使用者当前把 `autolearn` 关着，`disabledProviders` 里还有
  `omp-managed`。项目沉淀知识的路径是 `AGENTS.md` + index-guard + `/sync-index`，**不是** autolearn。
  也不要试图在项目 `config.yml` 里改 `disabledProviders` 去"放开"它 —— 数组整表替换，会连带解禁别的。
- **`extensions/` 里只能放导出 factory 的模块**：loader 会 dynamic import 该目录下的**每个** `.ts`，
  不导出 factory 的文件会在每次启动打印 `Failed to load extension ...`（已在真实会话中复现）。
  所以检查器 CLI 放在 `checks/` —— 那不是 omp 的约定目录，不会被任何 loader 扫描。
- **长期记忆不是项目真相**：`<memories>` 是启发式背景，与代码或 `AGENTS.md` 冲突时以后者为准。
  坐标类事实必须写进索引，不能只 `retain`。
- **会话压缩不会删规范**：压缩改写的是会话历史；`AGENTS.md`、`RULES.md`、rulebook 列表每轮重新组装。
  但压缩后模型更依赖摘要，此时应重新打开 `AGENTS.md` / `rule://` 核对，而不是只信摘要。

## 怎么验证这一层真的生效

- 规则被发现：`read rule://rust-quality`（拿不到就是没被加载或缺 `description`）；
- subagent 可派发：**当前会话即可**用 `task` 显式指定 `agent: "index-keeper"` —— 派发时会重新扫盘；
  只有"模型自己看到的 agent 列表"按 cwd 做了进程级缓存，所以新增 agent 后列表里可能暂时看不到，
  但直接点名派发照样成功；
- 命令被发现：输入 `/gate` 应展开为 prompt，而不是原样发给模型；
- extension 被加载：**需要新会话**。加载后用 `write` 提交超 120 行的 `AGENTS.md` 会被直接拒绝；
  **`edit` 不会被拦截** —— hashline 补丁里没有结构化的目标路径与最终行数，守卫只能在
  `tool_result` 阶段事后把违规报告追加到工具结果里（不改成失败），以及在下次会话开场提示。
  换句话说：`write` 是硬闸门，`edit` 是事后报告，别把后者当拦截；
- **自动对账**：会话想收尾时若索引仍不同步，`session_stop` 会强制续跑一轮并给出违规清单，
  每会话只触发一次（`continue` 配额上限 8，这里只用 1）。所以"忘了跑 `/sync-index`"不再等于索引烂掉；
  但改写仍由 agent 完成 —— 检测能定位，不能替你决定哪条该删；
- 反向检测（顶层目录没有坐标）用**反引号路径 token 精确比对**，不是全文子串 ——
  否则 `test/` 会被命令表里的 `cargo test` 命中而漏报。确实不该入索引的目录写
  `<!-- index-ignore: <名字> -->`，决定落盘可 review；
- 守卫逻辑本身：`bun .omp/checks/index-guard.check.ts` 可独立跑，不依赖 omp 是否加载了 extension。

## 扩展这一层时

1. 先确认要加的东西属于哪一层（坐标 / 红线 / 细则 / 流程 / 确定性检查），**不要新开第五层**；
2. frontmatter 与 settings 键名必须有 omp 文档依据（`read omp://<topic>.md`），臆造的键不会报错，
   只会静默失效；
3. 加完按上一节验证它真的被发现了 —— 写进文件不等于生效；
4. 能用确定性工具强制的规则，写成检查器或 lint，不要写成给模型看的散文。
