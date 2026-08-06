// dep-boundary.check.ts —— workspace 内部 crate 依赖方向 ratchet，跑一次 cargo metadata
// 校验三条边界规则并打印报告。
//
// 用途：
//   ZCode 正在做"客户端 ↔ agent 运行时"跨进程边界的前置改造：`zcode-protocol` 是唯一
//   允许跨越该边界的 crate（wire 数据契约），`zcode-tui`（客户端渲染）与运行时 crate
//   （`zcode-agent`/`zcode-ai`/`zcode-catalog`）之间不应有编译期依赖——一旦有，就说明
//   有代码绕开 wire 协议直接拿运行时内部类型，跨进程边界就形同虚设。本脚本把这条约束
//   钉成可执行检查，不靠 code review 肉眼盯 Cargo.toml。
//
// 为什么不能照抄 jcode 的 scripts/check_dependency_boundaries.py：
//   该脚本的 FORBIDDEN_INTERNAL_DEPS 表（jcode/scripts/check_dependency_boundaries.py:26-51）
//   只保护 `jcode-*-types` 这类数据契约 crate，不许它们反向依赖运行时/UI/存储 crate——
//   护栏方向是"type crate 不许往外伸手"。它完全没有对称检查"UI crate 不许把整个运行时
//   吞进来"这条边：`crates/jcode-tui/src/lib.rs:23` 用
//   `pub use jcode_app_core::*;` 把 server/agent/provider/auth/session/tool/config 等
//   整个应用核心一次性转出到 `jcode-tui` 的公开 API 上，而该边界脚本检查的对象是
//   jcode-tui-core/jcode-tui-render 等 *-types 相邻 crate、不检查 jcode-tui 本身，
//   于是这条 tui→runtime 的依赖泄漏在 CI 里完全没有反应。ZCode 还没出现过 tui→runtime
//   的先例，本脚本的职责就是在它第一次出现前，把"tui 不得直接依赖运行时 crate"这条边
//   卡在 Cargo 依赖图这一层——比 review 更早、比扫描 re-export 语义更简单可靠。
//
// 三条规则（详见 checkBoundaries 内注释）：
//   1. zcode-protocol 不得依赖任何 workspace 内部 crate——协议 crate 就是边界本身。
//   2. zcode-tui 不得依赖运行时 crate（zcode-agent / zcode-ai / zcode-catalog）。
//      客户端只能经 zcode-protocol 的 wire 类型与运行时对话。
//   3. 运行时 crate（zcode-agent / zcode-ai / zcode-catalog / zcode-protocol /
//      zcode-utils）不得直接依赖 ratatui / crossterm（只查直接依赖，不递归传递依赖）：
//      daemon 进程运行这些 crate 时不该被迫拉进整套渲染栈。
//
//   `crates/coding-agent`（包名 `zcode`）是装配层——CLI 入口本来就要同时接线 tui 与
//   运行时才能跑起来，不受规则 2/3 约束（两者都依赖是它的职责本身，不是泄漏）。
//   它天然不在 RUNTIME_CRATES / TUI_CRATE 名单里，无需额外豁免逻辑。
//
// 数据源：`cargo metadata --format-version 1 --no-deps`（经 Bun.$ 子进程调用）。
//   加了 `--no-deps` 之后，返回的 `packages` 数组只含 workspace 成员本身，因此
//   "某个依赖名是否出现在 packages 里"就等价于"是不是 workspace 内部 crate"，
//   不需要再解析 `workspace_members` 的 pkgid 字符串来做交叉引用。
//
// 只看直接依赖：`dependencies[].kind === null` 对应 Cargo.toml 的 `[dependencies]` 小节；
// `"dev"` / `"build"` 分别是 `[dev-dependencies]` / `[build-dependencies]`，不计入——
// 测试专用依赖不会进最终产物，不影响 daemon/客户端的运行时边界。
//
// 用法：
//   bun .omp/checks/dep-boundary.check.ts [repoRoot]
//   省略 [repoRoot] 时取本文件所在目录的上上级目录（.omp/checks -> .omp -> 仓库根）。
//
// 退出码：
//   有任一违规，或 cargo metadata 本身执行失败 → 1；全部通过 → 0（可直接接入 CI）。

import * as path from "node:path";
import { fileURLToPath } from "node:url";

/** cargo metadata 里单条依赖项的最小字段集合（仅取本脚本用到的部分）。 */
interface CargoDependency {
  readonly name: string;
  readonly kind: string | null;
}

/** cargo metadata 里单个 workspace 成员包的最小字段集合。 */
interface CargoPackage {
  readonly name: string;
  readonly dependencies: readonly CargoDependency[];
}

/** `cargo metadata --format-version 1 --no-deps` 输出的最小字段集合。 */
interface CargoMetadata {
  readonly packages: readonly CargoPackage[];
}

/** 单条违规记录：结构化保留规则编号，便于未来按规则过滤或加测试断言。 */
interface Violation {
  readonly rule: 1 | 2 | 3;
  readonly message: string;
}

const PROTOCOL_CRATE = "zcode-protocol";
const TUI_CRATE = "zcode-tui";

/** 规则 2 禁止 zcode-tui 依赖的运行时 crate（不含 zcode-protocol/zcode-utils：那两个是允许的边界通道）。 */
const RUNTIME_ONLY_CRATES = ["zcode-agent", "zcode-ai", "zcode-catalog"] as const;

/** 规则 3 覆盖的运行时 crate 全集（daemon 进程实际会加载的那一侧）。 */
const RUNTIME_CRATES = [
  "zcode-agent",
  "zcode-ai",
  "zcode-catalog",
  "zcode-protocol",
  "zcode-utils",
] as const;

/** 规则 3 禁止运行时 crate 直接依赖的渲染栈 crate。 */
const RENDER_STACK_CRATES = ["ratatui", "crossterm"] as const;

function resolveRepoRoot(argv: string[]): string {
  const override = argv[2];
  if (override && override.length > 0) {
    return path.resolve(override);
  }
  const here = path.dirname(fileURLToPath(import.meta.url));
  // .omp/checks/dep-boundary.check.ts -> .omp/checks -> .omp -> 仓库根
  return path.resolve(here, "..", "..");
}

/**
 * 运行 `cargo metadata` 并解析为最小结构。
 * 子进程失败（cargo 缺失、Cargo.toml 损坏、workspace 无法解析）时让异常向上冒泡，
 * 由调用方决定如何呈现——本脚本不吞掉这类基础设施错误，否则会把它误判成"通过"。
 */
async function loadMetadata(repoRoot: string): Promise<CargoMetadata> {
  const raw = await Bun.$`cargo metadata --format-version 1 --no-deps`.cwd(repoRoot).quiet().text();
  return JSON.parse(raw) as CargoMetadata;
}

/** 某个包的直接（非 dev、非 build）依赖名列表，对应 Cargo.toml 的 `[dependencies]` 小节。 */
function directDependencyNames(pkg: CargoPackage): string[] {
  return pkg.dependencies.filter((dep) => dep.kind === null).map((dep) => dep.name);
}

function findPackage(metadata: CargoMetadata, name: string): CargoPackage | undefined {
  return metadata.packages.find((pkg) => pkg.name === name);
}

/** 跑三条规则，一次性收集全部违规（不 fail-fast），方便一次 CI 失败里把问题列全。 */
function checkBoundaries(metadata: CargoMetadata): Violation[] {
  const workspaceNames = new Set(metadata.packages.map((pkg) => pkg.name));
  const violations: Violation[] = [];

  // 规则 1：zcode-protocol 不得依赖任何 workspace 内部 crate——协议 crate 就是边界本身。
  const protocolPkg = findPackage(metadata, PROTOCOL_CRATE);
  if (protocolPkg) {
    const internalDeps = directDependencyNames(protocolPkg).filter((name) => workspaceNames.has(name));
    for (const dep of internalDeps) {
      violations.push({
        rule: 1,
        message:
          `[规则 1] ${PROTOCOL_CRATE} 依赖了 workspace 内部 crate "${dep}"。\n` +
          `  为什么禁止：${PROTOCOL_CRATE} 就是客户端 ↔ 运行时跨进程边界本身，一旦它反过来` +
          `依赖其他内部 crate，就不再是纯粹的 wire 数据契约，边界失去意义。\n` +
          `  正确做法：把 "${dep}" 需要共享给协议层的类型下沉/重新定义到 ${PROTOCOL_CRATE} ` +
          `自己的 wire 模块里，或者反过来让 "${dep}" 依赖 ${PROTOCOL_CRATE}，绝不能反向依赖。`,
      });
    }
  }

  // 规则 2：zcode-tui 不得依赖运行时 crate；客户端只能经 zcode-protocol 与运行时对话。
  const tuiPkg = findPackage(metadata, TUI_CRATE);
  if (tuiPkg) {
    const tuiDeps = new Set(directDependencyNames(tuiPkg));
    for (const forbidden of RUNTIME_ONLY_CRATES) {
      if (tuiDeps.has(forbidden)) {
        violations.push({
          rule: 2,
          message:
            `[规则 2] ${TUI_CRATE} 依赖了运行时 crate "${forbidden}"。\n` +
            `  为什么禁止：客户端（tui）与 agent 运行时要跨进程分离，只能经 ${PROTOCOL_CRATE} ` +
            `的 wire 类型通信；直接依赖运行时 crate 会让编译期耦合先于进程边界固化，将来把两者` +
            `拆成独立进程时这条依赖会被迫留一个空壳或硬拆返工。\n` +
            `  正确做法：${TUI_CRATE} 只能通过 ${PROTOCOL_CRATE} 暴露的 Request/Reply/Event 等 ` +
            `wire 类型与运行时交互，不得直接 import "${forbidden}" 的任何类型或函数。`,
        });
      }
    }
  }

  // 规则 3：运行时 crate 不得直接依赖渲染栈；daemon 进程不该被迫拉进 ratatui/crossterm。
  for (const runtimeCrate of RUNTIME_CRATES) {
    const pkg = findPackage(metadata, runtimeCrate);
    if (!pkg) {
      continue;
    }
    const deps = new Set(directDependencyNames(pkg));
    for (const renderDep of RENDER_STACK_CRATES) {
      if (deps.has(renderDep)) {
        violations.push({
          rule: 3,
          message:
            `[规则 3] ${runtimeCrate} 直接依赖了渲染栈 crate "${renderDep}"。\n` +
            `  为什么禁止：${runtimeCrate} 是可能以无头 daemon 进程形态运行的运行时 crate，不该` +
            `被迫编译/链接终端渲染栈（ratatui/crossterm）——那是客户端（${TUI_CRATE}）的职责，` +
            `daemon 侧引入它只会拖慢编译、扩大攻击面，且违反跨进程边界的设计意图。\n` +
            `  正确做法：把渲染相关代码移到 ${TUI_CRATE}，${runtimeCrate} 只通过 ${PROTOCOL_CRATE} ` +
            `的数据类型向外传递信息，不感知具体渲染方式。`,
        });
      }
    }
  }

  return violations;
}

async function main(): Promise<void> {
  const repoRoot = resolveRepoRoot(process.argv);
  console.log(`dep-boundary 校验目标仓库：${repoRoot}`);
  console.log(
    `规则：1) ${PROTOCOL_CRATE} 不依赖任何内部 crate；` +
      `2) ${TUI_CRATE} 不依赖 [${RUNTIME_ONLY_CRATES.join(", ")}]；` +
      `3) [${RUNTIME_CRATES.join(", ")}] 不直接依赖 [${RENDER_STACK_CRATES.join(", ")}]`,
  );
  console.log("豁免：crates/coding-agent（包名 zcode）是装配层，不受规则 2/3 约束。");
  console.log("");

  let metadata: CargoMetadata;
  try {
    metadata = await loadMetadata(repoRoot);
  } catch (err) {
    console.error("cargo metadata 执行失败，无法校验依赖边界：");
    console.error(err instanceof Error ? err.message : String(err));
    process.exitCode = 1;
    return;
  }

  const violations = checkBoundaries(metadata);

  if (violations.length === 0) {
    console.log("全部通过：未发现依赖方向违规。");
    process.exitCode = 0;
    return;
  }

  console.error(`发现 ${violations.length} 处依赖方向违规：`);
  console.error("");
  for (const violation of violations) {
    console.error(violation.message);
    console.error("");
  }
  process.exitCode = 1;
}

// 只有直接 `bun .omp/checks/dep-boundary.check.ts` 运行时才执行，被别处 import 时不产生副作用。
if (import.meta.main) {
  await main();
}
