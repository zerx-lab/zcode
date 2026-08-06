// index-guard.ts —— ZCode 根 AGENTS.md 记忆索引的确定性守卫（omp extension）。
//
// 用途：
//   - 防止根 `AGENTS.md`（记忆索引）膨胀回一份长文：写超限直接硬闸门拦截，
//     编辑/覆盖后软反馈违规详情。
//   - 会话开场做一次静默检测，仅在有违规时提示；`before_agent_start` 额外
//     注入一次结构化提醒（同一进程内最多一次，避免每回合重复刷屏）。
//   - 检测逻辑（行数上限 / 过时路径引用 / 新鲜度锚）以纯函数形式导出，供
//     `../checks/index-guard.check.ts`（独立 CLI，供人工/CI 运行）复用，两处逻辑单一来源。
//
// 依赖的 omp API 与文档出处（均已核对，未使用未在文档中出现的字段）：
//   - 默认导出工厂 `export default function (pi: ExtensionAPI)`：
//       omp://extensions.md「What an extension is」
//   - `pi.on(event, handler)` 注册、`ExtensionAPI` 核心方法列表：
//       omp://extensions.md「1) Registration and actions (ExtensionAPI)」
//   - handler 的 `ctx.cwd` / `ctx.ui`：
//       omp://extensions.md「2) Handler context (ExtensionContext)」
//   - `session_start` handler 签名 `(_event, ctx) => {...}`：
//       omp://extensions.md「Quick start」；omp://skills/authoring-extensions.md「Minimum viable extension」
//   - `tool_call` 事件字段 `event.toolName` / `event.input` / `event.toolCallId`，
//     以及 `{ block: true, reason }` 拦截契约：
//       omp://skills/authoring-extensions.md「Subscribing to events」
//       omp://skills/examples/safety-hook/README.md「What it demonstrates」
//   - `tool_call` handler 抛异常 = fail-closed（会阻断工具），因此本文件所有
//     handler 内部自行 try/catch、异常时静默放行（fail-open），仅主动返回
//     `{ block: true }` 时才是有意拦截：
//       omp://extensions.md「Constraints and pitfalls」
//       omp://skills/authoring-extensions.md「Important constraints」
//   - `tool_result` 事件字段 `toolName/toolCallId/input/content/details/isError`，
//     以及可返回 `{ content?, details?, isError? }` 覆盖、"middleware-style，
//     handlers run in extension order and each sees prior modifications"：
//       omp://hooks.md「Execution model and mutation semantics」§3
//       omp://extensions.md「Tool lifecycle」
//   - `before_agent_start` 可返回
//     `{ message?: { customType; content; display; details; attribution } }`：
//       omp://hooks.md「Agent/context events」
//       omp://skills/authoring-hooks.md 事件表
//     （`extensions.md` 只列出事件名未展开返回形状；
//      `omp://skills/authoring-extensions.md`「Extension vs hook」明确
//      "Extensions are a strict superset of hooks"，故沿用该返回契约）
//   - `custom_message.attribution` 合法值含 `"user"` / `"agent"`，
//     `content` 可为字符串，`customType` 须使用反向域名等命名空间前缀避免
//     与核心保留值冲突：
//       omp://session.md「custom_message」
//       omp://skills/authoring-extensions.md（`attribution: "user"` 示例）
//   - `registerCommand(name, { description, handler(args, ctx) })`：
//       omp://extensions.md「Quick start」
//       omp://skills/authoring-extensions.md「Registering commands」
//   - `write` 工具输入字段 `path` / `content`（全量替换内容）：
//       omp://tools/write.md「## Inputs」
//   - `edit` 工具输入字段 `input`（hashline 补丁串，每段以 `[PATH#TAG]` 开头）：
//       omp://tools/edit.md「## Input」「Canonical patch language」
//   - `.omp/extensions/` 项目级自动发现（`.ts`/`.js`，无需 settings 声明）：
//       omp://extension-loading.md「1) Auto-discovered native extension modules」
//       omp://skills/authoring-extensions.md「Discovery paths」
//
// 未在文档中找到（本文件未使用，仅记录以免臆造）：
//   - `ctx.ui.notify` 的 level 联合类型完整枚举（文档示例仅出现过 `"info"`，
//     故本文件仅使用 `"info"`）。
//   - `pi.logger` 的具体方法签名（仅确认其存在，本文件用可选链防御性调用）。
//
// 如何启用：
//   放在项目 `.omp/extensions/` 目录下即被自动发现并加载，无需在
//   `.omp/config.yml` 的 `extensions:` 或 `.omp/settings.json#extensions`
//   中额外声明（那是给项目外部路径用的）。如需临时禁用，可在
//   `.omp/config.yml` 写：
//     disabledExtensions:
//       - extension-module:index-guard
//   （派生名取自文件名主干，见 omp://extension-loading.md「Disable specific
//   extension modules」）。

import * as fs from "node:fs";
import * as path from "node:path";
import { spawnSync } from "node:child_process";

// ---------------------------------------------------------------------------
// 最小本地类型声明
//
// 刻意不写 `import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent"`：
// 本仓库尚未落盘 Cargo/Node 项目、没有 node_modules，无法确认该包名在此工作区
// 可解析；即便是 `import type` 也可能在某些加载路径下触发解析失败，进而让整个
// 扩展加载失败。这里只声明本文件实际用到的最小字段子集，字段命名与形状均对齐
// 上方引用的文档段落。
// ---------------------------------------------------------------------------

interface ExtUIContext {
  notify(message: string, level: "info"): void;
}

interface ExtContext {
  cwd: string;
  ui: ExtUIContext;
}

interface ToolCallEvent {
  toolName: string;
  toolCallId: string;
  input: Record<string, unknown>;
}

interface ToolCallHandlerResult {
  block?: boolean;
  reason?: string;
  input?: Record<string, unknown>;
}

interface ToolResultContentChunk {
  type: string;
  text?: string;
  [key: string]: unknown;
}

interface ToolResultEvent {
  toolName: string;
  toolCallId: string;
  input: Record<string, unknown>;
  content: ToolResultContentChunk[];
  details?: unknown;
  isError: boolean;
}

interface ToolResultHandlerResult {
  content?: ToolResultContentChunk[];
  details?: unknown;
  isError?: boolean;
}

interface BeforeAgentStartHandlerResult {
  message?: {
    customType: string;
    content: string;
    display?: boolean;
    details?: unknown;
    attribution?: "user" | "agent";
  };
}

type MaybePromise<T> = T | Promise<T>;

interface Logger {
  error?: (...args: unknown[]) => void;
  warn?: (...args: unknown[]) => void;
  info?: (...args: unknown[]) => void;
  debug?: (...args: unknown[]) => void;
}

interface ExtensionAPI {
  logger?: Logger;
  on(
    event: "session_start",
    handler: (event: unknown, ctx: ExtContext) => MaybePromise<void>,
  ): void;
  on(
    event: "tool_call",
    handler: (
      event: ToolCallEvent,
      ctx: ExtContext,
    ) => MaybePromise<ToolCallHandlerResult | void>,
  ): void;
  on(
    event: "tool_result",
    handler: (
      event: ToolResultEvent,
      ctx: ExtContext,
    ) => MaybePromise<ToolResultHandlerResult | void>,
  ): void;
  on(
    event: "before_agent_start",
    handler: (
      event: unknown,
      ctx: ExtContext,
    ) => MaybePromise<BeforeAgentStartHandlerResult | void>,
  ): void;
  registerCommand(
    name: string,
    def: {
      description: string;
      handler: (args: string, ctx: ExtContext) => MaybePromise<void>;
    },
  ): void;
}

// ---------------------------------------------------------------------------
// 常量（阈值集中在此，check.ts 与 extension 共用）
// ---------------------------------------------------------------------------

/** 根 AGENTS.md 文件名。 */
export const AGENTS_MD_FILENAME = "AGENTS.md";

/** 根索引行数上限（契约：AGENTS.md 索引契约，行数上限 120）。 */
export const MAX_LINES = 120;

/** 新鲜度锚落后 HEAD 的 commit 数上限，超过判定违规。 */
export const COMMITS_BEHIND_LIMIT = 30;

/** 新鲜度锚日期距今天数上限，超过判定违规。 */
export const ANCHOR_AGE_DAYS_LIMIT = 14;

/** 判定「跳过 fenced code block」用的三反引号围栏识别正则（行首，允许缩进）。 */
const FENCE_LINE = /^\s*```/;

/** 行内反引号 code span：`token`（不跨行）。 */
const INLINE_CODE = /`([^`\n]+)`/g;

/**
 * 尾部新鲜度锚：`<!-- index-verified: <commit-sha> <YYYY-MM-DD> -->`。
 * `none` 是 `git init` 之前的合法占位值 —— 此时无 sha 可写，不应判成缺锚。
 */
const ANCHOR_PATTERN =
  /<!--\s*index-verified:\s*([0-9a-fA-F]{7,40}|none)\s+(\d{4}-\d{2}-\d{2})\s*-->/g;

/** 判断路径候选的常见仓库文件扩展名。 */
const REPO_PATH_EXTENSIONS = [".rs", ".toml", ".md", ".yml", ".yaml", ".json"];

/** 排除：以这些前缀开头的 token（命令示例 / CLI flag，不是路径）。 */
const EXCLUDED_PREFIXES = ["cargo", "rustup", "git", "bun", "just", "npx", "-"];

/** 排除：这些 scheme 开头的 token（内部 URI，不是仓库路径）。 */
const EXCLUDED_SCHEMES = [
  "rule://",
  "skill://",
  "omp://",
  "agent://",
  "http://",
  "https://",
];

/** 标记某一行豁免过时路径检测的关键字（大小写不敏感）。 */
const PLANNED_MARKER = /\(planned\)/i;

/** 排除：slash command（如 `/gate`）—— 形如 /name，无第二个分隔符、无扩展名。 */
const SLASH_COMMAND = /^\/[A-Za-z][A-Za-z0-9:_-]*$/;

/** 排除：裸扩展名（如 `.rs`）—— 在讨论文件类型，不是引用某个路径。 */
const BARE_EXTENSION = /^\.[A-Za-z0-9]+$/;

/**
 * 反向检测（索引落后于仓库）时忽略的顶层目录名。
 *
 * 判据是「**项目结构**目录都该在索引里有坐标」，所以这里只排除两类：
 *   1. 工具/VCS 私有目录 —— 它们不是项目结构，由各自工具负责；
 *   2. 构建产物与依赖缓存 —— 可再生成，且通常已被 gitignore。
 *
 * 注意 `.github/` **不在**排除表里：CI 是项目结构的一部分，应该有坐标。
 * 判定某个目录确实不该进索引时，用索引里的 `<!-- index-ignore: a, b -->` 显式豁免，
 * 而不是靠"点目录一律跳过"这种和重要性无关的规则。
 */
const UNINDEXED_SCAN_IGNORE = new Set([
  // 工具 / VCS 私有
  ".git",
  ".jj",
  ".hg",
  ".svn",
  ".omp",
  ".claude",
  ".codex",
  ".gemini",
  ".cursor",
  ".vscode",
  ".idea",
  ".zed",
  ".cargo",
  ".husky",
  // 构建产物 / 依赖缓存
  "target",
  "node_modules",
  "dist",
  "build",
  "vendor",
  "__pycache__",
]);

/** 索引里的显式豁免声明：`<!-- index-ignore: plans, examples -->`（可出现多次，累加）。 */
const INDEX_IGNORE_PATTERN = /<!--\s*index-ignore:\s*([^>]*?)\s*-->/g;

// ---------------------------------------------------------------------------
// 报告结构
// ---------------------------------------------------------------------------

export interface IndexGuardReport {
  ok: boolean;
  lineCount: number;
  maxLines: number;
  staleRefs: string[];
  plannedRefs: string[];
  /** 顶层目录存在于磁盘，但 AGENTS.md 全文从未提及 —— 索引落后于仓库。 */
  unindexedDirs: string[];
  missingAnchor: boolean;
  anchorCommit?: string;
  headCommit?: string;
  commitsBehind?: number;
  anchorAgeDays?: number;
  notes: string[];
}

// ---------------------------------------------------------------------------
// 纯函数：解析 / 检测逻辑
// ---------------------------------------------------------------------------

/** 统计「有效行数」：末尾若因结尾换行产生一个空字符串元素则不计入。 */
export function countLines(content: string): number {
  const lines = content.split(/\r?\n/);
  if (lines.length > 0 && lines[lines.length - 1] === "") {
    lines.pop();
  }
  return lines.length;
}

interface InlineCodeHit {
  /** 1-based 行号，仅用于日志/调试，报告本身不暴露。 */
  line: number;
  /** 该 token 所在的完整原始行文本，用于 `(planned)` 豁免判定。 */
  lineText: string;
  /** 反引号内的原始 token。 */
  token: string;
}

/**
 * 提取所有「行内反引号」token，跳过三反引号 fenced code block 的全部内容
 * （围栏行本身与围栏内部都不产出 token）。
 */
function extractInlineCodeTokens(markdown: string): InlineCodeHit[] {
  const lines = markdown.split(/\r?\n/);
  const hits: InlineCodeHit[] = [];
  let inFence = false;

  for (let i = 0; i < lines.length; i++) {
    const rawLine = lines[i];
    if (FENCE_LINE.test(rawLine)) {
      inFence = !inFence;
      continue;
    }
    if (inFence) continue;

    INLINE_CODE.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = INLINE_CODE.exec(rawLine)) !== null) {
      hits.push({ line: i + 1, lineText: rawLine, token: match[1] });
    }
  }

  return hits;
}

/** 判断一个反引号 token 是否「看起来像仓库内路径」，并排除命令示例/内部 URI。 */
function isRepoPathCandidate(token: string): boolean {
  const trimmed = token.trim();
  if (trimmed.length === 0) return false;
  if (/\s/.test(trimmed)) return false;
  if (EXCLUDED_SCHEMES.some((scheme) => trimmed.startsWith(scheme))) return false;
  if (EXCLUDED_PREFIXES.some((prefix) => trimmed.startsWith(prefix))) return false;
  if (SLASH_COMMAND.test(trimmed)) return false;
  if (BARE_EXTENSION.test(trimmed)) return false;

  const lower = trimmed.toLowerCase();
  const looksLikePath =
    trimmed.includes("/") || REPO_PATH_EXTENSIONS.some((ext) => lower.endsWith(ext));
  return looksLikePath;
}

/**
 * 把候选 token 归约成一个用于磁盘存在性检测的相对路径：
 * - 允许尾部 `/`（去掉后按目录/文件通用检测）
 * - 含 `*` 时，只保留其最深的、不含通配符的前缀目录
 */
function toExistenceCheckRelPath(token: string): string {
  let candidate = token.trim();
  if (candidate.endsWith("/")) {
    candidate = candidate.slice(0, -1);
  }
  if (candidate.includes("*")) {
    const segments = candidate.split("/");
    const wildcardIndex = segments.findIndex((segment) => segment.includes("*"));
    const prefixSegments = wildcardIndex >= 0 ? segments.slice(0, wildcardIndex) : segments;
    candidate = prefixSegments.join("/");
  }
  return candidate;
}

/** 检测 token 对应的路径在磁盘上是否存在（相对于仓库根）。 */
function pathExistsOnDisk(repoRoot: string, token: string): boolean {
  const relPath = toExistenceCheckRelPath(token);
  if (relPath.length === 0) {
    // 通配符前缀退化为空（如 token 本身就是 `*.rs`）：没有可校验的前缀目录，
    // 视为仓库根自身，永远存在，不计入过时引用。
    return true;
  }
  const absPath = path.resolve(repoRoot, relPath);
  try {
    return fs.existsSync(absPath);
  } catch {
    return false;
  }
}

/** 解析索引里的 `<!-- index-ignore: a, b -->` 声明，返回被显式豁免的目录名集合。 */
function parseIndexIgnores(markdown: string): Set<string> {
  const ignored = new Set<string>();
  INDEX_IGNORE_PATTERN.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = INDEX_IGNORE_PATTERN.exec(markdown)) !== null) {
    for (const raw of match[1].split(",")) {
      const name = raw.trim().replace(/\/$/, "");
      if (name.length > 0) ignored.add(name);
    }
  }
  return ignored;
}

/**
 * 收集索引里**已有坐标**的顶层名字：只取行内反引号里的仓库路径 token，
 * 规范化成第一段路径名。
 *
 * 必须按 token 精确比对，不能用 `markdown.includes(name)` 做全文子串匹配 ——
 * 那样根目录 `test/` 会被命令表里的 `cargo test` 命中、`doc/` 会被 doctest 命中，
 * 检测直接失效。
 */
function collectIndexedTopLevelNames(markdown: string): Set<string> {
  const names = new Set<string>();
  for (const hit of extractInlineCodeTokens(markdown)) {
    if (!isRepoPathCandidate(hit.token)) continue;
    const first = hit.token.trim().replace(/^\.\//, "").split("/")[0];
    if (first.length > 0) names.add(first);
  }
  return names;
}

/**
 * 反向检测：磁盘上有顶层目录，但 AGENTS.md 全文从未提及它。
 *
 * 这条覆盖 staleRefs 抓不到的方向 —— 索引里的路径都还在，但仓库长出了索引不知道的结构，
 * 或索引里"仓库只有 X"这类枚举式陈述已经不成立。
 *
 * 只看顶层目录，三道过滤：工具/构建产物白名单、索引里的显式 `index-ignore` 豁免、
 * 以及该名字是否已作为反引号路径 token 出现在索引里。
 */
function findUnindexedTopLevelDirs(repoRoot: string, markdown: string): string[] {
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(repoRoot, { withFileTypes: true });
  } catch {
    return [];
  }
  const explicitlyIgnored = parseIndexIgnores(markdown);
  const indexedNames = collectIndexedTopLevelNames(markdown);
  return entries
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .filter((name) => !UNINDEXED_SCAN_IGNORE.has(name) && !explicitlyIgnored.has(name))
    .filter((name) => !indexedNames.has(name))
    .sort();
}

/** 提取尾部新鲜度锚（取文件中最后一次出现的匹配，容忍锚不在真正的文件尾部）。 */
function extractFreshnessAnchor(
  markdown: string,
): { commit: string; date: string } | undefined {
  ANCHOR_PATTERN.lastIndex = 0;
  let last: { commit: string; date: string } | undefined;
  let match: RegExpExecArray | null;
  while ((match = ANCHOR_PATTERN.exec(markdown)) !== null) {
    last = { commit: match[1], date: match[2] };
  }
  return last;
}

interface GitCommandResult {
  ok: boolean;
  stdout: string;
}

/** 运行一次 git 子命令（参数数组形式，绝不拼接 shell 字符串），失败时优雅降级。 */
function runGit(repoRoot: string, args: string[]): GitCommandResult {
  const result = spawnSync("git", args, {
    cwd: repoRoot,
    encoding: "utf8",
  });
  if (result.error || result.status !== 0) {
    return { ok: false, stdout: "" };
  }
  return { ok: true, stdout: (result.stdout ?? "").trim() };
}

/** `runIndexGuardCheck` 的可选行为开关。 */
export interface IndexGuardCheckOptions {
  /**
   * 跳过 git 子进程调用（锚落后多少 commit）。
   * 工具执行后的软反馈用它：锚落后的 commit 数不会因为单次工具调用而变化，
   * 而每次 write/edit/bash 都 fork 一个 git 进程是不可接受的开销。
   */
  skipGit?: boolean;
}

/**
 * 对仓库根路径运行 index-guard 检测，返回结构化报告。
 *
 * 三项检测：
 *   1. AGENTS.md 行数是否超过 MAX_LINES
 *   2. 行内反引号路径 token 是否在磁盘上存在（`(planned)` 行豁免）
 *   3. 尾部新鲜度锚是否存在、是否落后 HEAD 太多 commit / 太久未更新
 *      （非 git 仓库时优雅降级为 note，不算违规；`skipGit` 时整段跳过）
 */
export function runIndexGuardCheck(
  repoRoot: string,
  options: IndexGuardCheckOptions = {},
): IndexGuardReport {
  const notes: string[] = [];
  const agentsPath = path.join(repoRoot, AGENTS_MD_FILENAME);

  let content: string;
  try {
    content = fs.readFileSync(agentsPath, "utf8");
  } catch {
    return {
      ok: false,
      lineCount: 0,
      maxLines: MAX_LINES,
      staleRefs: [],
      plannedRefs: [],
      unindexedDirs: [],
      missingAnchor: true,
      notes: [`根 ${AGENTS_MD_FILENAME} 不存在或不可读：${agentsPath}`],
    };
  }

  const lineCount = countLines(content);

  const staleSet = new Set<string>();
  const plannedSet = new Set<string>();
  for (const hit of extractInlineCodeTokens(content)) {
    if (!isRepoPathCandidate(hit.token)) continue;
    if (pathExistsOnDisk(repoRoot, hit.token)) continue;

    if (PLANNED_MARKER.test(hit.lineText)) {
      plannedSet.add(hit.token);
    } else {
      staleSet.add(hit.token);
    }
  }
  // 同一 token 只要有任意一行没标 (planned)，就按过时处理：
  // 豁免必须逐行显式声明，否则一处标记会静默豁免其它行的同名引用。
  for (const token of staleSet) plannedSet.delete(token);
  const staleRefs = [...staleSet];
  const plannedRefs = [...plannedSet];

  const anchor = extractFreshnessAnchor(content);
  const missingAnchor = anchor === undefined;

  let anchorCommit: string | undefined;
  let headCommit: string | undefined;
  let commitsBehind: number | undefined;
  let anchorAgeDays: number | undefined;

  if (anchor) {
    anchorCommit = anchor.commit;
    const anchorMs = Date.parse(`${anchor.date}T00:00:00Z`);
    if (Number.isFinite(anchorMs)) {
      anchorAgeDays = Math.floor((Date.now() - anchorMs) / 86_400_000);
    } else {
      notes.push(`新鲜度锚日期无法解析：${anchor.date}`);
    }
  }

  if (options.skipGit) {
    notes.push("软反馈快路径：已跳过 git 校验（锚落后 commit 数不因单次工具调用变化）。");
  } else {
    const gitDirCheck = runGit(repoRoot, ["rev-parse", "--git-dir"]);
    if (!gitDirCheck.ok) {
      notes.push(
        "当前目录不是 git 仓库（`git rev-parse --git-dir` 失败），已跳过 commitsBehind 校验，仅按锚点日期判断新鲜度。",
      );
    } else if (anchor && anchor.commit.toLowerCase() === "none") {
      notes.push(
        "新鲜度锚仍是占位值 none，但当前已是 git 仓库：请把锚更新为当前 HEAD 短 sha。",
      );
    } else if (anchor) {
      const headResult = runGit(repoRoot, ["rev-parse", "HEAD"]);
      if (headResult.ok) {
        headCommit = headResult.stdout;
        const countResult = runGit(repoRoot, [
          "rev-list",
          "--count",
          `${anchor.commit}..HEAD`,
        ]);
        if (countResult.ok) {
          const parsed = Number.parseInt(countResult.stdout, 10);
          if (Number.isFinite(parsed)) {
            commitsBehind = parsed;
          }
        } else {
          notes.push(
            `无法计算 commitsBehind：锚点 commit ${anchor.commit} 可能不在当前历史中。`,
          );
        }
      } else {
        notes.push("`git rev-parse HEAD` 失败，已跳过 commitsBehind 计算。");
      }
    }
  }

  const unindexedDirs = findUnindexedTopLevelDirs(repoRoot, content);

  const hasViolation =
    lineCount > MAX_LINES ||
    staleRefs.length > 0 ||
    unindexedDirs.length > 0 ||
    missingAnchor ||
    (commitsBehind !== undefined && commitsBehind > COMMITS_BEHIND_LIMIT) ||
    (anchorAgeDays !== undefined && anchorAgeDays > ANCHOR_AGE_DAYS_LIMIT);

  return {
    ok: !hasViolation,
    lineCount,
    maxLines: MAX_LINES,
    staleRefs,
    plannedRefs,
    unindexedDirs,
    missingAnchor,
    anchorCommit,
    headCommit,
    commitsBehind,
    anchorAgeDays,
    notes,
  };
}

/** 把报告渲染成一段紧凑的中文文本，供 TUI 提示 / tool_result 追加 / CLI 打印共用。 */
export function formatCompactReport(report: IndexGuardReport): string {
  if (report.ok) {
    return "[index-guard] AGENTS.md 索引校验通过。";
  }

  const lines: string[] = ["[index-guard] AGENTS.md 索引校验未通过："];

  if (report.lineCount > report.maxLines) {
    lines.push(
      `- 行数超限：${report.lineCount} 行 > 上限 ${report.maxLines} 行。请把领域细节迁移到 .omp/rules/*.md，根索引只保留 ≤${report.maxLines} 行的地图式条目。`,
    );
  }
  if (report.staleRefs.length > 0) {
    lines.push(
      `- 过时路径引用（磁盘不存在，未标 (planned)）：${report.staleRefs.join(", ")}`,
    );
  }
  if (report.unindexedDirs.length > 0) {
    lines.push(
      `- 未入索引的顶层目录（磁盘上存在，索引从未提及）：${report.unindexedDirs.map((d) => `${d}/`).join(", ")}。请在代码地图补一行坐标，或说明为何不该出现在索引里。`,
    );
  }
  if (report.missingAnchor) {
    lines.push(
      "- 缺少新鲜度锚：文末需要 `<!-- index-verified: <commit-sha> <YYYY-MM-DD> -->`。",
    );
  }
  if (
    report.commitsBehind !== undefined &&
    report.commitsBehind > COMMITS_BEHIND_LIMIT
  ) {
    lines.push(
      `- 新鲜度锚落后 HEAD ${report.commitsBehind} 个 commit（上限 ${COMMITS_BEHIND_LIMIT}），请刷新锚点。`,
    );
  }
  if (
    report.anchorAgeDays !== undefined &&
    report.anchorAgeDays > ANCHOR_AGE_DAYS_LIMIT
  ) {
    lines.push(
      `- 新鲜度锚日期已过 ${report.anchorAgeDays} 天（上限 ${ANCHOR_AGE_DAYS_LIMIT} 天），请刷新锚点。`,
    );
  }
  if (report.plannedRefs.length > 0) {
    lines.push(`(planned 豁免，未计入违规：${report.plannedRefs.join(", ")})`);
  }
  for (const note of report.notes) {
    lines.push(`(note) ${note}`);
  }

  return lines.join("\n");
}

// ---------------------------------------------------------------------------
// extension 行为
// ---------------------------------------------------------------------------

function readStringField(
  input: Record<string, unknown> | undefined,
  field: string,
): string | undefined {
  const value = input?.[field];
  return typeof value === "string" ? value : undefined;
}

/**
 * 执行后需要重跑索引检测的工具。
 * `write`/`edit` 可能直接改 AGENTS.md；三者都可能让索引引用的路径失效
 * （`bash` 的 rm/mv 最典型）。
 */
const INDEX_AFFECTING_TOOLS = new Set(["write", "edit", "bash"]);

export default function indexGuardExtension(pi: ExtensionAPI): void {
  // `before_agent_start` 可能每回合触发一次；只在进程内注入一次提醒，
  // 避免每个 agent turn 都重复插入同一条 custom_message。
  let beforeAgentStartAttempted = false;

  // `session_stop` 最多允许 8 次连续 continue（omp://extensions.md「session_stop」）。
  // 这里只用掉一次：会话想收尾时若索引仍不同步，强制续跑一轮做对账；
  // 续跑后不再拦第二次，避免把会话锁死在对账循环里。
  let stopReconcileRequested = false;

  // ---- 硬闸门：write AGENTS.md 超行数上限时直接拦截 ----
  pi.on("tool_call", async (event) => {
    try {
      if (event.toolName !== "write") return undefined;
      const targetPath = readStringField(event.input, "path");
      if (!targetPath || path.basename(targetPath) !== AGENTS_MD_FILENAME) {
        return undefined;
      }
      const newContent = readStringField(event.input, "content") ?? "";
      const newLineCount = countLines(newContent);
      if (newLineCount <= MAX_LINES) return undefined;

      return {
        block: true,
        reason:
          `根 ${AGENTS_MD_FILENAME} 是 ≤${MAX_LINES} 行的记忆索引，本次写入 ${newLineCount} 行超限。` +
          "请把领域细节/长篇约定迁移到 .omp/rules/*.md（按需 rulebook，或带 condition 的 TTSR），" +
          "根索引只保留地图式条目 + 尾部 `<!-- index-verified: <sha> <YYYY-MM-DD> -->` 新鲜度锚。",
      };
    } catch (error) {
      pi.logger?.error?.("[index-guard] tool_call handler failed:", error);
      return undefined; // fail-open：守卫自身异常绝不能顶替成误拦截
    }
  });

  // ---- 软反馈：可能影响索引的工具执行后重跑检测，违规才追加紧凑报告 ----
  // 覆盖两类情形：直接改 AGENTS.md（write/edit），以及间接让索引里的路径失效
  // （bash 的 rm/mv、write/edit 在别处落盘导致引用目标消失）。
  // 走 skipGit 快路径，避免每次工具调用都 fork git。
  pi.on("tool_result", async (event, ctx) => {
    try {
      if (!INDEX_AFFECTING_TOOLS.has(event.toolName)) return undefined;

      const report = runIndexGuardCheck(ctx.cwd, { skipGit: true });
      if (report.ok) return undefined;

      return {
        content: [
          ...event.content,
          { type: "text", text: `\n\n${formatCompactReport(report)}` },
        ],
      };
    } catch (error) {
      pi.logger?.error?.("[index-guard] tool_result handler failed:", error);
      return undefined; // 不把成功篡改成失败，也不影响 isError
    }
  });

  // ---- 自动对账：会话收尾前若索引与仓库不同步，强制续跑一轮 ----
  // 这是整套机制里唯一不依赖人工触发的环节：检测是确定性的，
  // 改写交给 agent（哪条该删、哪个目录该有坐标是语义判断），但触发不需要有人记得跑 /sync-index。
  pi.on("session_stop", async (event, ctx) => {
    try {
      // 已经处在某个 stop hook 触发的续跑里就直接放行（omp://skills/authoring-extensions.md
      // 的 session_stop 示例）。`continue` 配额全局只有 8 次，若别的 extension 先 continue、
      // 本 handler 再叠一次，会把配额吞掉并可能造成收尾抖动 —— 进程内 flag 挡不住这条路径。
      if (event.stop_hook_active) return undefined;
      if (stopReconcileRequested) return undefined;
      const report = runIndexGuardCheck(ctx.cwd);
      if (report.ok) return undefined;
      stopReconcileRequested = true;

      return {
        continue: true,
        additionalContext: [
          "会话收尾前的索引对账（index-guard 自动触发，本次会话只触发这一次）：",
          "",
          formatCompactReport(report),
          "",
          `把根 ${AGENTS_MD_FILENAME} 改到检验通过，判据见 rule://agents-index：`,
          "- 过时路径：该行无 `(planned)` 标记且路径已不存在 → 删掉该条目，不要改写成模糊说法；",
          "- 未入索引的顶层目录：在代码地图补一行坐标（路径 — 职责 — 入口符号）；",
          "  确认它不该出现在索引里，就在维护契约节加一行 `<!-- index-ignore: <目录名> -->` 并写明理由 ——",
          "  这个决定会落盘、可 review，不要靠让检测保持红色来表达'我看过了'；",
          "- 行数超限：把细节搬进 .omp/rules/*.md 并从索引删除原文，搬迁不是复制；",
          "- 锚过期：先刷新文末 `<!-- index-verified: <sha> <YYYY-MM-DD> -->`，再重跑检查器；",
          "改完跑 `bun .omp/checks/index-guard.check.ts` 确认 ok: true，然后正常结束。",
        ].join("\n"),
      };
    } catch (error) {
      pi.logger?.error?.("[index-guard] session_stop handler failed:", error);
      return undefined; // fail-open：绝不因守卫异常阻断会话收尾
    }
  });

  // ---- 会话开场提醒：静默检测，有违规才提示 ----
  pi.on("session_start", async (_event, ctx) => {
    try {
      const report = runIndexGuardCheck(ctx.cwd);
      if (report.ok) return;
      ctx.ui.notify(formatCompactReport(report), "info");
    } catch (error) {
      pi.logger?.error?.("[index-guard] session_start handler failed:", error);
    }
  });

  // ---- 首个 agent turn 前额外注入一次结构化提醒（仅一次） ----
  pi.on("before_agent_start", async (_event, ctx) => {
    try {
      if (beforeAgentStartAttempted) return undefined;
      beforeAgentStartAttempted = true; // 无论本次是否违规，都只尝试注入一次

      const report = runIndexGuardCheck(ctx.cwd);
      if (report.ok) return undefined;

      return {
        message: {
          customType: "org.zcode.index-guard.report",
          content: formatCompactReport(report),
          display: true,
          attribution: "agent",
        },
      };
    } catch (error) {
      pi.logger?.error?.("[index-guard] before_agent_start handler failed:", error);
      return undefined;
    }
  });

  // ---- 手动触发检测的命令 ----
  pi.registerCommand("index-check", {
    description: "立即运行 AGENTS.md 索引校验（行数上限 / 过时路径 / 新鲜度锚）",
    handler: async (_args, ctx) => {
      try {
        const report = runIndexGuardCheck(ctx.cwd);
        ctx.ui.notify(formatCompactReport(report), "info");
      } catch (error) {
        pi.logger?.error?.("[index-guard] index-check command failed:", error);
        ctx.ui.notify(`[index-guard] 校验执行失败：${String(error)}`, "info");
      }
    },
  });
}
