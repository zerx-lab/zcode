# ZCode 审查关注点

本项目**规范层与 Rust workspace 并存**：`crates/` 下已有十个成员在逐个填实现，
`.omp/` 与 `AGENTS.md` 是协作规范层。两侧都审，判据不同 —— 规范看自洽，代码看域约束。

优先盯这些：

- **索引膨胀**：`AGENTS.md` 超过 120 行，或把 clippy / rustfmt / 测试能强制的规则写了进去。
  正确处置是搬进 `.omp/rules/`，不是压缩措辞。
- **过时坐标**：`AGENTS.md` 里反引号路径不存在且该行没有 `(planned)` 标记；或命令写法与
  `.omp/rules/`、`.omp/config.yml` 不一致。索引让路于代码，不是反过来。
- **约定分叉**：同一件事在 `AGENTS.md`、`.omp/RULES.md`、`.omp/rules/*` 里各写一遍且措辞不同。
  其中一种会**静默失效**：always-apply 规则（`RULES.md`）的内容若已出现在 context file
  （本项目即 `AGENTS.md`）中，会被去重逻辑跳过注入。按需读取的 `rules/*` 不参与该去重，
  但散落多份仍会改一处漏一处。
- **配置踩坑**：往 `.omp/config.yml` 写数组键（`disabledProviders` / `enabledModels` / `cycleOrder` /
  `modelProviderOrder` / `skills.customDirectories` / `bashInterceptor.patterns`）会整表覆盖用户级设置。
- **臆造 schema**：rule frontmatter 只有 `description` / `globs` / `alwaysApply` / `condition` /
  `astCondition` / `scope` / `interruptMode`；agent frontmatter、settings 键名、hook 事件名同理。
  任何没有 omp 文档依据的键名一律指出，别放过 "看起来像真的" 的字段。
- **虚假验证**：说"已验证"却没写出跑了什么命令、看到什么输出；声称跑过 `cargo` / `nextest`
  但产物或报错与命令不匹配；声称 extension 已生效却没有新会话验证过加载。

代码侧只审这些 clippy 管不了、只有 `AGENTS.md` / `rule://zcode-architecture` 记着的域约束：

- worker 子进程是否重入 CLI 入口，而不是新增独立 `[[bin]]` 目标；
- prompt 是否被拼在代码里，而非静态 `.md` + Handlebars；
- 生成物是否被手改，而非改生成器源头；
- 渲染路径是否遗漏**错误消息**与**流式预览**两条分支（只修成功路径不算修好）；
- 库代码是否漏出 `unwrap` / `expect` / `panic!` / `as` 数值转换；
- 显示宽度 / 输出截断 / 路径脱敏是否绕过 `crates/text/` 的唯一实现，就地自己算；
- 是否退回 `AGENTS.md` 已记的反悔点：Windows 上先探活再连 named pipe、凭据文件无锁整文件重写、
  turn 结束不 reset `InterruptSignal`、TUI 发绝对 `MoveTo`、schema 校验降级成跳过约束；
- 是否在既有约定旁并行造第二套写法，而没有先迁调用点再删旧路径。
