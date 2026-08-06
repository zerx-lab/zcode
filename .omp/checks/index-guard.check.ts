// index-guard.check.ts —— 独立 CLI，跑一次完整的 AGENTS.md 索引校验并打印报告。
//
// 用途：
//   `../extensions/index-guard.ts` 里的确定性守卫只在 omp 会话内的事件上触发
//   （write/edit/bash、session_start、before_agent_start、session_stop）。
//   本文件是同一套检测逻辑（原样 import，单一事实来源，不复制）的命令行入口，
//   供人工随时核查，或接入 CI 作为一个独立检查步骤。
//
// 为什么放在 `.omp/checks/` 而不是 `.omp/extensions/`：
//   extension loader 会把 `.omp/extensions/` 下的**每个** .ts 都当扩展模块 dynamic import，
//   本文件不导出 factory，于是每次启动都会打印
//   `Failed to load extension ...: Extension does not export a valid factory function`。
//   已在真实 omp 会话中复现。`.omp/checks/` 不是 omp 的约定目录，不会被任何 loader 扫描。
//
// 用法：
//   bun .omp/checks/index-guard.check.ts [repoRoot]
//   node --experimental-strip-types .omp/checks/index-guard.check.ts [repoRoot]
//
//   省略 [repoRoot] 时，默认取本文件所在目录的上上级目录
//   （`.omp/checks/` → `.omp/` → 仓库根），对应本项目实际落盘位置。
//   传入 [repoRoot] 可指向任意目录（例如临时目录里的样例 AGENTS.md），
//   便于在不污染当前仓库的前提下验证 `(planned)` 豁免等边界行为。
//
// 退出码：
//   有任一违规 → 1；全部通过 → 0（便于直接接入 CI 的一个检查步骤）。
//
// 依赖的 omp API：无（本文件不是 omp extension，不引用 ExtensionAPI/ctx，
// 纯粹是 Bun/Node 可执行的 TS 脚本）。检测逻辑本身的文档依据见
// `../extensions/index-guard.ts` 顶部注释。

import * as path from "node:path";
import { fileURLToPath } from "node:url";
import {
  ANCHOR_AGE_DAYS_LIMIT,
  COMMITS_BEHIND_LIMIT,
  MAX_LINES,
  formatCompactReport,
  runIndexGuardCheck,
} from "../extensions/index-guard.ts";

function resolveRepoRoot(argv: string[]): string {
  const override = argv[2];
  if (override && override.length > 0) {
    return path.resolve(override);
  }
  const here = path.dirname(fileURLToPath(import.meta.url));
  // .omp/checks/index-guard.check.ts -> .omp/checks -> .omp -> 仓库根
  return path.resolve(here, "..", "..");
}

function main(): void {
  const repoRoot = resolveRepoRoot(process.argv);
  const report = runIndexGuardCheck(repoRoot);

  console.log(`index-guard 校验目标仓库：${repoRoot}`);
  console.log(
    `阈值：MAX_LINES=${MAX_LINES}，COMMITS_BEHIND_LIMIT=${COMMITS_BEHIND_LIMIT}，ANCHOR_AGE_DAYS_LIMIT=${ANCHOR_AGE_DAYS_LIMIT}`,
  );
  console.log("");
  console.log("结构化报告：");
  console.log(JSON.stringify(report, null, 2));
  console.log("");
  console.log(formatCompactReport(report));

  process.exitCode = report.ok ? 0 : 1;
}

// 只有直接 `bun .omp/checks/index-guard.check.ts` 运行时才执行，
// 被别处 import 时不产生任何副作用（不打印、不改 process.exitCode）。
if (import.meta.main) {
  main();
}
