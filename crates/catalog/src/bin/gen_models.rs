//! `models.json` 生成器：抓取上游快照，归一成本仓的 [`CatalogFile`] 形态后落盘。
//!
//! ```text
//! cargo run -p zcode-catalog --features gen --bin gen-models
//! ```
//!
//! 设计约束（见 `rule://zcode-architecture` 的「生成物禁改」）：
//!
//! * **构建可复现**：只读一个公开只读 URL，不读本机凭据、不发带 key 的 discovery 请求。
//!   同一份上游快照在任何机器上产出的字节完全一致——这是 oh-my-pi 生成器
//!   （`packages/catalog/scripts/generate-models.ts:80-113` 读 env + 本机 `agent.db`）
//!   最大的债，本仓明确不继承。
//! * **确定性**：容器一律 `BTreeMap`，序列化用紧凑格式，产出即可 diff。
//! * **不猜定价**：上游没给 `cost` 的条目就是 `None`，不用 0 冒充免费。

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;

use anyhow::{Context as _, bail};
use serde::Deserialize;
use zcode_catalog::spec::{
    CatalogFile, CostSpec, LimitSpec, Modality, ModelSpec, ModelStatus, ProviderSpec, TierCostSpec,
};

/// 上游快照地址。zstd 压缩的 models.dev 形态目录，公开只读。
const UPSTREAM_URL: &str = "https://catalog.stencil.so/models.json.zstd";

/// 解压后的合理上限。上游 2026-08 实测约 1.7 MB；16 MB 给足十倍余量，
/// 同时挡住"上游被替换成 zip bomb"这种情况下的无界内存分配。
const MAX_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;

fn main() -> anyhow::Result<()> {
    // reqwest 用 `rustls-no-provider`（见根 Cargo.toml 的注释），必须在建 Client 前装 provider。
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        bail!("安装 rustls crypto provider 失败");
    }

    let runtime = tokio::runtime::Runtime::new().context("创建 tokio runtime 失败")?;
    let compressed = runtime.block_on(fetch(UPSTREAM_URL))?;
    let raw = decompress(&compressed)?;

    let upstream: BTreeMap<Box<str>, UpstreamProvider> =
        serde_json::from_slice(&raw).context("解析上游 JSON 失败")?;
    let catalog = convert(upstream);

    let providers = catalog.len();
    let models = catalog.values().map(|p| p.models.len()).sum::<usize>();
    if models == 0 {
        bail!("上游快照没有任何模型，拒绝写入空目录");
    }

    let mut out = serde_json::to_string(&catalog).context("序列化目录失败")?;
    out.push('\n');

    let path = output_path();
    std::fs::write(&path, out.as_bytes())
        .with_context(|| format!("写入 {} 失败", path.display()))?;

    println!(
        "已写入 {}：{providers} 个提供商 / {models} 个模型 / {} 字节",
        path.display(),
        out.len()
    );
    Ok(())
}

/// 产出路径：`<crate>/src/models.json`。
fn output_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("models.json")
}

async fn fetch(url: &str) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("zcode-catalog/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("构建 HTTP 客户端失败")?;
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("请求 {url} 失败"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("上游返回 {status}");
    }
    let bytes = response.bytes().await.context("读取上游响应体失败")?;
    Ok(bytes.to_vec())
}

fn decompress(compressed: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut decoder = zstd::Decoder::new(compressed).context("初始化 zstd 解码器失败")?;
    let mut raw = Vec::new();
    // `take` 而非直接 `read_to_end`：上游是外部输入，必须有上限。
    decoder
        .by_ref()
        .take(u64::try_from(MAX_DECOMPRESSED_BYTES).unwrap_or(u64::MAX))
        .read_to_end(&mut raw)
        .context("zstd 解压失败")?;
    if raw.len() >= MAX_DECOMPRESSED_BYTES {
        bail!("上游解压后超过 {MAX_DECOMPRESSED_BYTES} 字节上限");
    }
    Ok(raw)
}

fn convert(upstream: BTreeMap<Box<str>, UpstreamProvider>) -> CatalogFile {
    upstream
        .into_iter()
        .filter_map(|(id, provider)| {
            let models: BTreeMap<Box<str>, ModelSpec> = provider
                .models
                .into_iter()
                .map(|(model_id, model)| (model_id, convert_model(model)))
                .collect();
            if models.is_empty() {
                return None;
            }
            let spec = ProviderSpec {
                id: id.clone(),
                name: provider.name.unwrap_or_else(|| id.clone()),
                api: provider.api,
                env: provider.env.into_boxed_slice(),
                models,
            };
            Some((id, spec))
        })
        .collect()
}

fn convert_model(model: UpstreamModel) -> ModelSpec {
    ModelSpec {
        id: model.id.clone(),
        name: model.name.unwrap_or_else(|| model.id.clone()),
        cost: model.cost.map(convert_cost),
        limit: LimitSpec {
            context: positive(model.limit.context),
            output: positive(model.limit.output),
            input: positive(model.limit.input),
        },
        input: convert_modalities(model.modalities.input),
        output: convert_modalities(model.modalities.output),
        reasoning: model.reasoning,
        tool_call: model.tool_call,
        status: model.status.as_deref().and_then(convert_status),
    }
}

fn convert_cost(cost: UpstreamCost) -> CostSpec {
    CostSpec {
        input: cost.input,
        output: cost.output,
        cache_read: cost.cache_read,
        cache_write: cost.cache_write,
        context_over_200k: cost.context_over_200k.map(|tier| TierCostSpec {
            input: tier.input,
            output: tier.output,
            cache_read: tier.cache_read,
            cache_write: tier.cache_write,
        }),
    }
}

fn convert_modalities(raw: Vec<Box<str>>) -> Box<[Modality]> {
    let mut out: Vec<Modality> = raw
        .into_iter()
        .filter_map(|m| match &*m {
            "text" => Some(Modality::Text),
            "image" => Some(Modality::Image),
            "pdf" => Some(Modality::Pdf),
            "audio" => Some(Modality::Audio),
            "video" => Some(Modality::Video),
            // 上游新增模态时静默忽略，而不是让整次生成失败：目录的价值在广度，
            // 一个没见过的模态名不该拖垮 6000 条其他记录。
            _ => None,
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out.into_boxed_slice()
}

fn convert_status(raw: &str) -> Option<ModelStatus> {
    match raw {
        "alpha" => Some(ModelStatus::Alpha),
        "beta" => Some(ModelStatus::Beta),
        "deprecated" => Some(ModelStatus::Deprecated),
        _ => None,
    }
}

/// 上游用 `0` 表示"没这个信息"，本仓一律映射成 `None`（见 [`LimitSpec`] 的文档）。
fn positive(value: Option<u32>) -> Option<u32> {
    value.filter(|v| *v > 0)
}

// ── 上游形态（models.dev schema），只在本生成器内可见 ───────────────────────

#[derive(Debug, Deserialize)]
struct UpstreamProvider {
    #[serde(default)]
    name: Option<Box<str>>,
    #[serde(default)]
    api: Option<Box<str>>,
    #[serde(default)]
    env: Vec<Box<str>>,
    #[serde(default)]
    models: BTreeMap<Box<str>, UpstreamModel>,
}

#[derive(Debug, Deserialize)]
struct UpstreamModel {
    id: Box<str>,
    #[serde(default)]
    name: Option<Box<str>>,
    #[serde(default)]
    cost: Option<UpstreamCost>,
    #[serde(default)]
    limit: UpstreamLimit,
    #[serde(default)]
    modalities: UpstreamModalities,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    status: Option<Box<str>>,
}

#[derive(Debug, Deserialize)]
struct UpstreamCost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Option<f64>,
    #[serde(default)]
    context_over_200k: Option<UpstreamTierCost>,
}

#[derive(Debug, Deserialize)]
struct UpstreamTierCost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct UpstreamLimit {
    #[serde(default)]
    context: Option<u32>,
    #[serde(default)]
    output: Option<u32>,
    #[serde(default)]
    input: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct UpstreamModalities {
    #[serde(default)]
    input: Vec<Box<str>>,
    #[serde(default)]
    output: Vec<Box<str>>,
}
