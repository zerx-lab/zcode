//! 主题颜色的四态模型、解析、以及色深降级。
//!
//! # 四态
//!
//! 主题 JSON 的 `colors` 段每个值是四态之一，判定顺序**严格**如下
//! （oh-my-pi `packages/coding-agent/src/modes/theme/theme.ts:1328-1344`）：
//!
//! | 形态 | 语义 |
//! | --- | --- |
//! | JSON number | 0-255 的 ANSI 调色板索引 |
//! | `""` | 终端默认色（`Color::Reset`） |
//! | `#…` 前缀 | 字面 hex |
//! | 其余字符串 | `vars` 段的键名，递归解析 |
//!
//! 顺序不能调换：`vars` 里完全可以有一个叫 `accent` 的键，同时 `colors.accent`
//! 的值是 `"#febc38"`——先看前缀才能区分「字面 hex」与「恰好长得像 hex 的 var 名」。
//!
//! # 色深
//!
//! [`ColorMode`] 只有 truecolor / 256 两档，与上游一致（`theme.ts:1279-1301`）。
//! **没有 16 色档**：`TERM=linux` 这类真 8/16 色终端会收到 `38;5;` 序列，由终端
//! 自己近似——上游的取舍，代价是 16 色终端上保真度下降，收益是省掉一整套量化路径。
//!
//! 256 量化算法上游没有（它在 Bun 运行时里），[`quantize_256`] 是本仓实现。

use std::collections::BTreeMap;

use ratatui::style::Color;

/// 主题 JSON 里一个颜色槽位的原始值（尚未解析 `vars` 引用）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
pub enum RawColor {
    /// ANSI 调色板索引（0-255）。JSON 里写成裸数字。
    Index(u8),
    /// 空串（终端默认色）、`#…` 字面 hex，或 `vars` 段的键名。
    Named(String),
}

/// `vars` 段：自由命名的间接层，值同样是 [`RawColor`] 的字符串形态。
pub type Vars = BTreeMap<String, String>;

/// 主题解析失败。
#[derive(Debug, thiserror::Error)]
pub enum ColorError {
    /// `colors.<key>` 引用了 `vars` 里不存在的名字。
    #[error("颜色 `{key}` 引用了未定义的变量 `{var}`")]
    UnknownVar {
        /// 出问题的 `colors` 键。
        key: &'static str,
        /// 找不到的 `vars` 键。
        var: String,
    },
    /// `vars` 内部成环（`a → b → a`）。
    #[error("颜色 `{key}` 的变量引用成环：`{var}`")]
    CircularVar {
        /// 出问题的 `colors` 键。
        key: &'static str,
        /// 环上被二次访问的 `vars` 键。
        var: String,
    },
    /// `#` 开头但不是合法的 3 / 6 / 8 位 hex。
    #[error("颜色 `{key}` 的值 `{value}` 不是合法 hex")]
    BadHex {
        /// 出问题的 `colors` 键。
        key: &'static str,
        /// 原始值。
        value: String,
    },
}

/// 终端色深。启动时判定一次，全程只读——与 [`crate::caps::OutputCaps`] 同一约定
/// （不变量 5：不在渲染路径反复探测终端）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// 24-bit 直色。默认档：现代终端几乎全支持，探测失败时丢色深的代价远小于
    /// 误判 256 丢保真度（`theme.ts:1285-1301` 的同一取舍）。
    #[default]
    TrueColor,
    /// 256 色调色板。
    Indexed256,
}

impl ColorMode {
    /// 从环境变量判定色深。纯函数版见 [`ColorMode::from_env_values`]。
    #[must_use]
    pub fn probe() -> Self {
        Self::from_env_values(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var_os("WT_SESSION").is_some(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    /// 判定逻辑本体，参数化以便测试。
    ///
    /// `TERM` 为 `dumb` / 空 / `linux` 时判 256，其余一律 truecolor。`linux` 是真
    /// 8/16 色 TTY，判 256 已经是高估——但上游没有 16 色档，这里同样不引入。
    #[must_use]
    pub fn from_env_values(colorterm: Option<&str>, wt_session: bool, term: Option<&str>) -> Self {
        if matches!(colorterm, Some("truecolor" | "24bit")) || wt_session {
            return Self::TrueColor;
        }
        match term {
            None | Some("" | "dumb" | "linux") => Self::Indexed256,
            Some(_) => Self::TrueColor,
        }
    }
}

/// 已解析、未降级的颜色：要么是具体 RGB，要么是调色板索引，要么是终端默认色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rgb {
    /// 24-bit 直色。
    Direct(u8, u8, u8),
    /// ANSI 调色板索引。
    Palette(u8),
    /// 终端默认色（主题里的空串）。
    Default,
}

impl Rgb {
    /// 按色深降级成 ratatui 的 [`Color`]。
    ///
    /// [`Rgb::Palette`] 不受色深影响：`38;5;n` 在两档下都能发。
    #[must_use]
    pub fn to_color(self, mode: ColorMode) -> Color {
        match self {
            Self::Default => Color::Reset,
            Self::Palette(i) => Color::Indexed(i),
            Self::Direct(r, g, b) => match mode {
                ColorMode::TrueColor => Color::Rgb(r, g, b),
                ColorMode::Indexed256 => Color::Indexed(quantize_256(r, g, b)),
            },
        }
    }

    /// 具体 RGB 分量；[`Rgb::Default`] 没有可用分量，返回 `None`。
    #[must_use]
    pub fn components(self) -> Option<(u8, u8, u8)> {
        match self {
            Self::Direct(r, g, b) => Some((r, g, b)),
            Self::Palette(i) => Some(palette_to_rgb(i)),
            Self::Default => None,
        }
    }
}

/// 解析一个 [`RawColor`]，递归展开 `vars` 引用。
///
/// `key` 只用于错误信息。递归深度由 `visited` 集合封顶：`vars` 是纯链式结构
/// （每层只有一个后继），因此「访问过即成环」这个判据不会误伤合法的 DAG。
pub fn resolve(key: &'static str, raw: &RawColor, vars: &Vars) -> Result<Rgb, ColorError> {
    match raw {
        RawColor::Index(i) => Ok(Rgb::Palette(*i)),
        RawColor::Named(name) => resolve_named(key, name, vars, &mut Vec::new()),
    }
}

fn resolve_named(
    key: &'static str,
    value: &str,
    vars: &Vars,
    visited: &mut Vec<String>,
) -> Result<Rgb, ColorError> {
    if value.is_empty() {
        return Ok(Rgb::Default);
    }
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex(hex)
            .map(|(r, g, b)| Rgb::Direct(r, g, b))
            .ok_or(ColorError::BadHex {
                key,
                value: value.to_owned(),
            });
    }
    // 这里**没有**「纯数字字符串也当调色板索引」的分支：上游没有它
    // （`theme.ts:1328-1344` 对 `"244"` 会走到 `if (!(value in vars)) throw`），
    // 内置主题里也一处都没有——`statusLineSep` 全是裸数字 / `#hex` / var 名三种形态。
    // 加了它就等于让所有名为 `"0"`..`"255"` 的 `vars` 键永远查不到，
    // 而模块文档正在强调四态的判定顺序不可调换。
    if visited.iter().any(|seen| seen == value) {
        return Err(ColorError::CircularVar {
            key,
            var: value.to_owned(),
        });
    }
    let next = vars.get(value).ok_or_else(|| ColorError::UnknownVar {
        key,
        var: value.to_owned(),
    })?;
    visited.push(value.to_owned());
    resolve_named(key, next, vars, visited)
}

/// 解析 `#` 之后的 hex 主体：支持 3 / 6 / 8 位。
///
/// 8 位形态（`#rrggbbaa`）**丢弃 alpha**：终端单元格没有 alpha 通道，上游对它有
/// 两条不一致的处理路径（`theme.ts` 的 hex 与 export 段），本仓统一取前 6 位。
#[must_use]
pub fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let bytes = hex.as_bytes();
    match bytes.len() {
        3 => {
            let r = nybble(*bytes.first()?)?;
            let g = nybble(*bytes.get(1)?)?;
            let b = nybble(*bytes.get(2)?)?;
            // 每位倍写：`#abc` == `#aabbcc`。
            Some((r * 17, g * 17, b * 17))
        }
        6 | 8 => {
            let r = byte_at(bytes, 0)?;
            let g = byte_at(bytes, 2)?;
            let b = byte_at(bytes, 4)?;
            Some((r, g, b))
        }
        _ => None,
    }
}

fn byte_at(bytes: &[u8], at: usize) -> Option<u8> {
    let hi = nybble(*bytes.get(at)?)?;
    let lo = nybble(*bytes.get(at + 1)?)?;
    Some(hi * 16 + lo)
}

const fn nybble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 6×6×6 色立方每一档的实际强度。
/// 出处 oh-my-pi `packages/utils/src/color.ts:230-244` 的 `CUBE_STEPS`。
const CUBE_STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// xterm 前 16 色的标准 RGB。出处同上（`color.ts:208-225` 的 `ANSI_16`）。
const ANSI_16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (128, 0, 0),
    (0, 128, 0),
    (128, 128, 0),
    (0, 0, 128),
    (128, 0, 128),
    (0, 128, 128),
    (192, 192, 192),
    (128, 128, 128),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (0, 0, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// 调色板索引 → RGB。
///
/// 三段：`0..16` 查表、`16..232` 是 6×6×6 立方、`232..=255` 是 24 级灰阶
/// （`8 + (i - 232) * 10`）。
#[must_use]
pub fn palette_to_rgb(index: u8) -> (u8, u8, u8) {
    let slot = usize::from(index);
    if let Some(rgb) = ANSI_16.get(slot) {
        return *rgb;
    }
    if slot < 232 {
        let cube = slot - 16;
        let red = CUBE_STEPS.get((cube / 36) % 6).copied().unwrap_or(0);
        let green = CUBE_STEPS.get((cube / 6) % 6).copied().unwrap_or(0);
        let blue = CUBE_STEPS.get(cube % 6).copied().unwrap_or(0);
        return (red, green, blue);
    }
    let level = 8_u16.saturating_add(u16::try_from(slot - 232).unwrap_or(0) * 10);
    let level = u8::try_from(level).unwrap_or(u8::MAX);
    (level, level, level)
}

/// RGB → 最接近的 256 调色板索引。
///
/// 上游没有这个函数（它在 `Bun.color` 里），因此是本仓实现：分别求「色立方最近格」
/// 与「灰阶最近级」，取平方距离更小的那个。
///
/// **不在渲染热路径上**：只有 [`Rgb::to_color`] 调它，而那条链只在 [`crate::Theme`]
/// 构造时跑一遍（每个颜色键一次，共 66 次），之后主题全程只读。全整数算术是因为
/// 距离比较用整数更直白，不是为了省每帧开销。
#[must_use]
pub fn quantize_256(r: u8, g: u8, b: u8) -> u8 {
    let cube_idx = |v: u8| -> usize {
        // CUBE_STEPS 单调递增，找绝对差最小的一档。
        let mut best = 0;
        let mut best_d = i32::MAX;
        for (i, step) in CUBE_STEPS.iter().enumerate() {
            let d = (i32::from(v) - i32::from(*step)).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    };
    let (ri, gi, bi) = (cube_idx(r), cube_idx(g), cube_idx(b));
    let cube_rgb = (
        CUBE_STEPS.get(ri).copied().unwrap_or(0),
        CUBE_STEPS.get(gi).copied().unwrap_or(0),
        CUBE_STEPS.get(bi).copied().unwrap_or(0),
    );
    let cube_code = u8::try_from(16 + 36 * ri + 6 * gi + bi).unwrap_or(u8::MAX);

    // 灰阶：level = round((v - 8) / 10)，clamp 到 0..=23。用整数做 round-half-up。
    let avg = (i32::from(r) + i32::from(g) + i32::from(b)) / 3;
    let gray_level = ((avg - 8) * 2 + 10).div_euclid(20).clamp(0, 23);
    let gray_v = u8::try_from((8 + gray_level * 10).clamp(0, 255)).unwrap_or(u8::MAX);
    let gray_code = u8::try_from(232 + gray_level).unwrap_or(u8::MAX);

    if dist2((r, g, b), (gray_v, gray_v, gray_v)) < dist2((r, g, b), cube_rgb) {
        gray_code
    } else {
        cube_code
    }
}

fn dist2(a: (u8, u8, u8), b: (u8, u8, u8)) -> i32 {
    let dr = i32::from(a.0) - i32::from(b.0);
    let dg = i32::from(a.1) - i32::from(b.1);
    let db = i32::from(a.2) - i32::from(b.2);
    dr * dr + dg * dg + db * db
}

/// 感知亮度（BT.709 加权，**gamma 编码域**，不做线性化），归一到 `0.0..=1.0`。
///
/// 出处 oh-my-pi `packages/utils/src/color.ts:271-275`。上游注释明确：这个值只用于
/// 亮/暗**分类**，不能拿去算 WCAG 对比度——那需要先 [`linearize`] 再加权。
#[must_use]
pub fn luma(r: u8, g: u8, b: u8) -> f32 {
    (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0
}

/// sRGB 通道线性化，WCAG 2.x 的相对亮度用它。
/// 出处 `color.ts:257-260`。
#[must_use]
pub fn linearize(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.x 相对亮度。出处 `color.ts:285-289`。
#[must_use]
pub fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        let mut v = Vars::new();
        v.insert("accent".into(), "#febc38".into());
        v.insert("alias".into(), "accent".into());
        v.insert("loop_a".into(), "loop_b".into());
        v.insert("loop_b".into(), "loop_a".into());
        v
    }

    #[test]
    fn resolves_four_states() {
        let vars = vars();
        let named = |s: &str| RawColor::Named(s.to_owned());
        assert_eq!(
            resolve("k", &RawColor::Index(244), &vars).unwrap(),
            Rgb::Palette(244)
        );
        assert_eq!(resolve("k", &named(""), &vars).unwrap(), Rgb::Default);
        assert_eq!(
            resolve("k", &named("#89d281"), &vars).unwrap(),
            Rgb::Direct(0x89, 0xd2, 0x81)
        );
        assert_eq!(
            resolve("k", &named("accent"), &vars).unwrap(),
            Rgb::Direct(0xfe, 0xbc, 0x38)
        );
    }

    #[test]
    fn resolves_var_chain() {
        // var 可以指向另一个 var，递归到终态为止。
        assert_eq!(
            resolve("k", &RawColor::Named("alias".into()), &vars()).unwrap(),
            Rgb::Direct(0xfe, 0xbc, 0x38)
        );
    }

    #[test]
    fn rejects_cycles_and_unknown_vars() {
        let vars = vars();
        assert!(matches!(
            resolve("k", &RawColor::Named("loop_a".into()), &vars),
            Err(ColorError::CircularVar { .. })
        ));
        assert!(matches!(
            resolve("k", &RawColor::Named("nope".into()), &vars),
            Err(ColorError::UnknownVar { .. })
        ));
    }

    #[test]
    fn hex_forms() {
        assert_eq!(parse_hex("abc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex("717cb4"), Some((0x71, 0x7c, 0xb4)));
        // 8 位形态丢弃 alpha。
        assert_eq!(parse_hex("717cb425"), Some((0x71, 0x7c, 0xb4)));
        assert_eq!(parse_hex("xyzxyz"), None);
        assert_eq!(parse_hex("abcd"), None);
    }

    #[test]
    fn palette_three_ranges() {
        assert_eq!(palette_to_rgb(0), (0, 0, 0));
        assert_eq!(palette_to_rgb(15), (255, 255, 255));
        // 16 是立方原点，231 是立方最亮角。
        assert_eq!(palette_to_rgb(16), (0, 0, 0));
        assert_eq!(palette_to_rgb(231), (255, 255, 255));
        assert_eq!(palette_to_rgb(232), (8, 8, 8));
        assert_eq!(palette_to_rgb(255), (238, 238, 238));
    }

    #[test]
    fn quantize_roundtrips_palette_entries() {
        // 立方与灰阶上的精确点必须量化回自己，否则 256 色终端上颜色会整体漂移。
        for idx in 16_u8..=255 {
            let (r, g, b) = palette_to_rgb(idx);
            assert_eq!(quantize_256(r, g, b), idx, "调色板 {idx} 未能回到自身");
        }
    }

    #[test]
    fn quantize_prefers_gray_ramp_for_neutral_colors() {
        // #808080 落在灰阶上（244），比立方里最近的 (135,135,135) 更接近。
        assert_eq!(quantize_256(0x80, 0x80, 0x80), 244);
    }

    #[test]
    fn color_mode_probe_rules() {
        assert_eq!(
            ColorMode::from_env_values(Some("truecolor"), false, Some("dumb")),
            ColorMode::TrueColor
        );
        assert_eq!(
            ColorMode::from_env_values(None, true, Some("dumb")),
            ColorMode::TrueColor
        );
        assert_eq!(
            ColorMode::from_env_values(None, false, Some("linux")),
            ColorMode::Indexed256
        );
        assert_eq!(
            ColorMode::from_env_values(None, false, None),
            ColorMode::Indexed256
        );
        assert_eq!(
            ColorMode::from_env_values(None, false, Some("xterm-256color")),
            ColorMode::TrueColor
        );
    }

    #[test]
    fn luma_classifies_dark_and_light_backgrounds() {
        // dark.json 与 light.json 的 statusLineBg：亮暗判据必须落在 0.5 两侧。
        assert!(luma(0x12, 0x12, 0x12) <= 0.5);
        assert!(luma(0xe0, 0xe0, 0xe0) > 0.5);
    }
}
