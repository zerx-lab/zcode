---
description: 写入 .rs 时命中 panic 路径或 as 数值转换时自动注入的替代方案。
condition: '(\.unwrap\(\)|\.expect\(|\bpanic!\(|\bunimplemented!\(|\btodo!\(|\bas\s+(usize|isize|u8|u16|u32|u64|u128|i8|i16|i32|i64|i128|f32|f64)\b)'
scope: "tool:edit(**/*.rs), tool:write(**/*.rs)"
interruptMode: tool-only
---

<!-- 正则意图：命中 .unwrap()、.expect(、panic!(、unimplemented!(、todo!(，以及 `as <数值类型>` 转换；
     刻意用 \bas\b 排除 as_str/as_ref/as_bytes 等标识符（下划线属单词字符，无边界）。 -->

# 禁用写法 → 替代方案

- `.unwrap()` → `?` 向上传播，配合 `thiserror`（库）；调用点无 `?` 语境时用 `.expect("<invariant>")` 并写清不变量。
- `.expect(...)`（非不变量场景）→ `?` + 具体错误类型，或 `.context("...")`（二进制/`anyhow`）。
- `panic!(...)` → 返回 `Err(YourError::Variant { .. })`，让调用方决定如何处理。
- `todo!()`/`unimplemented!()` → 要么补全实现，要么返回 `Err(...)` 明确“暂不支持”，不要留运行时炸弹。
- `x as <数值类型>` → `<类型>::try_from(x)?` 或 `x.try_into()?`，处理溢出/精度损失。

确属例外（内部不变量、确定无损转换）时，必须在**同一行**写出理由注释，例如 `expect("items 已在上方校验非空")`。测试代码不受本规则限制。

已知限制（正则/glob 原语决定，不是配置疏忽）：无法判断命中位置是否在 `#[cfg(test)]` 内，
所以测试里合法的 `unwrap()` 也会触发一次；TTSR 默认每会话只注入一次，落盘后的持续保障靠
`[workspace.lints.clippy]` 里的 `unwrap_used` / `as_conversions` 等 `deny`。
