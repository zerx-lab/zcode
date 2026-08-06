# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]
### Breaking Changes

- 项目许可证从 `MIT OR Apache-2.0` 切换为 `AGPL-3.0-only`。

### Added

- 初始化 crate 骨架：继承 workspace 元数据与 lint 配置。
- `CompiledSchema` / `SchemaCache`：draft 2020-12 子集的自研单趟校验器，惰性编译 + 按内容哈希缓存。
  自研而非引 `jsonschema` 的理由与上游一致（oh-my-pi
  `packages/ai/src/utils/schema/json-schema-validator.ts:13-14`）：单趟、同步、容忍 LLM 写出的
  非标形状（`nullable`、`2.0` 当 integer）。缓存 key 是**内容哈希**而非对象身份——上游用身份做 key，
  schema 就地改写后缓存不失效，是已记录的技术债。
- `ValidationIssue` / `ValidationError` / `render_validation_error`：回给模型的错误文本形状
  （`root` 根路径、`/` 连接的段、单字段截断到 256 字符）是可观察契约，逐字对齐上游。
  上游 `truncateArgsForError` 递归无深度上限，本仓加了 `MAX_TRUNCATE_DEPTH = 32`。

### Notes

- **fail-closed**：`compile` 会校验每一个已实现 keyword 的形状，非法即
  `SchemaError::InvalidKeyword`；`regex` crate 编译不了的 `pattern`（lookaround、反向引用）
  同样在编译期报错，绝不降级成"跳过该约束"。校验器的价值在于"说通过就是真通过"，
  宁可拒绝一个支持不了的 schema，也不给假阳性。
- `unevaluatedProperties` / `unevaluatedItems` 未实现，按宽松处理，但会经
  `unsupported_keywords()` 显式上报——上游只在进程内 warn 一次，调用方观测不到，不继承该缺口。

### Fixed

- rustdoc 在 `-D warnings` 下报的文档链接问题：`error.rs` 指向私有 `MAX_REF_DEPTH` 的
  intra-doc 链接降级为代码 span，`lib.rs` 模块表里 6 处 `redundant_explicit_links` 改为
  显示文本自带路径。
