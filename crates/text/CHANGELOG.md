# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]
### Breaking Changes

- 项目许可证从 `MIT OR Apache-2.0` 切换为 `AGPL-3.0-only`。

### Added

- 初始化 crate 骨架：继承 workspace 元数据与 lint 配置。
- `width`：ANSI 感知的显示宽度、按宽度截断（不超限时返回 `Cow::Borrowed`）、硬换行、
  列↔字节映射、制表符展开、ANSI 剥离、控制字符清洗。多码点 grapheme 簇整簇交给
  `UnicodeWidthStr`，不逐字符求和——jcode `crates/jcode-tui/src/tui/ui/display_width.rs:1-19`
  逐 char 累加会把 ZWJ 家庭 emoji 算成 8 列（实际 2 列），那是债，不抄。
- `truncate`：工具输出的统一截断（3000 行 / 50 KB / 512 列）、`enforce_inline_byte_cap`
  的 60/25/15 预算分配、有状态的 `OutputSink`（头窗 + 滚动尾窗，跨 chunk 保持每行列上限状态，
  一行只补一个省略号，列裁量单独计账不算进中段省略）。
- `path::shorten_path`：主目录 → `~`。Windows 分支按大小写不敏感比较（`c:/users/alice` 与
  `C:/Users/Alice` 必须匹配，否则主目录原样泄露），两个平台都做分隔符边界校验，
  `/home/foo` 不会误匹配 `/home/foobar`。
- `grep`：进程内 ripgrep 引擎（`grep-searcher` + `grep-regex` + `ignore` + `rayon`），
  不 fork `rg` 子进程——jcode 走 fork 且全链路无超时无取消，那是债。支持墙钟超时与外部
  取消标志、超大文件前缀窗、非法正则两级回退（并在 `GrepOutcome::matcher` 回报实际用的种类，
  上游静默切方言、调用方无从得知）。
- `image`：出站图像的解码 → 缩放 → 重编码流水线，多编码器竞速取最小，
  两阶段预算收敛（先降质量再降尺寸）。

### Notes

- **单行截断只有一套，按显示宽度。** 上游同一个 512 喂给两套实现——native 侧按字节
  （oh-my-pi `crates/pi-natives/src/grep.rs:325-334`）、JS 侧按字符
  （`packages/coding-agent/src/session/streaming-output.ts:266-270`）——CJK 下截断位置差三倍。
- **二进制早停会被上报。** `BinaryDetection::quit(0)` 的真实语义是"截断在第一个 NUL"而非
  "跳过二进制文件"（UTF-16 文本会在第 2 字节停），上游对此无任何告知，本仓记入
  `GrepOutcome::stopped_on_binary`。
- **grep 的 `max_files` 只约束有命中的结果文件。** 若拿它限制"已搜索文件数"，前 20 个
  普通文件不命中就会停止，后面的真实匹配永远看不到——那不是截断，是错误结果。
- **图像解码有像素上限。** 输入字节上限挡不住解压炸弹（几十 KB 的 PNG 可声明 50000×50000），
  故在解码前按 `MAX_PIXELS` 拒绝，并用 `ImageReader::limits` 的 `max_alloc` 作第二道防线。
- **尺度梯地板是 `min_dimension`（200px），不是上游的 100px。** 上游自己论证了 <200px 会被
  vision 后端硬拒并污染整个请求，却把地板设在 100px，两处未协调。

### Fixed

- rustdoc 在 `-D warnings` 下报的文档链接问题：`image.rs` / `width.rs` 中指向私有
  `MAX_INPUT_BYTES` / `MAX_PIXELS` / `grapheme_width` 等项的 intra-doc 链接降级为代码 span；
  `lib.rs` 模块表里的 `grep` 用 `[mod@grep]` 消歧（它同时是模块与被 glob 再导出的函数）。
