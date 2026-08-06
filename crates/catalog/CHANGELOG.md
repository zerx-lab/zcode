# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]

### Added

- 初始化 crate 骨架：继承 workspace 元数据与 lint 配置。
- `spec`：`models.json` 的磁盘契约，生成器与运行时共用。容器一律 `BTreeMap`、
  字段顺序由声明顺序固定，因此同一份上游快照在任何机器上产出的字节完全一致。
- `bin/gen-models`（`--features gen`）：从 `https://catalog.stencil.so/models.json.zstd`
  抓取并归一，写入 `src/models.json`（当前 180 个提供商 / 6149 个模型）。
  **只读一个公开只读 URL**，不读本机凭据、不发带 key 的 discovery 请求——oh-my-pi 的生成器
  （`packages/catalog/scripts/generate-models.ts:80-113`）读 env + 本机 `agent.db` 再发真实请求，
  导致不同机器产出不同的 `models.json`，那是债，不继承。
- `models`：两级惰性加载（顶层 `RawValue` 只切子串，单个提供商首次访问时才解析成
  `Arc<ProviderSpec>`）、`calculate_cost` / `cache_write_cost`。
- `descriptors`：16 条提供商描述符（base URL、环境变量、默认模型、发现能力、线格式），
  并用测试断言每条非 `discovery_only` 的 id 与 `default_model` 在 `models.json` 里真实存在
  ——上游的描述符表与生成物已经漂移且用不安全 cast 掩盖，缺的正是这个一致性检查。
- `effort` / `thinking`：7 档 `Effort` 与 `ThinkingConfig`（effort / budget / google-level /
  anthropic-adaptive 四种控制模式，定长表零分配）。
- `identity`：模型 id 的解析、族判定、展示名清洗。全部是无缓存纯函数——上游的无界 memo
  声称 key 来自"有界集合"，但代理发现路径会把远端 `/v1/models` 的任意 id 喂进来，该前提已被证伪。
- `cache`：运行时发现结果的 SQLite 落盘缓存。
- `manager`：静态目录 / 缓存 / 本轮发现结果的仲裁与新鲜度判定。**不发 HTTP 请求**——
  `zcode_ai::http` 是全 workspace 唯一的 `reqwest` 客户端而 `zcode-ai` 反过来依赖本 crate，
  拉取由调用方完成后把字节交进来。

### Changed

- `zcode-ai` 的 `ReasoningEffort` 已删除，`Thinking::Effort` 改用本 crate 的 `Effort`
  并由 `zcode_ai` re-export。模型目录才是档位集合的事实来源
  （`rule://zcode-architecture` 的 catalog 导入边界）。线上取值不变，`Effort::Off` 写作 `"none"`。

### Notes

- **定价未知绝不按 0。** 上游约 7% 的条目不给价，记成 0 会让成本统计静默偏低；
  `cost` 缺失时 `calculate_cost` 返回 `None`，某个分量单价未知而该分量用量非零时同样返回 `None`。
- **缓存只有一条失效通道**：`(SCHEMA_VERSION, static_fingerprint)`。上游并行维护三套
  （schema version / fingerprint version / 手工 bump 的 cache-provider-id），正是红线所禁的
  "并行的第二套写法"。
- **失效只能删行或加列**，绝不写版本提升式迁移——上游早期 `UPDATE ... WHERE version = 2`
  把最初版本静默提升到最新，废掉了此后每一次失效（#4146）。
- **headers 永不落盘**，且从类型签名上就没有接受 header 的入口：自定义提供商可用任意
  header 名承载凭据，没有基于名字的过滤能做到完备。
- `CACHE_TTL = 2h` 沿用上游取值，但上游未给出前提（无 bench、无 issue），本仓亦未实测。
