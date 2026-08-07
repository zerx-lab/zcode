//! 工具输出的统一收尾：本 crate 里唯一允许把字符串包装成 [`ToolOutput`] / [`ToolError`] 的
//! 地方，其余代码一律经 [`finish`] / [`error`]，不直接构造。
//!
//! # 为什么每一条渲染路径都必须过这里，包括错误消息
//!
//! `rule://zcode-architecture`「TUI 输出清理」一节把要求定成了不变量：制表符要展开、长行
//! 要按显示宽度截断、总量要有行数/字节上限——而且**这适用于每一条渲染路径，不只是成功
//! 路径**。原因很直接：
//!
//! - 工具结果既会进 TUI 的等宽网格，也会原样喂回模型下一轮上下文。裸制表符在网格里是
//!   视觉空洞；未封顶的输出（比如 `grep` 命中几千行、`bash` 打印一个几十 MB 的日志）会
//!   把一次工具调用的 token 成本炸到失控，`turn` 循环的压缩预算是按"工具输出有合理上限"
//!   这个前提设计的。
//! - `ToolError::Failed` 的文本会原样喂回模型（`crates/agent/src/turn.rs:337-355`）。patch
//!   失败消息经常带着"未匹配到的原始文件行"，那些行和成功路径的文件内容一样可能带制表符、
//!   超宽行、甚至是攻击者故意拼出来的几万字符长串——错误路径没有理由比成功路径更放纵。
//!
//! 因此本模块不区分"成功文案"和"失败文案"，两条入口 [`finish`] 与 [`error`] 底层共用同一个
//! [`clean`]，任何新增的产出路径都必须经过其中之一，不允许在别处手搓 `ToolOutput::text` /
//! `ToolError::Failed` 绕开清理。

use zcode_agent::{ToolError, ToolOutput};
use zcode_text::{TruncateLimits, Truncated, cap_columns, expand_tabs, truncate_head};

/// 对一段即将回给模型/落进终端的文本做统一清理：
///
/// 1. 制表符按对齐规则展开成空格（[`expand_tabs`]）——不能保留原样，等宽网格里会挖出空洞；
/// 2. 每行按显示宽度截断到 [`TruncateLimits::max_columns`]（[`cap_columns`]），必须经
///    `unicode-width`，不能按字节或字符数量截断，否则 CJK/emoji 内容会在错误的位置断开；
/// 3. 整体按行数/字节数封顶（[`truncate_head`]），保留头部——工具输出通常是"越靠前越关键"
///    （文件从头读、命令从头打印横幅），截尾部更符合直觉；
/// 4. 触顶时追加一行明示，见 [`truncation_notice`]。
///
/// 步骤顺序不能颠倒：必须先展开制表符、按列裁到位，再算行/字节预算——否则未展开的制表符
/// 或未裁剪的超宽行会让字节预算的估算失真，可能多丢或少丢行。
fn clean(body: &str) -> String {
    let limits = TruncateLimits::default();
    let expanded = expand_tabs(body);
    let capped = cap_columns(&expanded, limits.max_columns);
    let Truncated {
        mut text,
        dropped_lines,
        dropped_bytes,
        ..
    } = truncate_head(&capped, &limits);
    if dropped_lines > 0 || dropped_bytes > 0 {
        text.push_str(&truncation_notice(&limits, dropped_lines, dropped_bytes));
    }
    text
}

/// 截断后追加的明示行：不只报告"被截断了"，还要给模型一个能照做的下一步。
///
/// 措辞的参照系是 oh-my-pi `packages/coding-agent/src/tools/read.ts:2900-2902`——那条消息
/// 之所以有效，是因为它给了具体可执行的续读语法（`Use :<N> to continue`）。但本函数是全部
/// 八个工具共用的兜底安全网，跑在每个工具自己的收尾逻辑之后；`read` 这类有天然续读游标
/// （行偏移量）的工具应该在自己的模块里用更具体的话术处理截断，只有兜底再走到这里时，才
/// 只能给通用建议（缩小请求范围），不能编造一个所有工具都不支持的具体语法。
fn truncation_notice(
    limits: &TruncateLimits,
    dropped_lines: usize,
    dropped_bytes: usize,
) -> String {
    format!(
        "\n\n[Output truncated: {dropped_lines} more line(s) ({dropped_bytes} bytes) omitted to \
stay under the {max_lines}-line / {max_bytes}-byte cap. Narrow the request (a more specific path, \
pattern, or range) and re-run the tool to see the rest.]",
        max_lines = limits.max_lines,
        max_bytes = limits.max_bytes,
    )
}

/// 工具输出的统一收尾。所有工具的成功产出（含"零结果"这类没有长期价值的成功）都经这个
/// 函数构造 [`ToolOutput`]，不直接调用 `ToolOutput::text`。
#[must_use]
// `body: String` 是跨模块契约固定签名（contract.md §3），八个工具调用点都是刚拼出一个
// 新 String 就地传入，不存在"改成 &str 更省一次分配"的场景；`clippy::needless_pass_by_value`
// 只看到函数体内只借用，看不到"调用方本就该转让所有权"这层调用惯例，故显式豁免。
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn finish(body: String, title: impl Into<String>) -> ToolOutput {
    ToolOutput::text(clean(&body)).with_title(title)
}

/// 工具失败文案的统一收尾。同样过 [`clean`]，但不标 `useless`——失败的原因模型往往还要
/// 依据它改道重试，不是一条用完即弃的结果。
#[must_use]
pub(crate) fn error(message: impl Into<String>) -> ToolError {
    ToolError::Failed(clean(&message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcode_agent::StoredToolResultContent;
    use zcode_text::visible_width;

    /// 从 [`ToolOutput`] 里取出唯一的文本块，测试专用的小型断言辅助函数。
    fn text_of(output: &ToolOutput) -> &str {
        match output.content.first() {
            Some(StoredToolResultContent::Text { text }) => text.as_str(),
            other => panic!("预期单个文本内容块，实际是 {other:?}"),
        }
    }

    #[test]
    fn tabs_are_replaced_with_spaces() {
        let output = finish("a\tb\tc".to_owned(), "t");
        let text = text_of(&output);
        assert!(!text.contains('\t'), "制表符应已被展开：{text:?}");
        assert!(
            text.starts_with("a "),
            "首个制表符应展开成对齐空格：{text:?}"
        );
    }

    #[test]
    fn wide_lines_are_truncated_by_display_width_not_bytes() {
        // "测" 显示宽度 2、UTF-8 编码 3 字节；重复 600 次得到显示宽度 1200 的单行，
        // 远超默认 512 列上限，但整体字节数（1800）远低于默认 50_000 字节上限，
        // 因此本用例只触发列截断，不触发行/字节截断。
        let wide_line = "测".repeat(600);
        let output = finish(wide_line, "t");
        let text = text_of(&output);
        let mut lines = text.lines();
        let capped = lines.next().expect("至少应保留一行");
        assert!(lines.next().is_none(), "单行输入不应被拆成多行");

        let width = visible_width(capped);
        assert!(width <= 512, "截断后显示宽度 {width} 超过 512 列上限");
        // 若按字节而不是显示宽度截断，512 字节预算下最多只能保留 ~170 个三字节字符
        // （显示宽度 ~340），与按显示宽度截断（应逼近 512 列）差出一大截，足以区分两种实现。
        assert!(
            width > 480,
            "按显示宽度截断应逼近列上限，实际只有 {width} 列"
        );
        assert!(capped.ends_with('…'), "触顶的行应以省略号收尾：{capped:?}");
    }

    #[test]
    fn wide_emoji_lines_are_truncated_by_display_width_not_bytes() {
        // "🙂" 显示宽度 2、UTF-8 编码 4 字节（单码点 emoji，非 ZWJ 序列，宽度求和走
        // 单码点快路径）；重复 600 次同样得到远超 512 列上限、但字节数远低于
        // 50_000 上限的单行，专门覆盖 CJK 之外的多字节字符类别。
        let emoji_line = "🙂".repeat(600);
        let output = finish(emoji_line, "t");
        let text = text_of(&output);
        let mut lines = text.lines();
        let capped = lines.next().expect("至少应保留一行");
        assert!(lines.next().is_none(), "单行输入不应被拆成多行");

        let width = visible_width(capped);
        assert!(width <= 512, "截断后显示宽度 {width} 超过 512 列上限");
        assert!(capped.ends_with('…'), "触顶的行应以省略号收尾：{capped:?}");
        let content_bytes = capped.trim_end_matches('…').len();
        assert_eq!(
            content_bytes % 4,
            0,
            "每个 emoji 占 4 字节，截断必须停在完整字符边界上，不能切碎：{capped:?}"
        );
    }

    #[test]
    fn excess_lines_are_capped_with_an_explicit_notice() {
        let body = "x\n".repeat(3005);
        let output = finish(body, "t");
        let text = text_of(&output);

        assert!(
            text.contains("[Output truncated:"),
            "超行数应追加明示行：{text:?}"
        );
        assert!(
            text.lines().count() < 3005,
            "截断后行数应少于原始行数，实际 {} 行",
            text.lines().count()
        );
        assert!(
            text.lines().take(3000).all(|line| line == "x"),
            "保留下来的应是原始头部内容，未被篡改"
        );
    }

    #[test]
    fn untouched_input_is_returned_byte_for_byte() {
        let body = "short line\nanother line\n".to_owned();
        let output = finish(body.clone(), "t");
        assert_eq!(text_of(&output), body, "未超限时不应产生任何字节差异");
    }

    #[test]
    fn error_messages_are_cleaned_the_same_way() {
        let ToolError::Failed(text) = error("bad\tpath\tvalue") else {
            unreachable!("error() 恒定返回 ToolError::Failed");
        };
        assert!(!text.contains('\t'), "错误文案的制表符也应被清理：{text:?}");
    }
}
