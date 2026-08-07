//! 测量渲染热路径的实际成本：markdown 排版与 syntect 高亮各要多久。
//!
//! 存在的理由：oh-my-pi 记录过「100 行代码高亮 ~26 ms、150 行 ~40 ms，吃光 33 ms 帧
//! 预算」（`packages/coding-agent/src/modes/theme/theme.ts:2942-2948`），并因此加了一层
//! LRU。但那个数字含 JS↔Rust FFI 往返、ANSI 编码、再由 JS 侧解析；本仓是纯 Rust 直出
//! `Span`，省掉了整条编解码链。**要不要抄那层缓存，取决于本仓自己的实测值，不是上游的。**
//!
//! ```console
//! cargo run -p zcode-tui --release --example render_cost
//! ```
//!
//! 必须用 `--release` 跑：debug 下 syntect 的正则引擎慢一个数量级，测出来的数字对
//! 「发布后会不会卡」这个问题没有意义。

use std::time::Instant;

use ratatui::style::Style;
use zcode_tui::markdown::{MarkdownOptions, render_markdown};
use zcode_tui::theme::{BuiltinTheme, ColorMode, SymbolPreset};

fn main() {
    let Ok(theme) = BuiltinTheme::Dark.load(ColorMode::TrueColor, SymbolPreset::Unicode) else {
        eprintln!("内置主题加载失败");
        return;
    };

    // 三种典型负载：纯散文（最常见）、带一个中等代码块、大代码块（最坏情况）。
    let prose =
        "这是一段中文散文，混着 English words 和 `行内代码`，还有 **强调**。\n\n".repeat(20);
    let code_block = format!("说明文字。\n\n```rust\n{}```\n", rust_code(40));
    let big_code = format!("```rust\n{}```\n", rust_code(200));

    for (label, src, highlight) in [
        ("散文 40 行", prose.as_str(), true),
        ("散文 + 40 行代码", code_block.as_str(), true),
        ("200 行代码（高亮）", big_code.as_str(), true),
        ("200 行代码（不高亮）", big_code.as_str(), false),
    ] {
        let opts = MarkdownOptions {
            base: Style::default(),
            code_block_indent: 0,
            highlight,
        };
        // 先跑一次把 syntect 语法集的懒加载排除在计时之外——它一个进程只发生一次，
        // 混进每帧成本里会得出错误结论。
        let warm = render_markdown(src, 100, &theme, &opts);
        let started = Instant::now();
        for _ in 0..ITERATIONS {
            let _ = render_markdown(src, 100, &theme, &opts);
        }
        let per_call = started.elapsed() / ITERATIONS;
        println!("{label:<24} {:>6} 行  {per_call:>10.2?}/次", warm.len());
    }

    println!();
    println!("判据：单帧预算约 33 ms（30 fps）。变化的块每帧重渲染一次——所以只要");
    println!("单块超过预算，流式输出就会肉眼可见地卡。这正是流式期间关掉高亮的理由。");
}

/// 每档测量的重复次数。20 次足够压掉调度抖动，又不至于让整个例子跑太久。
const ITERATIONS: u32 = 20;

fn rust_code(lines: usize) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for i in 0..lines {
        let initial = char::from(b'a' + u8::try_from(i % 26).unwrap_or(0));
        // 写进 String 不会失败；真失败了这个例子的输出也没意义，直接停在这里。
        let _ = writeln!(
            out,
            "fn item_{i}(input: &str) -> Option<usize> {{ input.find('{initial}').map(|p| p + {i}) }}"
        );
    }
    out
}
