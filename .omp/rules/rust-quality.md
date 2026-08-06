---
description: Rust 代码质量硬约束：错误类型、数值转换、导出与可见性、异步、依赖与 lint 集中管理。编辑或新增 .rs / Cargo.toml 前必读。
globs:
  - "**/*.rs"
  - "**/Cargo.toml"
---

# Rust 代码质量

## 错误处理

- 库 crate：用 `thiserror` 定义具体错误枚举，绝不用 `Box<dyn Error>`/字符串错误。
- 二进制/顶层流程：用 `anyhow::Result<T>`，在调用点用 `.context("做什么时失败")` 附加语境。
- 库代码绝不用 `unwrap()`/`expect()`/`panic!()` 处理可恢复错误。唯一例外：违反的是内部不变量，且同行/上一行注释说明原因：
  ```rust
  let first = items.first().expect("items 已在上方校验非空");
  ```
- 测试代码不受此限（见 `rule://rust-testing`）。

## 数值转换

- 绝不用 `as` 做数值收窄/跨符号转换。用 `TryFrom`/`try_into()` 并处理溢出：
  ```rust
  let n: u32 = len.try_into().context("长度超出 u32 范围")?;
  ```
- `as` 仅允许用于确定无损的方向（如 `u8 as u32`、`u32 as u64`），且应在同行注释说明“无损”。

## 万能类型

- 除非绝对必要，禁止 `Box<dyn Any>`/`serde_json::Value` 作为参数或返回类型的“万能容器”——定义具体结构体/枚举，让编译器检查字段。
- 确需处理任意 JSON（如透传未知 schema）时，`serde_json::Value` 仅允许出现在该边界的最小范围内，不得向上传播。

## 导入与导出

- 所有 `use` 写在模块顶部；绝不在函数体内 `use`（测试模块的 `use super::*` 除外）。
- 绝不用内联路径拼接（`<Foo as Bar>::` 之外的写法）规避导入。
- 模块导出优先星号 re-export，即使只导出一个标识符：
  ```rust
  pub use crate::config::*;
  ```
  而不是逐项命名 `pub use crate::config::{Config, ConfigError};`。星号 re-export 造成命名歧义时，删除冗余的导出路径，不保留重复项。

## 可见性

- 默认私有；跨模块共享用 `pub(crate)`；只有真正对外的 API 才用 `pub`。
- 绝不为了让测试通过而放宽可见性——测试放进同模块的 `#[cfg(test)] mod tests`，它能访问私有项。

## 异步

- 统一用 `tokio`；绝不在 async 上下文里调用阻塞 API。
- CPU 密集或阻塞调用（同步文件系统大批量操作、CPU 计算、FFI 阻塞调用）用 `tokio::task::spawn_blocking`：
  ```rust
  let hash = tokio::task::spawn_blocking(move || blake3::hash(&bytes)).await?;
  ```
- 绝不用 `futures::executor::block_on` 在 async 里嵌套另一个运行时。

## 共享状态

- 优先 `Arc<T>` + 消息传递（`tokio::sync::mpsc`）而非共享可变状态。
- 确需可变共享时用 `Arc<Mutex<T>>`；只在需要跨 `.await` 持锁时才用 `tokio::sync::Mutex`，否则用 `std::sync::Mutex`。
- 绝不跨 `.await` 持有 `std::sync::MutexGuard`——会阻塞整个执行器线程，且在多线程 runtime 上可能死锁。

## 依赖与 lint 集中管理

- 依赖版本统一声明在 workspace 根 `Cargo.toml` 的 `[workspace.dependencies]`；各成员 crate 用 `dep.workspace = true`，绝不在单个 crate 里固定不同版本。
- lint 集中在 `[workspace.lints]`（含 `unwrap_used`/`expect_used`/`panic`/`as_conversions`/`cast_possible_truncation` 等 `deny`）；各成员 crate `Cargo.toml` 写 `[lints] workspace = true`，绝不在成员 crate 里另建一套 lint 配置。
- 绝不提交带 `#[allow(...)]` 的 lint 抑制来绕过告警，除非同行注释说明了原因。

## 标准库 / 生态优先，绝不外壳调用

对已有库 API 支持的操作，绝不启动 shell 命令或外部进程模拟：

| 操作       | 应用                                    | 不应用                          |
| ---------- | ---------------------------------------- | -------------------------------- |
| 文件 I/O   | `tokio::fs`（async）/`std::fs`（同步）   | `Command::new("cat")`            |
| 创建目录   | `fs::create_dir_all`                     | `mkdir -p` 子进程                |
| 启动进程   | `tokio::process::Command` + 参数数组     | `sh -c "..."`/`cmd /C "..."`     |
| 休眠       | `tokio::time::sleep(Duration)`           | `std::thread::sleep`（async 中） |
| 查找二进制 | 生态封装的 `which` 辅助函数              | `Command::new("which")`          |
| HTTP       | `reqwest`                                | `curl` 子进程                    |
| 哈希       | `sha2`/`blake3`；密码用 `argon2`         | 自实现哈希、`md5` 用于安全场景   |
| 随机数     | `rand`                                   | 基于时间戳自造随机               |
| 路径拼接   | `Path`/`PathBuf::join`/`push`            | 字符串拼接 `"/"`                 |
| 序列化     | `serde` + `serde_json`                   | 手写 JSON 解析/拼接              |
| 字符串宽度 | `unicode-width` 的 `UnicodeWidthStr`     | `len()`/`chars().count()`        |
| 错误类型   | 库 `thiserror`，应用 `anyhow`            | `Box<dyn Error>`、字符串错误     |
| 日志       | `tracing`                                | `println!`/`eprintln!`           |
| CLI 参数   | `clap`（derive）                         | 手写 `args()` 解析               |

- 进程一律用参数数组启动，绝不用 `sh -c`/`cmd /C` 拼接命令字符串（参数注入风险 + 跨平台行为不一致）；用户显式请求执行 shell 命令的工具实现是唯一例外。
- 长时间运行/需流式 stdin-stdout-stderr/需要信号或 `kill` 控制时，用 `Stdio::piped()` 并显式管理 `Child`；仅“触发后不等待”场景才 `spawn()` 后丢给独立任务收割，绝不泄漏僵尸进程。

## 先搜索，再实现，外部 API 不猜签名

- 写辅助函数前先 `grep` 搜索是否已有等价实现；即使两个实现都能跑，重复实现同一功能也是缺陷。
- 缺少能力时扩展现有辅助函数（新增参数/子函数），不要局部复制并分叉其逻辑。
- 使用外部 crate 前先查其源码（本地 registry 缓存）或 `cargo doc`，不得凭空猜测函数签名、字段名或返回类型。

## 日志

- 用 `tracing`（`error!`/`warn!`/`debug!`），带结构化字段：
  ```rust
  tracing::warn!(path = %path.display(), "回退到默认配置");
  ```
- 不进入 TUI/协议、执行后即退出的一次性 CLI 命令可用 `println!`/直接写 stdout 输出面向用户的信息；该例外由语义（是否会与其他渲染/协议共享同一 stdout）决定，而非文件名。
