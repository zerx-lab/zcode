//! 出站图像的解码 → 缩放 → 重编码流水线。
//!
//! 三仓（oh-my-pi / jcode / opencode）都没有可直接移植的 Rust 图像链路——
//! oh-my-pi 整条链路跑在 `Bun.Image` 里，Rust 侧只有 SIXEL 编码。可移植的是**策略与常量**，
//! 不是代码：jcode `crates/jcode-base/src/provider/image_clamp.rs:59-113,183-228` 给出了
//! “尺寸钳制 + 字节钳制”两阶段的形状，缩放滤波按用途分档（出站保真用 Lanczos3，
//! 预算迭代用便宜的 Triangle，见 `image_clamp.rs:208,279`），本模块照搬这个分档。
//!
//! # 常量与前提
//!
//! | 值 | 前提 |
//! | --- | --- |
//! | `max_width`/`max_height` = 1568 | Anthropic 内部推荐尺寸：Claude 在 vision 处理前会把长边压到 1568px。**换 provider 该数即不成立。** |
//! | `max_bytes` = `500_000` | 降到 1568px 后 5 MB/图 的硬上限极少成为约束，目标定在远低处以省 token。 |
//! | `min_dimension` = 200 | **前提最硬**：vision 后端按固定 patch 切片（Anthropic 28px 一格），亚-patch 图会返回硬 400 并**污染整个请求**；200×200 = 64 visual tokens 是文档给出的最小合法尺寸。 |
//! | 快路径阈值 = `max_bytes / 4` | 已经这么小就别重编码，避免在小图标上做无用功；同时保证较大 PNG 仍会被送去竞速压缩。 |
//! | `jpeg_quality` = 80，质量梯 `[70,60,50,40]`，尺度梯 `[0.75,0.5,0.35,0.25]` | **上游（jcode）均无出处**，已核实，沿用但标明「前提未知」。 |
//!
//! **修掉的上游矛盾**：jcode 的尺度梯地板是 100px，会把图缩进被上面这张表证明危险的
//! `<200px` 区间。本仓统一把地板钳在 `min_dimension`（默认 200px），宁可让长边超出
//! `max_width`/`max_height`（软约束，Claude 自己也会再缩一次），也不让任何一边跌破
//! patch 最小合法尺寸（硬约束，跌破直接 400）。
//!
//! # 调用约定
//!
//! 整条链路是纯 CPU 运算（解码、重采样、多编码器竞速都不做 I/O），没有一处 `.await`。
//! 调用方如果在异步运行时里跑，必须自己包一层 `spawn_blocking`，本 crate 不依赖任何
//! 异步运行时，不代为处理。

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader, Limits};

/// 输入字节数硬上限（20 MiB）。
///
/// 前提：调用方最终常会把结果 base64 编码后塞进 JSON 请求体，不设上限的话一张畸形/
/// 恶意构造的大图会在解码前就先把内存吃穿。20 MiB 远大于任何合理的出站图像，纯粹是
/// OOM 兜底，不代表期望的输入尺寸。
const MAX_INPUT_BYTES: usize = 20 * 1024 * 1024;

/// 单张图像允许解码的像素总数上限：宽 × 高 ≤ 6400 万（约 8000×8000）。
///
/// 前提：这是 `ResizeOptions` 默认目标尺寸（1568px）的十几倍余量，覆盖几乎所有真实
/// 截图/照片；同时把单张 RGBA8 解码缓冲钉在约 244 MiB 内（6400 万像素 × 4 字节/像素）。
/// `MAX_INPUT_BYTES` 只挡压缩后的输入体积——一张几十 KB 的 PNG 完全可以在 IHDR 里声明
/// 50000×50000，解码时才会去申请那数 GB 的像素缓冲区（经典解压炸弹）。这道检查用探测
/// 阶段（不解码像素）拿到的宽高做乘法校验，在任何解码调用之前就拒绝，不给分配器机会。
const MAX_PIXELS: u64 = 64_000_000;

/// 字节预算超支后的 JPEG 质量梯：先降质量（可逆的信息损失），不动尺寸。
///
/// 数值本身上游（jcode）无出处，已核实，沿用但标明「前提未知」。只会尝试严格低于
/// [`ResizeOptions::jpeg_quality`] 的档位——避免在自定义了更低初始质量时反而调高。
const JPEG_QUALITY_LADDER: [u8; 4] = [70, 60, 50, 40];

/// 质量降到底仍超预算后的尺寸梯（相对初次缩放后目标尺寸的比例，`(分子, 分母)`）。
///
/// 对应 0.75 / 0.5 / 0.35 / 0.25；上游尺度梯的首项 1.0 就是进入本梯之前已经尝试过的
/// 初次缩放结果，这里不重复。地板钳在调用方传入的 `min_dimension`（默认 200px）——
/// 这是本模块相对 jcode 的修正点，见模块文档「修掉的上游矛盾」。数值本身同样「前提未知」。
const SCALE_LADDER: [(u32, u32); 4] = [(3, 4), (1, 2), (7, 20), (1, 4)];

/// 图像处理流水线可能出现的错误。
///
/// 按失败阶段细分，禁止用一个 catch-all 吞掉全部原因——上游整个 `try` 共用一个
/// `catch` 退化成“返回原图 + decodeFailed”，区分不了格式不认识 / 编码器写出失败 /
/// 尺寸非法这三种完全不同、需要不同处理方式的失败。
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    /// 无法从字节内容识别出受支持的图像格式：既可能是 magic bytes 完全不匹配任何已知
    /// 格式，也可能是识别出的格式不在本 crate 启用的编解码器集合（PNG/JPEG/GIF/WebP）内。
    #[error("无法识别图像格式")]
    UnknownFormat,
    /// 已识别格式但解码失败（文件损坏、被截断、内部尺寸校验不过等）。
    #[error("图像解码失败：{source}")]
    Decode {
        /// 底层解码错误。
        #[source]
        source: image::ImageError,
    },
    /// 指定编码器写出失败。
    #[error("以 {mime:?} 编码图像失败：{source}")]
    Encode {
        /// 失败时正在尝试的目标格式。
        mime: ImageMime,
        /// 底层编码错误。
        #[source]
        source: image::ImageError,
    },
    /// 输入字节数超过 [`MAX_INPUT_BYTES`]。
    #[error("图像字节数 {bytes} 超过上限 {limit}")]
    TooLarge {
        /// 实际字节数。
        bytes: usize,
        /// 允许的上限。
        limit: usize,
    },
    /// 探测到的宽或高为 0，图像退化，任何缩放都无意义。
    #[error("图像尺寸退化：{width}×{height}")]
    DegenerateDimensions {
        /// 退化宽度。
        width: u32,
        /// 退化高度。
        height: u32,
    },
    /// 探测到的像素总数（宽 × 高）超过 [`MAX_PIXELS`]：文件头可能声明了远超实际内容
    /// 体量的巨幅尺寸（典型解压炸弹构造），在解码前就拒绝，不给分配器机会。
    #[error("图像像素总数 {width}×{height} 超过上限 {limit}")]
    TooManyPixels {
        /// 探测到的宽度。
        width: u32,
        /// 探测到的高度。
        height: u32,
        /// 像素总数上限。
        limit: u64,
    },
}

/// 处理流水线认得的图像 MIME 类型。
///
/// 输出编码只在 `Png`/`Jpeg`/`Webp` 三者间竞速取最小；`Gif` 只会作为快路径下原样
/// 透传的输入格式出现，流水线从不把 GIF 当重编码目标（GIF 需要调色板量化，和这里
/// “选最小静态帧编码”的目标不匹配，且 Claude vision 本就按静态图处理输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMime {
    /// `image/png`。
    Png,
    /// `image/jpeg`。
    Jpeg,
    /// `image/webp`。
    Webp,
    /// `image/gif`。
    Gif,
}

impl ImageMime {
    /// 返回标准 MIME 字符串，供调用方拼装 `data:` URL 或 HTTP 头。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ImageMime::Png => "image/png",
            ImageMime::Jpeg => "image/jpeg",
            ImageMime::Webp => "image/webp",
            ImageMime::Gif => "image/gif",
        }
    }

    /// 从 [`image::ImageFormat`] 转换；本 crate 只启用了 PNG/JPEG/GIF/WebP 四个编解码器
    /// （见根 `Cargo.toml` 的 `image` feature 列表），其余格式一律视为不认识。
    fn from_format(format: ImageFormat) -> Option<Self> {
        match format {
            ImageFormat::Png => Some(ImageMime::Png),
            ImageFormat::Jpeg => Some(ImageMime::Jpeg),
            ImageFormat::WebP => Some(ImageMime::Webp),
            ImageFormat::Gif => Some(ImageMime::Gif),
            _ => None,
        }
    }

    /// 转回 [`image::ImageFormat`]，供解码器选型使用。
    fn to_format(self) -> ImageFormat {
        match self {
            ImageMime::Png => ImageFormat::Png,
            ImageMime::Jpeg => ImageFormat::Jpeg,
            ImageMime::Webp => ImageFormat::WebP,
            ImageMime::Gif => ImageFormat::Gif,
        }
    }
}

/// 图像处理的可调参数。默认值见 [`Default`] 实现；每个字段成立的前提写在模块文档的表格里。
#[derive(Debug, Clone, Copy)]
pub struct ResizeOptions {
    /// 最大宽度（像素）。
    pub max_width: u32,
    /// 最大高度（像素）。
    pub max_height: u32,
    /// 最小边长（像素）；任何一边小于此值都会被等比放大到此值。
    pub min_dimension: u32,
    /// 编码后字节数的软上限：超过会先降 JPEG 质量、再降尺寸去够，够不到就返回目前找到的最小结果。
    pub max_bytes: usize,
    /// 初次编码竞速使用的 JPEG 质量（1-100）。
    pub jpeg_quality: u8,
    /// 是否把 WebP 纳入编码竞速。
    pub allow_webp: bool,
}

impl Default for ResizeOptions {
    /// `1568` / `1568` / `200` / `500_000` / `80` / `true` —— 具体前提见模块文档的常量表。
    fn default() -> Self {
        Self {
            max_width: 1568,
            max_height: 1568,
            min_dimension: 200,
            max_bytes: 500_000,
            jpeg_quality: 80,
            allow_webp: true,
        }
    }
}

/// 图像处理流水线的输出。
#[derive(Debug, Clone)]
pub struct ProcessedImage {
    /// 编码后的图像字节（不含 base64；编码交给调用方，避免上游那种“惰性 getter 每次访问
    /// 重编码一次”的缺陷）。
    pub bytes: Vec<u8>,
    /// `bytes` 的实际编码格式。
    pub mime: ImageMime,
    /// 最终宽度（像素）。
    pub width: u32,
    /// 最终高度（像素）。
    pub height: u32,
    /// 原始宽度（像素）。
    pub original_width: u32,
    /// 原始高度（像素）。
    pub original_height: u32,
    /// 是否发生了重新编码；`false` 表示原样透传，`bytes` 与输入完全一致（快路径）。
    pub reencoded: bool,
}

impl ProcessedImage {
    /// 模型看到的是缩放后的图，给出的像素坐标要乘回这里的系数才能换算回原图坐标。
    ///
    /// 只有在尺寸真的发生了变化（宽或高与原图不同）时才返回 `Some`——快路径透传，或者
    /// 重编码但尺寸没变（比如只是压缩了字节数）时都应当返回 `None`，否则调用方会对着
    /// 一个恒等变换徒增换算噪音。
    #[must_use]
    pub fn dimension_note(&self) -> Option<String> {
        if self.width == self.original_width && self.height == self.original_height {
            return None;
        }
        // width/height 恒 >= 1（流水线保证，见 fit_dimensions），除法不会退化。
        let scale_x = f64::from(self.original_width) / f64::from(self.width);
        let scale_y = f64::from(self.original_height) / f64::from(self.height);
        Some(format!(
            "图片已从原始 {ow}×{oh} 缩放到 {w}×{h} 后再发送给模型；模型给出的像素坐标需分别乘以约 \
             {scale_x:.3}（横轴）与 {scale_y:.3}（纵轴）才能换算回原图坐标。",
            ow = self.original_width,
            oh = self.original_height,
            w = self.width,
            h = self.height,
        ))
    }
}

/// 只读文件头拿尺寸与格式，不做全量像素解码。
///
/// # 错误
///
/// - 输入超过 [`MAX_INPUT_BYTES`] → [`ImageError::TooLarge`]。
/// - magic bytes 无法识别，或识别出的格式不在本 crate 支持的四种之内 → [`ImageError::UnknownFormat`]。
/// - 识别出格式但文件头本身损坏（如声明的尺寸字段不合法）→ [`ImageError::Decode`]。
pub fn probe_dimensions(input: &[u8]) -> Result<(u32, u32, ImageMime), ImageError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(ImageError::TooLarge {
            bytes: input.len(),
            limit: MAX_INPUT_BYTES,
        });
    }
    // 纯字节内容嗅探，不涉及任何 I/O，因此不会有 io::Error——比 `ImageReader::with_guessed_format`
    // 更适合探测阶段。
    let format = image::guess_format(input).map_err(|_source| ImageError::UnknownFormat)?;
    let mime = ImageMime::from_format(format).ok_or(ImageError::UnknownFormat)?;
    // `with_format` 已知格式，之后只读取该格式的文件头（PNG/JPEG/GIF/WebP 的尺寸字段都在
    // 文件开头几十字节内），不会把整张图解码成像素缓冲区。
    let (width, height) = ImageReader::with_format(Cursor::new(input), format)
        .into_dimensions()
        .map_err(|source| ImageError::Decode { source })?;
    Ok((width, height, mime))
}

/// 解码 → 缩放 → 重编码一张出站图像。
///
/// 流水线：
/// 1. [`probe_dimensions`] 拿原始尺寸与格式（只读文件头，不解码像素）；
/// 2. 宽 × 高超过 [`MAX_PIXELS`] → 立即拒绝（防解压炸弹，见该常量文档）；
/// 3. 尺寸已合规**且**字节数 ≤ `max_bytes/4` → 原样返回（`reencoded = false`）；
/// 4. 否则解码（解码器同样带着 [`MAX_PIXELS`] 派生的资源上限，双重防护），按
///    [`ResizeOptions`] 等比缩放（过小的放大到 `min_dimension`，过大的压到
///    `max_width`/`max_height`，用 [`FilterType::Lanczos3`] 保真）；
/// 5. PNG/JPEG/WebP（`allow_webp` 控制是否含 WebP）并行编码，取最小的一个；
/// 6. 仍超 `max_bytes` → 先降 JPEG 质量、再降尺寸迭代够预算，尺寸迭代轮用便宜的
///    [`FilterType::Triangle`]，地板钳在 `min_dimension`。
///
/// # 错误
///
/// 见 [`ImageError`]；`UnknownFormat`/`TooLarge`/`DegenerateDimensions`/`TooManyPixels` 会在
/// 解码前就返回，
/// 绝不会用“返回原图”当作静默的失败兜底。
pub fn process_image(input: &[u8], options: &ResizeOptions) -> Result<ProcessedImage, ImageError> {
    let (original_width, original_height, mime) = probe_dimensions(input)?;
    if original_width == 0 || original_height == 0 {
        return Err(ImageError::DegenerateDimensions {
            width: original_width,
            height: original_height,
        });
    }

    // 探测阶段只读了文件头、没有解码任何像素，但已经足够算出总像素数——在真正申请
    // 解码缓冲区之前就把畸形/恶意声明的巨幅尺寸拒掉，输入字节数再小也挡不住。
    let pixel_count = u64::from(original_width) * u64::from(original_height);
    if pixel_count > MAX_PIXELS {
        return Err(ImageError::TooManyPixels {
            width: original_width,
            height: original_height,
            limit: MAX_PIXELS,
        });
    }

    // min_dimension 必须先钳到 min(max_width, max_height)，否则“既要 <= max 又要 >= min”
    // 在 min > max 的配置下永远不可能同时满足。
    let min_dimension = options
        .min_dimension
        .min(options.max_width)
        .min(options.max_height);

    let within_bounds = original_width <= options.max_width
        && original_height <= options.max_height
        && original_width >= min_dimension
        && original_height >= min_dimension;
    let fast_path_budget = options.max_bytes / 4;

    if within_bounds && input.len() <= fast_path_budget {
        return Ok(ProcessedImage {
            bytes: input.to_vec(),
            mime,
            width: original_width,
            height: original_height,
            original_width,
            original_height,
            reencoded: false,
        });
    }

    // 用带资源上限的 ImageReader 解码，而不是无限制的 `load_from_memory_with_format`：
    // 这是上面像素数校验之外的第二道防线（比如探测阶段读到的头部尺寸与解码器实际展开
    // 的尺寸因某种极端构造不一致时）。PNG 解码器会在解析完 IHDR、拿到宽高的第一时间
    // 就用 `max_image_width`/`max_image_height` 校验，早于任何像素缓冲区分配。
    let mut reader = ImageReader::with_format(Cursor::new(input), mime.to_format());
    reader.limits(decode_limits());
    let dynamic = reader
        .decode()
        .map_err(|source| ImageError::Decode { source })?;

    let (target_width, target_height) = fit_dimensions(
        original_width,
        original_height,
        options.max_width,
        options.max_height,
        min_dimension,
    );

    let resized = if (target_width, target_height) == (original_width, original_height) {
        dynamic
    } else {
        dynamic.resize_exact(target_width, target_height, FilterType::Lanczos3)
    };

    let initial = encode_best(&resized, options.jpeg_quality, options.allow_webp)?;
    let (bytes, out_mime, final_width, final_height) = shrink_to_budget(
        &resized,
        initial,
        target_width,
        target_height,
        options,
        min_dimension,
    );

    Ok(ProcessedImage {
        bytes,
        mime: out_mime,
        width: final_width,
        height: final_height,
        original_width,
        original_height,
        reencoded: true,
    })
}

/// 解码器级别的资源上限，作为 [`MAX_PIXELS`] 校验之外的第二道防线。
///
/// `Limits` 是 `#[non_exhaustive]`，只能从 `Default` 出发逐字段覆盖。`max_image_width`/
/// `max_image_height` 复用 [`MAX_PIXELS`] 作单边尺寸上限，`max_alloc` 钉住解码器允许
/// 分配的总字节数（按 RGBA8 4 字节/像素估算，与 [`MAX_PIXELS`] 的前提保持一致）。
fn decode_limits() -> Limits {
    // MAX_PIXELS = 64_000_000 远小于 u32::MAX，`try_from` 恒为 Ok；unwrap_or 只是类型
    // 系统要求的兜底，不代表这条路径预期失败。
    let dimension_limit = u32::try_from(MAX_PIXELS).unwrap_or(u32::MAX);
    let mut limits = Limits::default();
    limits.max_image_width = Some(dimension_limit);
    limits.max_image_height = Some(dimension_limit);
    limits.max_alloc = Some(MAX_PIXELS * 4);
    limits
}

/// 组合“压到 max 内”与“放大到 min”两步，得到最终缩放目标尺寸。
fn fit_dimensions(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
    min_dimension: u32,
) -> (u32, u32) {
    let (shrunk_w, shrunk_h) = scale_to_fit(width, height, max_width, max_height);
    scale_up_to_min(shrunk_w, shrunk_h, min_dimension)
}

/// 只做下缩：把宽高等比压进 `max_width`×`max_height` 的 bounding box。已经在范围内则原样返回
/// （这一步绝不放大）。
fn scale_to_fit(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    if width <= max_width && height <= max_height {
        return (width, height);
    }
    let w128 = u128::from(width);
    let h128 = u128::from(height);
    let max_w128 = u128::from(max_width);
    let max_height_128 = u128::from(max_height);
    // 交叉相乘比较宽高比与 max 宽高比，避免浮点除法：谁先顶到边界（宽先顶到 max_width，
    // 还是高先顶到 max_height）由这一次整数比较决定。
    if w128 * max_height_128 >= max_w128 * h128 {
        (max_width, mul_div_round(height, max_width, width))
    } else {
        (mul_div_round(width, max_height, height), max_height)
    }
}

/// 若短边小于 `min_dimension` 则等比放大，让短边恰好等于 `min_dimension`。
///
/// 长边可能因此超过 `max_width`/`max_height`——`min_dimension` 是硬约束（跌破直接触发
/// vision 后端 400），优先级高于 `max_width`/`max_height` 这个软约束（Anthropic 建议值，
/// Claude 自己也会再缩一次）。极端长宽比下两者无法同时满足时，本函数选择保住硬约束。
fn scale_up_to_min(width: u32, height: u32, min_dimension: u32) -> (u32, u32) {
    let short = width.min(height);
    if short == 0 || short >= min_dimension {
        return (width, height);
    }
    if width <= height {
        (min_dimension, mul_div_round(height, min_dimension, width))
    } else {
        (mul_div_round(width, min_dimension, height), min_dimension)
    }
}

/// 计算 `round(a * b / c)`，全程 `u128` 整数运算，不经过浮点、不用 `as` 数值转换。
///
/// 结果钳在 `[1, u32::MAX]`：`a`、`b`、`c` 在本模块内均由已校验非零的图像边长（或
/// `min_dimension`/`max_width`/`max_height`，同样保证 > 0）参与运算，理论上 `a*b/c` 不会
/// 小于 1，但极端长宽比下的整数除法仍可能把很小的分子舍成 0——用 `.max(1)` 兜底，保证
/// 输出恒是合法的正数边长。
fn mul_div_round(a: u32, b: u32, c: u32) -> u32 {
    if c == 0 {
        return a.max(1);
    }
    let product = u128::from(a) * u128::from(b);
    let c128 = u128::from(c);
    let rounded = (product + c128 / 2) / c128;
    u32::try_from(rounded).unwrap_or(u32::MAX).max(1)
}

/// 用 `num/den` 表示的比例缩放一个边长，同样全程整数运算。
fn scale_dim(dim: u32, num: u32, den: u32) -> u32 {
    mul_div_round(dim, num, den)
}

/// PNG/JPEG/WebP（按 `allow_webp` 决定是否含 WebP）并行编码，取最小的一个。
///
/// 线稿/UI 截图 PNG 更小、照片 JPEG/WebP 更小，直接都编一遍取最小比用启发式判断
/// “这张图是照片还是线稿”更可靠——真实图片经常混合两种内容，启发式判断错一次的代价
/// 就是文件明显更大。
fn encode_best(
    image: &DynamicImage,
    jpeg_quality: u8,
    allow_webp: bool,
) -> Result<(Vec<u8>, ImageMime), ImageError> {
    let (png_result, jpeg_result) =
        rayon::join(|| encode_png(image), || encode_jpeg(image, jpeg_quality));

    let mut candidates = vec![(ImageMime::Png, png_result), (ImageMime::Jpeg, jpeg_result)];
    if allow_webp {
        candidates.push((ImageMime::Webp, encode_webp(image)));
    }

    pick_smallest(candidates)
}

/// 从一组 `(格式, 编码结果)` 里选出字节数最小的成功结果；若全部失败，返回第一个失败的
/// 原始错误（而不是吞掉细节）。
fn pick_smallest(
    candidates: Vec<(ImageMime, Result<Vec<u8>, image::ImageError>)>,
) -> Result<(Vec<u8>, ImageMime), ImageError> {
    let mut best: Option<(ImageMime, Vec<u8>)> = None;
    let mut first_error: Option<(ImageMime, image::ImageError)> = None;

    for (mime, result) in candidates {
        match result {
            Ok(bytes) => {
                let is_better = best
                    .as_ref()
                    .is_none_or(|(_, current)| bytes.len() < current.len());
                if is_better {
                    best = Some((mime, bytes));
                }
            }
            Err(source) => {
                if first_error.is_none() {
                    first_error = Some((mime, source));
                }
            }
        }
    }

    if let Some((mime, bytes)) = best {
        return Ok((bytes, mime));
    }
    // best 为 None 意味着候选集合里没有一个 Ok；候选集合本身非空（`encode_best` 至少传入
    // Png/Jpeg 两项），所以 first_error 在这个分支下必然是 Some——用 UnknownFormat 兜底
    // 只是类型系统需要一个 else 分支，不代表这条路径预期会被走到。
    let (mime, source) = first_error.ok_or(ImageError::UnknownFormat)?;
    Err(ImageError::Encode { mime, source })
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, image::ImageError> {
    let mut buf = Cursor::new(Vec::new());
    image.write_with_encoder(PngEncoder::new(&mut buf))?;
    Ok(buf.into_inner())
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, image::ImageError> {
    let mut buf = Cursor::new(Vec::new());
    image.write_with_encoder(JpegEncoder::new_with_quality(&mut buf, quality))?;
    Ok(buf.into_inner())
}

fn encode_webp(image: &DynamicImage) -> Result<Vec<u8>, image::ImageError> {
    let mut buf = Cursor::new(Vec::new());
    // image 0.25 的 WebP 编码器只支持无损（VP8L）；有损 WebP 需要 libwebp，不在依赖白名单内。
    image.write_with_encoder(WebPEncoder::new_lossless(&mut buf))?;
    Ok(buf.into_inner())
}

/// 若初次编码竞速的结果仍超 `options.max_bytes`，依次尝试“降质量”再“降尺寸”两个阶段，
/// 返回过程中找到的字节数最小的结果（够不到预算时也返回目前最好的，而不是报错）。
fn shrink_to_budget(
    base: &DynamicImage,
    initial: (Vec<u8>, ImageMime),
    target_width: u32,
    target_height: u32,
    options: &ResizeOptions,
    min_dimension: u32,
) -> (Vec<u8>, ImageMime, u32, u32) {
    let (mut best_bytes, mut best_mime) = initial;
    let mut best_width = target_width;
    let mut best_height = target_height;

    if best_bytes.len() <= options.max_bytes {
        return (best_bytes, best_mime, best_width, best_height);
    }

    // 阶段一：只降 JPEG 质量，尺寸不变。PNG 是无损格式、本 crate 的 WebP 编码器也只支持
    // 无损（见 `encode_webp`），两者都没有质量旋钮可调，因此这一阶段只重编 JPEG。
    for &quality in JPEG_QUALITY_LADDER
        .iter()
        .filter(|&&q| q < options.jpeg_quality)
    {
        if let Ok(candidate) = encode_jpeg(base, quality)
            && candidate.len() < best_bytes.len()
        {
            let fits = candidate.len() <= options.max_bytes;
            best_bytes = candidate;
            best_mime = ImageMime::Jpeg;
            if fits {
                return (best_bytes, best_mime, best_width, best_height);
            }
        }
    }

    // 阶段二：质量降到底仍超预算，只能牺牲尺寸——尺寸损失不可逆，质量损失可接受，所以
    // 尺寸永远是最后手段。迭代轮用便宜的 Triangle 滤波（多轮重采样，没必要每轮都用
    // 保真但更贵的 Lanczos3）。地板钳在 `min_dimension`，绝不跌破。
    for &(num, den) in &SCALE_LADDER {
        let scaled_width = scale_dim(target_width, num, den).max(min_dimension);
        let scaled_height = scale_dim(target_height, num, den).max(min_dimension);
        if scaled_width == best_width && scaled_height == best_height {
            continue;
        }

        let shrunk = base.resize_exact(scaled_width, scaled_height, FilterType::Triangle);
        if let Ok((candidate_bytes, candidate_mime)) =
            encode_best(&shrunk, options.jpeg_quality, options.allow_webp)
        {
            let fits = candidate_bytes.len() <= options.max_bytes;
            if candidate_bytes.len() < best_bytes.len() {
                best_bytes = candidate_bytes;
                best_mime = candidate_mime;
                best_width = scaled_width;
                best_height = scaled_height;
            }
            if fits {
                return (best_bytes, best_mime, best_width, best_height);
            }
        }
    }

    (best_bytes, best_mime, best_width, best_height)
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageFormat, RgbImage};

    use super::*;

    /// 生成一张纯色 PNG：纯色区域压缩比极高，天然适合验证快路径（小尺寸 + 小字节数）。
    fn solid_png(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(
            width,
            height,
            image::Rgb([120, 40, 200]),
        ));
        let mut buf = Cursor::new(Vec::new());
        image
            .write_to(&mut buf, ImageFormat::Png)
            .expect("内存缓冲区写入不会失败");
        buf.into_inner()
    }

    /// 生成一张确定性伪随机噪声 PNG：噪声几乎不可压缩，用于把字节数推过快路径阈值，
    /// 同时保持尺寸在合规范围内——专门用来测试“尺寸不变但仍需重编码”的场景。
    fn noisy_png(width: u32, height: u32) -> Vec<u8> {
        let mut state: u32 = 0x1234_5678;
        let mut buffer = RgbImage::new(width, height);
        for pixel in buffer.pixels_mut() {
            // xorshift32：确定性、无需外部随机数依赖。
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let bytes = state.to_le_bytes();
            *pixel = image::Rgb([
                bytes.first().copied().unwrap_or(0),
                bytes.get(1).copied().unwrap_or(0),
                bytes.get(2).copied().unwrap_or(0),
            ]);
        }
        let image = DynamicImage::ImageRgb8(buffer);
        let mut buf = Cursor::new(Vec::new());
        image
            .write_to(&mut buf, ImageFormat::Png)
            .expect("内存缓冲区写入不会失败");
        buf.into_inner()
    }

    /// PNG 规范用的标准 CRC-32/ISO-HDLC（多项式 0xEDB88320，反射输入输出，初值/终值取反）
    /// ——逐位实现，只用来给测试里手写的 IHDR chunk 配一个能通过校验的 CRC，17 字节的
    /// 输入量级不值得为此拉一个查表实现或额外依赖。
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        crc ^ 0xFFFF_FFFF
    }

    /// 手工拼一个「PNG 签名 + IHDR + 一个空 IDAT 的 8 字节 chunk 头」的极小文件：IHDR 里
    /// 声明 `width`×`height`。`image` crate 的 PNG 解码器在拿到宽高后还会无条件调用底层
    /// `read_info()`，它要求扫到 IDAT chunk 的起始边界（`ChunkBegin` 事件）才肯返回，
    /// 否则报 `MissingImageData`——单独一个 IHDR 不够。但 `ChunkBegin` 只需要 IDAT 的
    /// 8 字节头（4 字节长度 + 4 字节类型 "IDAT"）就会触发，不需要真正的 chunk 数据或
    /// CRC，所以这里只追加这 8 个字节：足以让 `probe_dimensions`/解码器拿到（伪造的）
    /// 尺寸，又小到不会意外真的把像素解出来——专门用来验证像素预算校验发生在任何
    /// 解码/分配之前。
    fn oversized_ihdr_only_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

        let mut chunk = Vec::with_capacity(17);
        chunk.extend_from_slice(b"IHDR");
        chunk.extend_from_slice(&width.to_be_bytes());
        chunk.extend_from_slice(&height.to_be_bytes());
        chunk.push(8); // bit depth
        chunk.push(6); // color type：RGBA
        chunk.push(0); // compression method
        chunk.push(0); // filter method
        chunk.push(0); // interlace method

        bytes.extend_from_slice(&13_u32.to_be_bytes()); // IHDR 数据长度固定 13 字节
        bytes.extend_from_slice(&chunk);
        bytes.extend_from_slice(&crc32(&chunk).to_be_bytes());

        // 只写 IDAT 的 8 字节 chunk 头（长度=0 + 类型名），没有数据、没有 CRC——
        // ChunkBegin 事件在此刻就已经触发，读到这里为止就足够了。
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(b"IDAT");
        bytes
    }

    #[test]
    fn small_image_takes_fast_path_without_reencoding() {
        // 256x256 落在 [min_dimension, max_width/height] 区间内，纯色内容压缩后远小于
        // max_bytes/4，两个快路径条件都满足。
        let bytes = solid_png(256, 256);
        let original = bytes.clone();
        let result = process_image(&bytes, &ResizeOptions::default()).expect("合法 PNG 应处理成功");

        assert!(!result.reencoded);
        assert_eq!(result.bytes, original);
        assert_eq!((result.width, result.height), (256, 256));
        assert_eq!((result.original_width, result.original_height), (256, 256));
        assert!(result.dimension_note().is_none());
    }

    #[test]
    fn oversized_image_is_scaled_down_to_max_dimension() {
        // 3:2 的常见照片比例，避免触发 min_dimension 放大分支，单独验证下缩逻辑。
        let bytes = noisy_png(3000, 2000);
        let result = process_image(&bytes, &ResizeOptions::default()).expect("合法 PNG 应处理成功");

        assert!(result.reencoded);
        assert!(result.width <= 1568 && result.height <= 1568);
        assert_eq!(result.width.max(result.height), 1568);
        assert!(result.width >= 200 && result.height >= 200);
        assert_eq!(
            (result.original_width, result.original_height),
            (3000, 2000)
        );
        assert!(result.dimension_note().is_some());
    }

    #[test]
    fn sub_min_dimension_image_is_upscaled_to_floor() {
        let bytes = solid_png(40, 60);
        let result = process_image(&bytes, &ResizeOptions::default()).expect("合法 PNG 应处理成功");

        assert!(result.reencoded);
        assert_eq!(result.width.min(result.height), 200);
        assert!(result.width >= 200 && result.height >= 200);
        assert_eq!((result.original_width, result.original_height), (40, 60));
        assert!(result.dimension_note().is_some());
    }

    #[test]
    fn dimension_note_is_none_when_only_bytes_shrank() {
        // 300x300 已经在 [min_dimension, max_width/height] 范围内，但噪声内容让字节数
        // 超过快路径阈值（max_bytes/4 = 125_000），因此会被重编码——但尺寸不应该变。
        let bytes = noisy_png(300, 300);
        assert!(bytes.len() > ResizeOptions::default().max_bytes / 4);

        let result = process_image(&bytes, &ResizeOptions::default()).expect("合法 PNG 应处理成功");

        assert!(result.reencoded);
        assert_eq!((result.width, result.height), (300, 300));
        assert!(result.dimension_note().is_none());
    }

    #[test]
    fn oversized_input_is_rejected_before_decoding() {
        let bytes = vec![0_u8; MAX_INPUT_BYTES + 1];
        let err = process_image(&bytes, &ResizeOptions::default()).expect_err("超限输入应报错");
        assert!(matches!(err, ImageError::TooLarge { limit, .. } if limit == MAX_INPUT_BYTES));

        let err = probe_dimensions(&bytes).expect_err("超限输入应报错");
        assert!(matches!(err, ImageError::TooLarge { .. }));
    }

    #[test]
    fn oversized_declared_dimensions_are_rejected_before_any_decoding() {
        // 输入本身只有 37 字节，远小于 MAX_INPUT_BYTES，字节数上限挡不住这种构造；
        // 必须靠探测到的 50000×50000 声明尺寸触发 TooManyPixels，且不能长时间挂起或 OOM。
        let bytes = oversized_ihdr_only_png(50_000, 50_000);
        assert!(bytes.len() < 64);

        let err = process_image(&bytes, &ResizeOptions::default())
            .expect_err("巨幅声明尺寸应在解码前被拒绝");
        assert!(matches!(
            err,
            ImageError::TooManyPixels {
                width: 50_000,
                height: 50_000,
                ..
            }
        ));
    }

    #[test]
    fn unrecognized_bytes_report_unknown_format_not_silent_passthrough() {
        let junk = [0_u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

        let probe_err = probe_dimensions(&junk).expect_err("无法识别的字节应报错");
        assert!(matches!(probe_err, ImageError::UnknownFormat));

        let process_err = process_image(&junk, &ResizeOptions::default())
            .expect_err("无法识别的字节应报错，不能静默返回原图");
        assert!(matches!(process_err, ImageError::UnknownFormat));
    }

    #[test]
    fn probe_dimensions_matches_known_image() {
        let bytes = solid_png(123, 45);
        let (width, height, mime) = probe_dimensions(&bytes).expect("合法 PNG 应探测成功");
        assert_eq!((width, height), (123, 45));
        assert_eq!(mime, ImageMime::Png);
    }

    #[test]
    fn mul_div_round_rounds_to_nearest_and_never_returns_zero() {
        assert_eq!(mul_div_round(10, 1, 3), 3); // 3.33 -> 3
        assert_eq!(mul_div_round(10, 2, 3), 7); // 6.67 -> 7
        assert_eq!(mul_div_round(1, 1, 1000), 1); // 会被地板钳到 1，不允许退化成 0
    }
}
