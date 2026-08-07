//! 三档符号集：`unicode` / `nerd` / `ascii`。
//!
//! 取值逐条移植自 oh-my-pi `packages/coding-agent/src/modes/theme/theme.ts`
//! （unicode 表 `:252-458`、nerd 表 `:461-769`、ascii 表 `:772-975`）。
//!
//! # 为什么是三张全量表而不是「基表 + 差异补丁」
//!
//! 上游刻意让三档各自独立调优：`box_round` 与 `tree` 在 nerd 档故意与 unicode
//! **完全相同**（`theme.ts:496-542` 的 "same as unicode"），因为 Nerd Font 并没有
//! 更好的画线字形。补丁式结构无法表达「这一档故意不改」。
//!
//! 代价是加一个符号要改三处；`Symbols` 是 struct 而非 map，漏一处直接编译失败——
//! 这正是上游用 `Record<SymbolKey, string>` 换到的同一份编译期保障。
//!
//! # 档位怎么选
//!
//! **没有自动探测。** 上游历史上按终端身份猜过 nerd font，已整体删除
//! （`theme.ts` 全文搜不到 `NERD_FONTS`，只剩 CHANGELOG 记录），现在由用户显式选。
//! 猜错的代价是满屏豆腐块，而终端字体这件事进程内没有任何可靠信号。
//!
//! # 宽度
//!
//! 符号值**不保证是单个字素**：`status.pending` 在 unicode 档是 `⏳`（宽 2），
//! `thinking.*` 含空格和英文单词，ascii 档的 `status.success` 是 `[ok]`（宽 4）。
//! 任何排版都必须走 [`zcode_text::width`]，绝不假设 1 列。

/// 盒绘字符：四角 + 横竖 + 五个交叉/T 型。
#[derive(Debug, Clone, Copy)]
pub struct BoxSymbols {
    /// 左上角。
    pub top_left: &'static str,
    /// 右上角。
    pub top_right: &'static str,
    /// 左下角。
    pub bottom_left: &'static str,
    /// 右下角。
    pub bottom_right: &'static str,
    /// 横线。
    pub horizontal: &'static str,
    /// 竖线。
    pub vertical: &'static str,
    /// 十字交叉。
    pub cross: &'static str,
    /// 向下的 T。
    pub tee_down: &'static str,
    /// 向上的 T。
    pub tee_up: &'static str,
    /// 向右的 T（左侧竖线上的分叉）。
    pub tee_right: &'static str,
    /// 向左的 T（右侧竖线上的分叉）。
    pub tee_left: &'static str,
}

/// 树形列表的连接线。
#[derive(Debug, Clone, Copy)]
pub struct TreeSymbols {
    /// 非末项分支。
    pub branch: &'static str,
    /// 末项分支。
    pub last: &'static str,
    /// 续行竖线。
    pub vertical: &'static str,
    /// 横线。
    pub horizontal: &'static str,
    /// 挂钩（单字符收尾）。
    pub hook: &'static str,
}

/// 状态图标。
#[derive(Debug, Clone, Copy)]
pub struct StatusSymbols {
    /// 成功。
    pub success: &'static str,
    /// 失败。
    pub error: &'static str,
    /// 警告。
    pub warning: &'static str,
    /// 提示。
    pub info: &'static str,
    /// 等待中。
    pub pending: &'static str,
    /// 已禁用。
    pub disabled: &'static str,
    /// 已启用。
    pub enabled: &'static str,
    /// 执行中。
    pub running: &'static str,
    /// 被遮蔽。
    pub shadowed: &'static str,
    /// 已中止。
    pub aborted: &'static str,
    /// 已完成。
    pub done: &'static str,
}

/// 导航图标。
#[derive(Debug, Clone, Copy)]
pub struct NavSymbols {
    /// 输入提示符 / 列表光标。
    pub cursor: &'static str,
    /// 选中标记。
    pub selected: &'static str,
    /// 可展开。
    pub expand: &'static str,
    /// 可折叠。
    pub collapse: &'static str,
    /// 返回。
    pub back: &'static str,
}

/// 分隔符。
#[derive(Debug, Clone, Copy)]
pub struct SepSymbols {
    /// 中点分隔（**自带两侧空格**）。
    pub dot: &'static str,
    /// 竖线分隔（自带两侧空格）。
    pub pipe: &'static str,
    /// 斜杠分隔（自带两侧空格）。
    pub slash: &'static str,
    /// 实心块。
    pub block: &'static str,
    /// powerline 细分隔（左向）。unicode 档**不是** powerline 字形而是 `>`。
    pub powerline_thin_left: &'static str,
    /// powerline 细分隔（右向）。
    pub powerline_thin_right: &'static str,
    /// powerline 粗分隔（左向）。
    pub powerline_left: &'static str,
    /// powerline 粗分隔（右向）。
    pub powerline_right: &'static str,
}

/// markdown 专用符号。
#[derive(Debug, Clone, Copy)]
pub struct MdSymbols {
    /// 引用块左侧竖条。
    pub quote_border: &'static str,
    /// 水平线字符。
    pub hr_char: &'static str,
    /// 无序列表符号。
    pub bullet: &'static str,
    /// 行内 hex 颜色前的色块。
    pub color_swatch: &'static str,
}

/// 通用排版符号。
#[derive(Debug, Clone, Copy)]
pub struct FormatSymbols {
    /// 通用项目符号。
    pub bullet: &'static str,
    /// 破折号。
    pub dash: &'static str,
    /// 徽章左括号。
    pub bracket_left: &'static str,
    /// 徽章右括号。
    pub bracket_right: &'static str,
    /// 省略标记。
    pub ellipsis: &'static str,
}

/// 勾选框与单选钮。
#[derive(Debug, Clone, Copy)]
pub struct ChoiceSymbols {
    /// 已勾选。
    pub checked: &'static str,
    /// 未勾选。
    pub unchecked: &'static str,
    /// 单选已选。
    pub radio_on: &'static str,
    /// 单选未选。
    pub radio_off: &'static str,
}

/// 思考强度档位的标签（**含空格与英文单词**，不是单字形）。
#[derive(Debug, Clone, Copy)]
pub struct ThinkingSymbols {
    /// minimal 档。
    pub minimal: &'static str,
    /// low 档。
    pub low: &'static str,
    /// medium 档。
    pub medium: &'static str,
    /// high 档。
    pub high: &'static str,
    /// xhigh 档。
    pub xhigh: &'static str,
    /// max 档。
    pub max: &'static str,
    /// auto 尚未定档。
    pub auto_pending: &'static str,
}

/// 工具图标。键名对齐本仓的工具注册表。
#[derive(Debug, Clone, Copy)]
pub struct ToolSymbols {
    /// 读文件。
    pub read: &'static str,
    /// 写文件。
    pub write: &'static str,
    /// 编辑文件。
    pub edit: &'static str,
    /// 执行 shell。
    pub bash: &'static str,
    /// 文本搜索。
    pub grep: &'static str,
    /// 路径匹配。
    pub glob: &'static str,
    /// 目录列举。
    pub ls: &'static str,
    /// 待办。
    pub todo: &'static str,
    /// 子任务。
    pub task: &'static str,
    /// 询问用户。
    pub ask: &'static str,
    /// 未知工具的兜底图标。
    pub generic: &'static str,
}

/// spinner 的两组帧序列。
#[derive(Debug, Clone, Copy)]
pub struct SpinnerFrames {
    /// 状态行/工具头用（较稳重）。
    pub status: &'static [&'static str],
    /// 活动指示用（较轻快）。
    pub activity: &'static [&'static str],
}

/// 一整档符号集。
#[derive(Debug, Clone, Copy)]
pub struct Symbols {
    /// 圆角框。UI 外框一律用它。
    pub box_round: BoxSymbols,
    /// 直角框。表格用它。
    pub box_sharp: BoxSymbols,
    /// 树线。
    pub tree: TreeSymbols,
    /// 状态图标。
    pub status: StatusSymbols,
    /// 导航图标。
    pub nav: NavSymbols,
    /// 分隔符。
    pub sep: SepSymbols,
    /// markdown 符号。
    pub md: MdSymbols,
    /// 排版符号。
    pub format: FormatSymbols,
    /// 勾选/单选。
    pub choice: ChoiceSymbols,
    /// 思考档位标签。
    pub thinking: ThinkingSymbols,
    /// 工具图标。
    pub tool: ToolSymbols,
    /// 输入框内的软件光标。
    pub input_cursor: &'static str,
    /// advisor 便签的左侧竖条。**刻意比 `md.quote_border` 粗**，让它读起来是
    /// 「另一个声音」（`theme.ts:382` 的注释）。
    pub advisor_rail: &'static str,
    /// spinner 帧。
    pub spinner: SpinnerFrames,
}

/// 符号档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolPreset {
    /// 通用 Unicode 几何符号。默认档：不需要特殊字体。
    #[default]
    Unicode,
    /// Nerd Font 私有区图标。需要用户装了 Nerd Font 补丁字体。
    Nerd,
    /// 纯 ASCII。任何终端都能显示，代价是符号变成 `[ok]` 这类多字符标签。
    Ascii,
}

impl SymbolPreset {
    /// 取该档的符号表。
    #[must_use]
    pub const fn symbols(self) -> &'static Symbols {
        match self {
            Self::Unicode => &UNICODE,
            Self::Nerd => &NERD,
            Self::Ascii => &ASCII,
        }
    }
}

/// `unicode` 档。
pub static UNICODE: Symbols = Symbols {
    box_round: BoxSymbols {
        top_left: "╭",
        top_right: "╮",
        bottom_left: "╰",
        bottom_right: "╯",
        horizontal: "─",
        vertical: "│",
        // 圆角没有对应的交叉字形，直接借直角档的（`theme.ts:1786-1793` 同一处理）。
        cross: "┼",
        tee_down: "┬",
        tee_up: "┴",
        tee_right: "├",
        tee_left: "┤",
    },
    box_sharp: BoxSymbols {
        top_left: "┌",
        top_right: "┐",
        bottom_left: "└",
        bottom_right: "┘",
        horizontal: "─",
        vertical: "│",
        cross: "┼",
        tee_down: "┬",
        tee_up: "┴",
        tee_right: "├",
        tee_left: "┤",
    },
    tree: TreeSymbols {
        branch: "├─",
        last: "└─",
        vertical: "│",
        horizontal: "─",
        hook: "└",
    },
    status: StatusSymbols {
        success: "✔",
        error: "✘",
        warning: "⚠",
        info: "ⓘ",
        pending: "⏳",
        disabled: "⦸",
        enabled: "●",
        running: "⟳",
        shadowed: "○",
        aborted: "⏹",
        done: "•",
    },
    nav: NavSymbols {
        cursor: "❯",
        selected: "➤",
        expand: "▸",
        collapse: "▾",
        back: "⟵",
    },
    sep: SepSymbols {
        dot: " · ",
        pipe: " │ ",
        slash: " / ",
        block: "▌",
        powerline_thin_left: ">",
        powerline_thin_right: "<",
        powerline_left: "▶",
        powerline_right: "◀",
    },
    md: MdSymbols {
        quote_border: "▏",
        hr_char: "─",
        bullet: "•",
        color_swatch: "■",
    },
    format: FormatSymbols {
        bullet: "•",
        dash: "—",
        bracket_left: "⟦",
        bracket_right: "⟧",
        ellipsis: "…",
    },
    choice: ChoiceSymbols {
        checked: "☑",
        unchecked: "☐",
        radio_on: "◉",
        radio_off: "○",
    },
    thinking: ThinkingSymbols {
        minimal: "○ min",
        low: "◔ low",
        medium: "◑ med",
        high: "◒ high",
        xhigh: "◕ xhigh",
        max: "◉ max",
        auto_pending: "⟳",
    },
    tool: ToolSymbols {
        read: "◇",
        write: "✎",
        edit: "✎",
        bash: "❯",
        grep: "⌕",
        glob: "⌕",
        ls: "▤",
        todo: "☑",
        task: "⇶",
        ask: "?",
        generic: "◈",
    },
    input_cursor: "▏",
    advisor_rail: "▎",
    spinner: SpinnerFrames {
        status: &["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"],
        activity: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    },
};

/// `nerd` 档。画线字符与 `unicode` 相同（Nerd Font 没有更好的），差异集中在图标。
pub static NERD: Symbols = Symbols {
    box_round: UNICODE.box_round,
    box_sharp: UNICODE.box_sharp,
    tree: UNICODE.tree,
    status: StatusSymbols {
        success: "\u{f00c}",
        error: "\u{f00d}",
        warning: "\u{f12a}",
        info: "\u{f129}",
        pending: "\u{f254}",
        disabled: "\u{f05e}",
        enabled: "\u{f111}",
        running: "\u{f110}",
        shadowed: "\u{f10c}",
        aborted: "\u{f04d}",
        done: "•",
    },
    nav: NavSymbols {
        cursor: "\u{f054}",
        selected: "\u{f178}",
        expand: "\u{f0da}",
        collapse: "\u{f0d7}",
        back: "\u{f060}",
    },
    sep: SepSymbols {
        dot: " · ",
        pipe: "\u{e0b3}",
        slash: "\u{e0bb}",
        block: "█",
        powerline_thin_left: "\u{e0b1}",
        powerline_thin_right: "\u{e0b3}",
        powerline_left: "\u{e0b0}",
        powerline_right: "\u{e0b2}",
    },
    md: MdSymbols {
        quote_border: "│",
        hr_char: "─",
        bullet: "\u{f111}",
        color_swatch: "■",
    },
    format: FormatSymbols {
        bullet: "\u{f111}",
        dash: "–",
        bracket_left: "⟨",
        bracket_right: "⟩",
        ellipsis: "…",
    },
    choice: ChoiceSymbols {
        checked: "\u{f14a}",
        unchecked: "\u{f096}",
        radio_on: "\u{f192}",
        radio_off: "\u{f10c}",
    },
    thinking: ThinkingSymbols {
        minimal: "\u{f0a9e} min",
        low: "\u{f0a9f} low",
        medium: "\u{f0aa1} med",
        high: "\u{f0aa3} high",
        xhigh: "\u{f0aa5} xhi",
        max: "\u{f06d} max",
        auto_pending: "\u{f074}",
    },
    tool: ToolSymbols {
        read: "\u{ea7b}",
        write: "\u{ea7f}",
        edit: "\u{ea73}",
        bash: "\u{ebca}",
        grep: "\u{eb01}",
        glob: "\u{eb01}",
        ls: "\u{ea83}",
        todo: "\u{eab3}",
        task: "\u{f4a0}",
        ask: "\u{eac7}",
        generic: "\u{eb2d}",
    },
    input_cursor: "▏",
    advisor_rail: "▎",
    spinner: SpinnerFrames {
        status: &[
            "\u{f11d6}",
            "\u{f11cb}",
            "\u{f11cc}",
            "\u{f11cd}",
            "\u{f11ce}",
            "\u{f11cf}",
            "\u{f11d0}",
            "\u{f11d1}",
            "\u{f11d2}",
            "\u{f11d3}",
            "\u{f11d4}",
            "\u{f11d5}",
        ],
        activity: UNICODE.spinner.activity,
    },
};

/// `ascii` 档：任何终端、任何字体都能显示。
pub static ASCII: Symbols = Symbols {
    box_round: BoxSymbols {
        top_left: "+",
        top_right: "+",
        bottom_left: "+",
        bottom_right: "+",
        horizontal: "-",
        vertical: "|",
        cross: "+",
        tee_down: "+",
        tee_up: "+",
        tee_right: "+",
        tee_left: "+",
    },
    box_sharp: BoxSymbols {
        top_left: "+",
        top_right: "+",
        bottom_left: "+",
        bottom_right: "+",
        horizontal: "-",
        vertical: "|",
        cross: "+",
        tee_down: "+",
        tee_up: "+",
        tee_right: "+",
        tee_left: "+",
    },
    tree: TreeSymbols {
        branch: "|--",
        last: "'--",
        vertical: "|",
        horizontal: "-",
        hook: "`-",
    },
    status: StatusSymbols {
        success: "[ok]",
        error: "[!!]",
        warning: "[!]",
        info: "[i]",
        pending: "[*]",
        disabled: "[ ]",
        enabled: "[x]",
        running: "[~]",
        shadowed: "[/]",
        aborted: "[-]",
        done: "*",
    },
    nav: NavSymbols {
        cursor: ">",
        selected: "->",
        expand: "+",
        collapse: "-",
        back: "<-",
    },
    sep: SepSymbols {
        dot: " - ",
        pipe: " | ",
        slash: " / ",
        block: "#",
        powerline_thin_left: ">",
        powerline_thin_right: "<",
        powerline_left: ">",
        powerline_right: "<",
    },
    md: MdSymbols {
        quote_border: "|",
        hr_char: "-",
        bullet: "*",
        color_swatch: "[]",
    },
    format: FormatSymbols {
        bullet: "*",
        dash: "-",
        bracket_left: "[",
        bracket_right: "]",
        ellipsis: "...",
    },
    choice: ChoiceSymbols {
        checked: "[x]",
        unchecked: "[ ]",
        radio_on: "(o)",
        radio_off: "( )",
    },
    thinking: ThinkingSymbols {
        minimal: "[min]",
        low: "[low]",
        medium: "[med]",
        high: "[high]",
        xhigh: "[xhi]",
        max: "[max]",
        auto_pending: "[~]",
    },
    tool: ToolSymbols {
        read: "r",
        write: "+f",
        edit: "~",
        bash: "$",
        grep: "/",
        glob: "/",
        ls: "ls",
        todo: "[x]",
        task: ">>>",
        ask: "[?]",
        generic: "<>",
    },
    input_cursor: "|",
    advisor_rail: "|",
    spinner: SpinnerFrames {
        status: &["|", "/", "-", "\\"],
        activity: &["-", "\\", "|", "/"],
    },
};

#[cfg(test)]
mod tests {
    use super::*;
    use zcode_text::width::visible_width;

    #[test]
    fn ascii_preset_is_pure_ascii() {
        // ascii 档的存在意义就是「任何终端都能显示」，混进一个非 ASCII 字符就失效了。
        let s = SymbolPreset::Ascii.symbols();
        for value in [
            s.box_round.top_left,
            s.box_sharp.cross,
            s.tree.branch,
            s.status.success,
            s.nav.cursor,
            s.sep.dot,
            s.md.quote_border,
            s.format.bracket_left,
            s.choice.checked,
            s.thinking.max,
            s.tool.bash,
            s.input_cursor,
            s.advisor_rail,
        ] {
            assert!(value.is_ascii(), "ascii 档出现非 ASCII 值：{value:?}");
        }
        for frame in s.spinner.status.iter().chain(s.spinner.activity) {
            assert!(frame.is_ascii(), "ascii spinner 帧非 ASCII：{frame:?}");
        }
    }

    #[test]
    fn spinner_frames_are_uniform_width_within_a_preset() {
        // 帧宽不一致会让 spinner 旁边的文字每帧左右抖动，是肉眼可见的缺陷。
        for preset in [
            SymbolPreset::Unicode,
            SymbolPreset::Nerd,
            SymbolPreset::Ascii,
        ] {
            let s = preset.symbols();
            for frames in [s.spinner.status, s.spinner.activity] {
                let first = visible_width(frames.first().copied().unwrap_or(""));
                assert!(!frames.is_empty(), "{preset:?} 的 spinner 帧序列为空");
                for frame in frames {
                    assert_eq!(
                        visible_width(frame),
                        first,
                        "{preset:?} 的 spinner 帧宽不一致：{frame:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn box_edges_are_single_column() {
        // 边框字符宽度必须为 1，否则框线右边缘会错位——这是三档都要守住的。
        for preset in [
            SymbolPreset::Unicode,
            SymbolPreset::Nerd,
            SymbolPreset::Ascii,
        ] {
            let s = preset.symbols();
            for b in [s.box_round, s.box_sharp] {
                for ch in [
                    b.top_left,
                    b.top_right,
                    b.bottom_left,
                    b.bottom_right,
                    b.horizontal,
                    b.vertical,
                    b.cross,
                    b.tee_down,
                    b.tee_up,
                    b.tee_right,
                    b.tee_left,
                ] {
                    assert_eq!(
                        visible_width(ch),
                        1,
                        "{preset:?} 的框线字符 {ch:?} 不是 1 列"
                    );
                }
            }
        }
    }

    #[test]
    fn advisor_rail_differs_from_quote_border_where_glyphs_allow() {
        // 上游刻意让 advisor 的竖条比引用块粗，好让它读起来是「另一个声音」。
        // ascii 档没有粗细可选，两者相同是预期的。
        let u = SymbolPreset::Unicode.symbols();
        assert_ne!(u.advisor_rail, u.md.quote_border);
        let a = SymbolPreset::Ascii.symbols();
        assert_eq!(a.advisor_rail, a.md.quote_border);
    }
}
