//! stderr 展示文本清洗：制表符展开、控制字符清洗、按终端宽度截断。
//!
//! 只用于人读的进度/工具摘要/审批提示这些行；模型最终文本走原样透传，理由见
//! `crate::render` 模块文档「两种格式」一节——清理会改写字节，与 stdout「原样
//! 转发模型输出」的契约冲突。

use zcode_text::width;

/// stderr 展示用的终端宽度；探测失败时退回 80 列。
///
/// headless 场景下 stderr 常年被重定向到日志文件或非 tty 管道（`zcode run ...
/// 2> log`、CI 里完全没有控制终端），`crossterm::terminal::size` 在这些场景下
/// 必然失败——这不是异常，是 headless 的常态，因此静默回退而不是向上传播错误。
/// 80 列取 VT100 以来的事实标准最小终端宽度（DEC VT100 默认几何、`stty size`
/// 探测失败时几乎所有终端模拟器与 shell 工具的共同兜底值）。
fn stderr_width() -> usize {
    match crossterm::terminal::size() {
        Ok((cols, _rows)) if cols > 0 => usize::from(cols),
        _ => 80,
    }
}

/// 清洗一行准备写到 stderr 的展示文本。
///
/// 三步固定顺序：控制字符/`ESC` 清洗 → 制表符按显示宽度展开（制表符在等宽
/// 网格里原样保留会造成视觉空洞）→ 按终端宽度截断（绝不用 `str::len()`）。
/// 每条渲染路径都要过它，包括错误消息——`rule://zcode-architecture` 的
/// 「TUI 输出清理」一节明写这一条不能只覆盖成功路径。
pub(super) fn clean_line(text: &str) -> String {
    let sanitized = width::sanitize_text(text);
    let expanded = width::expand_tabs(&sanitized);
    let limit = stderr_width();
    width::truncate_to_width(&expanded, limit, "…").into_owned()
}
