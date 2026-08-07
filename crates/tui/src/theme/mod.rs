//! 主题：颜色调色板、符号档位、色深降级。
//!
//! 视觉取值全量对标 oh-my-pi（`packages/coding-agent/src/modes/theme/`）。
//!
//! # 与上游最重要的三处偏离
//!
//! 1. **不预先烘焙 ANSI 字符串。** 上游在构造主题时就把每个颜色拼成
//!    `\x1b[38;2;…m`（`theme.ts:1509-1536`），运行期只做字符串拼接。代价写在它
//!    自己的代码里：`getContrastFgAnsi`（`theme.ts:1669-1675`）不得不用正则从烘好的
//!    字符串里把 RGB 抠回来，256 色档抠不出来就丢失对比度保障。本仓存 ratatui 的
//!    [`Color`]，[`Style`] 由调用点组装，颜色值全程可读。
//! 2. **`dim` 是颜色，不是 [`Modifier::DIM`]。** 上游 `colors.dim` 是具体色值
//!    `#5f6673`（`dark.json:11,30`），全仓只有 diff 的缩进可视化用真 SGR 2（因为它
//!    需要「叠加在当前行色之上」的加性效果）。把 `dim` 映射成 `Modifier::DIM` 会
//!    同时丢掉颜色又叠上修饰符，两头不讨好。
//! 3. **不做全局单例。** 上游是 `export var theme`（`theme.ts:2178`）＋每个 getter
//!    手写 `typeof theme === "undefined"` 守卫，漏写一个就是线上崩溃（它的 issue
//!    #2998）。本仓把 `&Theme` 当参数传，「预初始化」这个状态从类型上就不存在。
//!
//! # 颜色的两层间接
//!
//! 主题 JSON 分 `vars`（自由命名）与 `colors`（固定键集）两段，`colors` 的值可以是
//! 一个 `vars` 键名。好处是主题作者改一处 `vars.accent` 就能带动所有引用它的槽位。
//! 解析规则见 [`color`] 模块文档。

pub mod color;
pub mod symbols;

use ratatui::style::{Color, Modifier, Style};

pub use crate::theme::color::{ColorError, ColorMode, RawColor, Rgb, Vars};
pub use crate::theme::symbols::{SymbolPreset, Symbols};

/// 由一份 JSON `colors` 段的字段清单，同时生成：
///
/// - `Raw…`：serde 反序列化目标，字段全是 [`RawColor`]；
/// - 前景/背景两个解析后的调色板 struct，字段全是 [`Color`]。
///
/// 上游用一个硬编码的 7 元素 Set 在运行期把 `colors` 切成 fg / bg 两半
/// （`theme.ts:2095-2110`）；这里在声明处切，切错就是编译错误。
///
/// 必填键缺失由 serde 直接报错。**刻意不做「缺键回落终端默认色」**：静默降级会让
/// 用户看到「这主题坏了」却拿不到任何诊断信息。
macro_rules! theme_palette {
    (
        $(#[$raw_attr:meta])* $raw:ident;
        $(#[$fg_attr:meta])* $fg:ident {
            $($(#[$fa:meta])* $ff:ident => $fj:literal,)*
        }
        $(#[$bg_attr:meta])* $bg:ident {
            $($(#[$ba:meta])* $bf:ident => $bj:literal,)*
        }
    ) => {
        $(#[$raw_attr])*
        #[derive(Debug, Clone, serde::Deserialize)]
        pub struct $raw {
            $($(#[$fa])* #[serde(rename = $fj)] pub $ff: RawColor,)*
            $($(#[$ba])* #[serde(rename = $bj)] pub $bf: RawColor,)*
            /// 思考档位边框：max。**唯一可选键**，缺省时回落 `thinkingXhigh`。
            #[serde(default, rename = "thinkingMax")]
            pub thinking_max: Option<RawColor>,
        }

        $(#[$fg_attr])*
        #[derive(Debug, Clone)]
        pub struct $fg {
            $($(#[$fa])* pub $ff: Color,)*
        }

        $(#[$bg_attr])*
        #[derive(Debug, Clone)]
        pub struct $bg {
            $($(#[$ba])* pub $bf: Color,)*
        }

        impl $raw {
            /// 解析全部槽位。返回值第三项是 `statusLineBg` 的感知亮度（空串时
            /// 为 `None`），亮暗判据要它，不值得为此单开一条解析路径。
            fn resolve(
                &self,
                vars: &Vars,
                mode: ColorMode,
            ) -> Result<($fg, $bg, Option<f32>), ColorError> {
                let fg = $fg {
                    $($ff: color::resolve($fj, &self.$ff, vars)?.to_color(mode),)*
                };
                let mut status_line_luma = None;
                let bg = $bg {
                    $($bf: {
                        let rgb = color::resolve($bj, &self.$bf, vars)?;
                        if $bj == "statusLineBg" {
                            status_line_luma =
                                rgb.components().map(|(r, g, b)| color::luma(r, g, b));
                        }
                        rgb.to_color(mode)
                    },)*
                };
                Ok((fg, bg, status_line_luma))
            }
        }
    };
}

theme_palette! {
    /// 主题 JSON 的 `colors` 段，尚未解析 `vars` 引用。
    RawColors;

    /// 解析后的前景色。
    Palette {
        /// 主强调色：logo、选中项、光标、工具标题。
        accent => "accent",
        /// 普通边框。
        border => "border",
        /// 高亮边框。
        border_accent => "borderAccent",
        /// 弱边框：工具卡片外框用它，不抢正文。
        border_muted => "borderMuted",
        /// 成功态。
        success => "success",
        /// 错误态。
        error => "error",
        /// 警告态。
        warning => "warning",
        /// 次级文本。
        muted => "muted",
        /// 极淡文本，比 `muted` 更弱。**是颜色，不是 SGR 2。**
        dim => "dim",
        /// 默认正文色。内置主题里是空串，即终端默认前景。
        text => "text",
        /// 思考内容文本。
        thinking_text => "thinkingText",
        /// 用户消息文本。
        user_message_text => "userMessageText",
        /// hook 注入消息的文本。
        custom_message_text => "customMessageText",
        /// hook 注入消息的类型标签。
        custom_message_label => "customMessageLabel",
        /// 工具卡片标题。
        tool_title => "toolTitle",
        /// 工具输出正文。
        tool_output => "toolOutput",
        /// markdown 标题。
        md_heading => "mdHeading",
        /// markdown 链接文字。
        md_link => "mdLink",
        /// markdown 链接 URL。
        md_link_url => "mdLinkUrl",
        /// markdown 行内代码。
        md_code => "mdCode",
        /// markdown 代码块正文（无语法高亮时的单色兜底）。
        md_code_block => "mdCodeBlock",
        /// markdown 代码围栏。
        md_code_block_border => "mdCodeBlockBorder",
        /// markdown 引用正文。
        md_quote => "mdQuote",
        /// markdown 引用竖条。
        md_quote_border => "mdQuoteBorder",
        /// markdown 水平线。
        md_hr => "mdHr",
        /// markdown 列表符号与序号。
        md_list_bullet => "mdListBullet",
        /// diff 新增行。**当前景用**，不铺整行背景。
        diff_added => "toolDiffAdded",
        /// diff 删除行。
        diff_removed => "toolDiffRemoved",
        /// diff 上下文行。
        diff_context => "toolDiffContext",
        /// 语法：注释。
        syntax_comment => "syntaxComment",
        /// 语法：关键字。
        syntax_keyword => "syntaxKeyword",
        /// 语法：函数名。
        syntax_function => "syntaxFunction",
        /// 语法：变量名。
        syntax_variable => "syntaxVariable",
        /// 语法：字符串。
        syntax_string => "syntaxString",
        /// 语法：数字。
        syntax_number => "syntaxNumber",
        /// 语法：类型名。
        syntax_type => "syntaxType",
        /// 语法：操作符。
        syntax_operator => "syntaxOperator",
        /// 语法：标点。
        syntax_punctuation => "syntaxPunctuation",
        /// 思考档位边框：off。
        thinking_off => "thinkingOff",
        /// 思考档位边框：minimal。
        thinking_minimal => "thinkingMinimal",
        /// 思考档位边框：low。
        thinking_low => "thinkingLow",
        /// 思考档位边框：medium。
        thinking_medium => "thinkingMedium",
        /// 思考档位边框：high。
        thinking_high => "thinkingHigh",
        /// 思考档位边框：xhigh。
        thinking_xhigh => "thinkingXhigh",
        /// bash 模式下的输入框边框。
        bash_mode => "bashMode",
        /// python 模式下的输入框边框。
        python_mode => "pythonMode",
        /// 状态行分隔符。
        status_line_sep => "statusLineSep",
        /// 状态行：模型段。
        status_line_model => "statusLineModel",
        /// 状态行：路径段。
        status_line_path => "statusLinePath",
        /// 状态行：git 干净。
        status_line_git_clean => "statusLineGitClean",
        /// 状态行：git 有改动。
        status_line_git_dirty => "statusLineGitDirty",
        /// 状态行：上下文用量段。
        status_line_context => "statusLineContext",
        /// 状态行：花费段。
        status_line_spend => "statusLineSpend",
        /// 状态行：git staged 计数。
        status_line_staged => "statusLineStaged",
        /// 状态行：git 未暂存计数。
        status_line_dirty => "statusLineDirty",
        /// 状态行：git 未跟踪计数。
        status_line_untracked => "statusLineUntracked",
        /// 状态行：输出 token 段。
        status_line_output => "statusLineOutput",
        /// 状态行：成本段。
        status_line_cost => "statusLineCost",
        /// 状态行：子 agent 段。
        status_line_subagents => "statusLineSubagents",
    }

    /// 解析后的背景色。
    Backgrounds {
        /// 列表选中项的背景（仅鼠标悬停用；键盘选中靠整行换 `accent` 前景）。
        selected => "selectedBg",
        /// 用户消息气泡背景。
        user_message => "userMessageBg",
        /// hook 注入消息的背景。
        custom_message => "customMessageBg",
        /// 工具卡片：执行中。
        tool_pending => "toolPendingBg",
        /// 工具卡片：成功。
        tool_success => "toolSuccessBg",
        /// 工具卡片：失败。
        tool_error => "toolErrorBg",
        /// 状态行背景。**亮暗判据取自这里**，见 [`Theme::is_light`]。
        status_line => "statusLineBg",
    }
}

/// 主题文件的顶层结构。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ThemeFile {
    /// 主题名。
    pub name: String,
    /// 自由命名的颜色变量。
    #[serde(default)]
    pub vars: Vars,
    /// 颜色段（前景 + 背景在 JSON 里同属一个对象）。
    pub colors: RawColors,
}

/// 解析完成、可直接用于渲染的主题。
#[derive(Debug, Clone)]
pub struct Theme {
    /// 主题名。
    pub name: String,
    /// 前景色。
    pub colors: Palette,
    /// 背景色。
    pub bg: Backgrounds,
    /// 符号档位对应的符号表。
    pub symbols: &'static Symbols,
    /// 构造时判定的色深。
    pub mode: ColorMode,
    /// 思考档位边框：max（已应用 `thinkingXhigh` 回落）。
    pub thinking_max: Color,
    /// `statusLineBg` 的感知亮度；空串（终端默认色）时为 `None`。
    status_line_luma: Option<f32>,
}

impl Theme {
    /// 从解析好的主题文件构造。
    ///
    /// # Errors
    ///
    /// 任一颜色槽位引用了未定义的 `vars` 键、引用成环，或写了非法 hex。
    pub fn from_file(
        file: &ThemeFile,
        mode: ColorMode,
        preset: SymbolPreset,
    ) -> Result<Self, ColorError> {
        let (colors, bg, status_line_luma) = file.colors.resolve(&file.vars, mode)?;
        let thinking_max = match &file.colors.thinking_max {
            Some(raw) => color::resolve("thinkingMax", raw, &file.vars)?.to_color(mode),
            None => colors.thinking_xhigh,
        };
        Ok(Self {
            name: file.name.clone(),
            colors,
            bg,
            symbols: preset.symbols(),
            mode,
            thinking_max,
            status_line_luma,
        })
    }

    /// 从主题 JSON 文本构造。
    ///
    /// # Errors
    ///
    /// JSON 不合法、缺少必需的颜色键，或颜色解析失败。
    pub fn from_json(
        json: &str,
        mode: ColorMode,
        preset: SymbolPreset,
    ) -> Result<Self, ThemeError> {
        let file: ThemeFile = serde_json::from_str(json)?;
        Ok(Self::from_file(&file, mode, preset)?)
    }

    /// 这是不是一个「亮色」主题。
    ///
    /// 判据是 `statusLineBg` 的感知亮度 > 0.5，**不是** `userMessageBg`：上游踩过
    /// 这个坑——`porcelain` 那类主题在整体亮色里放一个暗色聊天气泡，用气泡色判会
    /// 判反（`theme.ts:1491-1498`）。状态行才是「session 强调色实际绘制的表面」。
    #[must_use]
    pub fn is_light(&self) -> bool {
        self.status_line_luma.is_some_and(|luma| luma > 0.5)
    }

    /// 正文样式：默认前景。
    #[must_use]
    pub fn text(&self) -> Style {
        Style::new().fg(self.colors.text)
    }

    /// 极淡文本。用颜色而非 [`Modifier::DIM`]，理由见模块文档。
    #[must_use]
    pub fn dim(&self) -> Style {
        Style::new().fg(self.colors.dim)
    }

    /// 次级文本。
    #[must_use]
    pub fn muted(&self) -> Style {
        Style::new().fg(self.colors.muted)
    }

    /// 强调文本。
    #[must_use]
    pub fn accent(&self) -> Style {
        Style::new().fg(self.colors.accent)
    }

    /// 卡片标题：强调色 + 粗体。
    #[must_use]
    pub fn title(&self) -> Style {
        Style::new()
            .fg(self.colors.accent)
            .add_modifier(Modifier::BOLD)
    }

    /// 按思考档位取边框色。`Max` 已经在构造时应用过 `xhigh` 回落。
    #[must_use]
    pub fn thinking_color(&self, level: ThinkingLevel) -> Color {
        match level {
            ThinkingLevel::Off => self.colors.thinking_off,
            ThinkingLevel::Minimal => self.colors.thinking_minimal,
            ThinkingLevel::Low => self.colors.thinking_low,
            ThinkingLevel::Medium => self.colors.thinking_medium,
            ThinkingLevel::High => self.colors.thinking_high,
            ThinkingLevel::Xhigh => self.colors.thinking_xhigh,
            ThinkingLevel::Max => self.thinking_max,
        }
    }

    /// 取 spinner 的一帧。`tick` 由调用方按自己的节奏推进，这里只取模——
    /// 帧数随符号档位变（unicode 8 帧、nerd 12 帧、ascii 4 帧），硬编码 8 会越界。
    #[must_use]
    pub fn spinner_frame(&self, kind: SpinnerKind, tick: u64) -> &'static str {
        let frames = match kind {
            SpinnerKind::Status => self.symbols.spinner.status,
            SpinnerKind::Activity => self.symbols.spinner.activity,
        };
        let len = u64::try_from(frames.len()).unwrap_or(0);
        if len == 0 {
            return "";
        }
        let idx = usize::try_from(tick % len).unwrap_or(0);
        frames.get(idx).copied().unwrap_or("")
    }
}

/// 思考强度档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevel {
    /// 关闭。
    Off,
    /// 最低。
    Minimal,
    /// 低。
    Low,
    /// 中。
    Medium,
    /// 高。
    High,
    /// 超高。
    Xhigh,
    /// 最高。
    Max,
}

/// spinner 的两种节奏。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinnerKind {
    /// 状态行 / 工具卡片头，较稳重。
    Status,
    /// 活动指示，较轻快。
    Activity,
}

/// 主题加载失败。
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    /// JSON 语法错误或缺少必需的颜色键。
    #[error("主题 JSON 解析失败：{0}")]
    Json(#[from] serde_json::Error),
    /// 颜色槽位解析失败。
    #[error(transparent)]
    Color(#[from] ColorError),
}

/// 内置暗色主题的 JSON。
pub const DARK_JSON: &str = include_str!("themes/dark.json");
/// 内置亮色主题的 JSON。
pub const LIGHT_JSON: &str = include_str!("themes/light.json");

/// 内置主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuiltinTheme {
    /// 暗色。
    #[default]
    Dark,
    /// 亮色。
    Light,
}

impl BuiltinTheme {
    /// 该主题的 JSON 源文本。
    #[must_use]
    pub const fn json(self) -> &'static str {
        match self {
            Self::Dark => DARK_JSON,
            Self::Light => LIGHT_JSON,
        }
    }

    /// 构造 [`Theme`]。
    ///
    /// # Errors
    ///
    /// 只可能在内置 JSON 与 [`RawColors`] 的字段定义漂移时发生；单测
    /// `builtin_themes_load` 会先一步把它挡住。
    pub fn load(self, mode: ColorMode, preset: SymbolPreset) -> Result<Theme, ThemeError> {
        Theme::from_json(self.json(), mode, preset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> Theme {
        BuiltinTheme::Dark
            .load(ColorMode::TrueColor, SymbolPreset::Unicode)
            .expect("内置暗色主题必须能加载")
    }

    #[test]
    fn builtin_themes_load() {
        for builtin in [BuiltinTheme::Dark, BuiltinTheme::Light] {
            for mode in [ColorMode::TrueColor, ColorMode::Indexed256] {
                builtin
                    .load(mode, SymbolPreset::Unicode)
                    .unwrap_or_else(|e| panic!("{builtin:?} 在 {mode:?} 下加载失败：{e}"));
            }
        }
    }

    #[test]
    fn var_indirection_resolves() {
        // dark.json: colors.accent -> vars.accent -> #febc38
        let t = dark();
        assert_eq!(t.colors.accent, Color::Rgb(0xfe, 0xbc, 0x38));
        // colors.success -> vars.green -> #89d281，且 diff_added 引用同一个 var。
        assert_eq!(t.colors.success, Color::Rgb(0x89, 0xd2, 0x81));
        assert_eq!(t.colors.diff_added, t.colors.success);
    }

    #[test]
    fn empty_string_means_terminal_default() {
        // `text` / `toolTitle` / `userMessageText` 在两个内置主题里都是空串。
        let t = dark();
        assert_eq!(t.colors.text, Color::Reset);
        assert_eq!(t.colors.tool_title, Color::Reset);
        assert_eq!(t.colors.user_message_text, Color::Reset);
    }

    #[test]
    fn numeric_values_become_palette_indices() {
        // dark.json 的 statusLineSep / statusLineStaged 是裸数字。
        let t = dark();
        assert_eq!(t.colors.status_line_sep, Color::Indexed(244));
        assert_eq!(t.colors.status_line_staged, Color::Indexed(70));
    }

    #[test]
    fn light_and_dark_classify_correctly() {
        // 判据取 statusLineBg：dark 是 #121212，light 是 #e0e0e0。
        let light = BuiltinTheme::Light
            .load(ColorMode::TrueColor, SymbolPreset::Unicode)
            .expect("内置亮色主题必须能加载");
        assert!(!dark().is_light());
        assert!(light.is_light());
    }

    #[test]
    fn thinking_max_falls_back_to_xhigh() {
        // 两个内置主题都没有定义 thinkingMax。
        let t = dark();
        assert_eq!(t.thinking_max, t.colors.thinking_xhigh);
        assert_eq!(
            t.thinking_color(ThinkingLevel::Max),
            t.colors.thinking_xhigh
        );
    }

    #[test]
    fn indexed_mode_quantizes_direct_colors() {
        let t = BuiltinTheme::Dark
            .load(ColorMode::Indexed256, SymbolPreset::Unicode)
            .expect("内置暗色主题必须能加载");
        // truecolor 下是 Rgb，256 档下必须已经量化成索引。
        assert!(matches!(t.colors.accent, Color::Indexed(_)));
        // 调色板索引不受色深影响。
        assert_eq!(t.colors.status_line_sep, Color::Indexed(244));
    }

    #[test]
    fn spinner_frame_wraps_without_panicking() {
        let t = dark();
        let frames = u64::try_from(t.symbols.spinner.status.len()).unwrap_or(8);
        assert_eq!(
            t.spinner_frame(SpinnerKind::Status, 0),
            t.spinner_frame(SpinnerKind::Status, frames)
        );
        // 极大 tick 不能 panic，也不能返回空。
        assert!(!t.spinner_frame(SpinnerKind::Activity, u64::MAX).is_empty());
    }

    #[test]
    fn missing_required_key_is_an_error() {
        let err = Theme::from_json(
            r##"{"name":"x","vars":{},"colors":{"accent":"#ffffff"}}"##,
            ColorMode::TrueColor,
            SymbolPreset::Unicode,
        );
        assert!(matches!(err, Err(ThemeError::Json(_))));
    }

    #[test]
    fn unknown_var_is_reported_with_the_key() {
        let broken = DARK_JSON.replace("\"accent\": \"accent\"", "\"accent\": \"nope\"");
        match Theme::from_json(&broken, ColorMode::TrueColor, SymbolPreset::Unicode) {
            Err(ThemeError::Color(ColorError::UnknownVar { key, var })) => {
                assert_eq!(key, "accent");
                assert_eq!(var, "nope");
            }
            other => panic!("期望 UnknownVar，实际 {other:?}"),
        }
    }
}
