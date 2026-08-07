//! 视觉验收用例：把主题、markdown、语法高亮、卡片、状态头一次性画到**真实终端**。
//!
//! 单测能断言「行宽恰好等于 width」「span 带对的 `Style`」这类结构契约，但断言不了
//! 「好不好看」——配色对比度、边框接缝、CJK 与 emoji 混排的对齐，只能靠眼睛。
//! 这个例子就是那双眼睛的输入。
//!
//! ```console
//! cargo run -p zcode-tui --example theme_gallery
//! cargo run -p zcode-tui --example theme_gallery -- light
//! cargo run -p zcode-tui --example theme_gallery -- dark ascii
//! ```
//!
//! 观察点：
//!
//! - 三档符号（`unicode` / `nerd` / `ascii`）下边框、状态图标、spinner 都不出豆腐块；
//! - 卡片右边框**齐平**（每行显示宽度必须相等，CJK 与 emoji 各占正确列数）；
//! - 代码块的语法色与散文色可区分，行内代码不带反引号；
//! - 亮色主题下所有文字对背景仍然可读。

use std::io::{self, Write, stdout};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use zcode_tui::card::{Card, Section, StatusLine, render_card, render_status_line};
use zcode_tui::markdown::{MarkdownOptions, render_markdown};
use zcode_tui::theme::{BuiltinTheme, ColorMode, SpinnerKind, SymbolPreset, Theme};

const SAMPLE_MD: &str = r#"# 一级标题

## 二级标题

### 三级标题

散文里可以有 **粗体**、*斜体*、~~删除线~~ 和 `行内代码`，还有
[一个链接](https://example.com) 与自指链接 https://zcode.dev。

> 引用块：它靠左侧竖条和斜体与正文区分。
> 第二行仍在引用里。

- 无序列表项
- 第二项
  - 嵌套一层
1. 有序列表
10. 序号变宽时续行要跟着对齐，这一条特意写长一点好让它换行看效果。

```rust
/// 语法高亮取自主题的 11 个 syntax* 键。
fn main() {
    let answer: u32 = 42;
    println!("hello, {answer}");
}
```

| 列一 | 列二 |
| --- | --- |
| 值 | 中文宽字符 |

---
"#;

#[expect(
    clippy::too_many_lines,
    reason = "画廊就是一长串顺序绘制，拆开反而更难读"
)]
fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let builtin = match args.next().as_deref() {
        Some("light") => BuiltinTheme::Light,
        _ => BuiltinTheme::Dark,
    };
    let preset = match args.next().as_deref() {
        Some("nerd") => SymbolPreset::Nerd,
        Some("ascii") => SymbolPreset::Ascii,
        _ => SymbolPreset::Unicode,
    };
    let theme = match builtin.load(ColorMode::probe(), preset) {
        Ok(theme) => theme,
        Err(err) => {
            eprintln!("加载主题失败：{err}");
            return Ok(());
        }
    };
    let width = crossterm::terminal::size().map_or(80, |(w, _)| w.min(100));

    let mut out = stdout();
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(heading(
        &theme,
        &format!(
            "主题：{} · 符号：{preset:?} · 色深：{:?}",
            theme.name, theme.mode
        ),
    ));
    lines.push(Line::default());

    lines.push(heading(&theme, "调色板"));
    lines.extend(swatches(&theme));
    lines.push(Line::default());

    lines.push(heading(&theme, "状态头行"));
    for (icon, color, title, desc) in [
        (
            theme.symbols.status.running,
            theme.colors.warning,
            "Bash",
            "cargo test --workspace",
        ),
        (
            theme.symbols.status.success,
            theme.colors.success,
            "Read",
            "crates/tui/src/theme/mod.rs",
        ),
        (
            theme.symbols.status.error,
            theme.colors.error,
            "Edit",
            "找不到匹配的原文",
        ),
    ] {
        lines.push(render_status_line(
            &StatusLine {
                icon: Some(Span::styled(icon.to_owned(), Style::new().fg(color))),
                title,
                title_style: theme.title(),
                description: Some(desc),
                badge: Some(("2 hunks", Style::new().fg(theme.colors.accent))),
                meta: &["1.2s", "42 行"],
            },
            &theme,
        ));
    }
    lines.push(Line::default());

    lines.push(heading(&theme, "工具卡片（三态底色）"));
    for (label, bg, icon, color) in [
        (
            "pending",
            theme.bg.tool_pending,
            theme.symbols.status.running,
            theme.colors.warning,
        ),
        (
            "success",
            theme.bg.tool_success,
            theme.symbols.status.success,
            theme.colors.success,
        ),
        (
            "error",
            theme.bg.tool_error,
            theme.symbols.status.error,
            theme.colors.error,
        ),
    ] {
        let body = vec![
            Line::from(Span::styled(
                format!("这是 {label} 状态的输出正文"),
                Style::new().fg(theme.colors.tool_output),
            )),
            Line::from(Span::styled(
                "第二行：中文宽字符与 ASCII 混排 mixed 宽度必须算对".to_owned(),
                Style::new().fg(theme.colors.tool_output),
            )),
        ];
        let sections = [Section {
            label: None,
            lines: &body,
        }];
        lines.extend(render_card(
            &Card {
                header: Some(render_status_line(
                    &StatusLine {
                        icon: Some(Span::styled(icon.to_owned(), Style::new().fg(color))),
                        title: "Bash",
                        title_style: theme.title(),
                        description: Some(label),
                        badge: None,
                        meta: &[],
                    },
                    &theme,
                )),
                sections: &sections,
                border: Style::new().fg(theme.colors.border_muted),
                bg: Some(bg),
                padding_left: 1,
                padding_right: 1,
            },
            width,
            &theme,
        ));
        lines.push(Line::default());
    }

    lines.push(heading(&theme, "用户气泡"));
    let bubble = Style::new().bg(theme.bg.user_message);
    let inner = width.saturating_sub(2).max(1);
    lines.push(Line::from(Span::styled(
        " ".repeat(usize::from(width)),
        bubble,
    )));
    for line in render_markdown(
        "把 TUI 的样式完全对标 omp，顺便看看 `行内代码` 在气泡里的效果。",
        inner,
        &theme,
        &MarkdownOptions {
            base: Style::new().fg(theme.colors.user_message_text),
            code_block_indent: 2,
            highlight: true,
        },
    ) {
        let mut spans = vec![Span::styled(" ".to_owned(), bubble)];
        spans.extend(line.spans);
        let padded = zcode_tui::card::pad_line(Line::from(spans), usize::from(width), bubble);
        lines.push(zcode_tui::card::patch_line(padded, bubble));
    }
    lines.push(Line::from(Span::styled(
        " ".repeat(usize::from(width)),
        bubble,
    )));
    lines.push(Line::default());

    lines.push(heading(&theme, "markdown + 语法高亮"));
    lines.extend(render_markdown(
        SAMPLE_MD,
        width,
        &theme,
        &MarkdownOptions {
            base: theme.text(),
            code_block_indent: 0,
            highlight: true,
        },
    ));

    lines.push(heading(&theme, "spinner（两组帧）"));
    for kind in [SpinnerKind::Status, SpinnerKind::Activity] {
        let frames: Vec<Span<'static>> = (0..12)
            .map(|tick| {
                Span::styled(
                    format!("{} ", theme.spinner_frame(kind, tick)),
                    Style::new().fg(theme.colors.accent),
                )
            })
            .collect();
        lines.push(Line::from(frames));
    }

    for line in lines {
        write_line(&mut out, &line)?;
    }
    out.flush()
}

fn heading(theme: &Theme, text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_owned(),
        Style::new()
            .fg(theme.colors.md_heading)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    ))
}

/// 每个语义色画一格色块 + 名字，用来一眼看出对比度是否够。
fn swatches(theme: &Theme) -> Vec<Line<'static>> {
    let entries: [(&str, Color); 10] = [
        ("accent", theme.colors.accent),
        ("border", theme.colors.border),
        ("borderMuted", theme.colors.border_muted),
        ("success", theme.colors.success),
        ("error", theme.colors.error),
        ("warning", theme.colors.warning),
        ("muted", theme.colors.muted),
        ("dim", theme.colors.dim),
        ("mdHeading", theme.colors.md_heading),
        ("mdCode", theme.colors.md_code),
    ];
    entries
        .chunks(5)
        .map(|chunk| {
            let mut spans = Vec::new();
            for (name, color) in chunk {
                spans.push(Span::styled("  ".to_owned(), Style::new().bg(*color)));
                spans.push(Span::styled(
                    format!(" {name:<12}"),
                    Style::new().fg(*color),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

/// 把一条 `Line` 连同它的 `Style` 写成 ANSI 字节。
///
/// 例子不走 `Emitter`：它只是顺序打印，没有 viewport、没有历史注入，用不上那套账本。
fn write_line(out: &mut impl Write, line: &Line<'_>) -> io::Result<()> {
    for span in &line.spans {
        let style = line.style.patch(span.style);
        let mut sgr = String::from("\x1b[0m");
        if let Some(fg) = style.fg {
            sgr.push_str(&sgr_color(fg, true));
        }
        if let Some(bg) = style.bg {
            sgr.push_str(&sgr_color(bg, false));
        }
        if style.add_modifier.contains(Modifier::BOLD) {
            sgr.push_str("\x1b[1m");
        }
        if style.add_modifier.contains(Modifier::DIM) {
            sgr.push_str("\x1b[2m");
        }
        if style.add_modifier.contains(Modifier::ITALIC) {
            sgr.push_str("\x1b[3m");
        }
        if style.add_modifier.contains(Modifier::UNDERLINED) {
            sgr.push_str("\x1b[4m");
        }
        if style.add_modifier.contains(Modifier::REVERSED) {
            sgr.push_str("\x1b[7m");
        }
        if style.add_modifier.contains(Modifier::CROSSED_OUT) {
            sgr.push_str("\x1b[9m");
        }
        write!(out, "{sgr}{}", span.content)?;
    }
    writeln!(out, "\x1b[0m")
}

fn sgr_color(color: Color, foreground: bool) -> String {
    let base = if foreground { 38 } else { 48 };
    match color {
        Color::Rgb(r, g, b) => format!("\x1b[{base};2;{r};{g};{b}m"),
        Color::Indexed(i) => format!("\x1b[{base};5;{i}m"),
        // `Color::Reset` 与其余具名色都回到终端默认：本仓主题只产出 Rgb / Indexed /
        // Reset 三种，具名色不该出现，与其猜一个 SGR 编号不如保守走默认。
        _ => format!("\x1b[{}m", if foreground { 39 } else { 49 }),
    }
}
