/**
 * Fork-owned tests (upstream has no such file → zero rebase conflict surface).
 *
 * Two contracts, both about "does the repo's own always-apply rule actually reach
 * the model":
 *   1. project `rules/` resolve by ancestor walk bounded at repoRoot, so a session
 *      started in a subdirectory still sees them and never picks up rules from
 *      above the repo;
 *   2. user-scope `.omp` compat covers declarative context files only — commands,
 *      extensions and settings stay native-only so the two brands never share
 *      executable or stateful config.
 */
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { loadCapability } from "@oh-my-pi/pi-coding-agent/capability";
import { clearCache } from "@oh-my-pi/pi-coding-agent/capability/fs";
import { type Rule, ruleCapability } from "@oh-my-pi/pi-coding-agent/capability/rule";
import "@oh-my-pi/pi-coding-agent/discovery";

async function writeRule(file: string, name: string): Promise<void> {
	await Bun.write(file, `---\ndescription: ${name}\nalwaysApply: true\n---\n\nbody of ${name}\n`);
}

describe("project rule discovery from a subdirectory", () => {
	let root: string;

	beforeEach(async () => {
		root = await fs.mkdtemp(path.join(os.tmpdir(), "omp-rule-walk-"));
		// <root>/above/.omp/rules/above-repo.md   ← outside the repo, must NOT load
		// <root>/above/repo/.git                  ← repoRoot boundary
		// <root>/above/repo/.omp/rules/repo-wide.md
		// <root>/above/repo/packages/app          ← session cwd
		//
		// <root>/loose/.omp/rules/loose-parent.md ← no .git anywhere, must NOT load
		// <root>/loose/work/.omp/rules/loose-own.md
		await fs.mkdir(path.join(root, "above", "repo", ".git"), { recursive: true });
		await fs.mkdir(path.join(root, "above", "repo", "packages", "app"), { recursive: true });
		await writeRule(path.join(root, "above", ".omp", "rules", "above-repo.md"), "above-repo");
		await writeRule(path.join(root, "above", "repo", ".omp", "rules", "repo-wide.md"), "repo-wide");

		await writeRule(path.join(root, "loose", ".omp", "rules", "loose-parent.md"), "loose-parent");
		await writeRule(path.join(root, "loose", "work", ".omp", "rules", "loose-own.md"), "loose-own");
		clearCache();
	});

	afterEach(async () => {
		clearCache();
		await fs.rm(root, { recursive: true, force: true });
	});

	test("repo rules load from a nested cwd and the walk stops at repoRoot", async () => {
		const result = await loadCapability<Rule>(ruleCapability.id, {
			cwd: path.join(root, "above", "repo", "packages", "app"),
		});
		const projectRules = result.items.filter(rule => rule._source?.level === "project");

		const repoWide = projectRules.find(rule => rule.name === "repo-wide");
		expect(repoWide).toBeDefined();
		// alwaysApply is what makes it reach the system prompt at all; a rule that
		// loads but loses the flag is silently inert.
		expect(repoWide?.alwaysApply).toBe(true);

		// Everything above repoRoot belongs to an unrelated tree.
		expect(projectRules.map(rule => rule.name)).not.toContain("above-repo");
	});

	test("a relative cwd resolves the same repo boundary", async () => {
		// findRepoRoot() always returns an absolute path while `cwd` may arrive
		// relative; comparing the two raw forms degrades to "cwd only" and loses the
		// repo's rules again — the exact bug the ancestor walk was added to fix.
		const previous = process.cwd();
		process.chdir(path.join(root, "above", "repo"));
		try {
			clearCache();
			const result = await loadCapability<Rule>(ruleCapability.id, { cwd: path.join("packages", "app") });
			expect(result.items.map(rule => rule.name)).toContain("repo-wide");
		} finally {
			process.chdir(previous);
		}
	});

	test("with no repo boundary only cwd speaks for the project", async () => {
		// mkdtemp gives a tree with no `.git` between cwd and the fs root, so
		// findRepoRoot returns null. Inheriting a parent's rules there would silently
		// constrain every turn with an unrelated directory's policy — unlike a
		// missing skill, that is not a recoverable loss.
		const result = await loadCapability<Rule>(ruleCapability.id, {
			cwd: path.join(root, "loose", "work"),
		});
		const projectRules = result.items.filter(rule => rule._source?.level === "project").map(rule => rule.name);

		expect(projectRules).toContain("loose-own");
		expect(projectRules).not.toContain("loose-parent");
	});
});

/**
 * User-scope compat needs a distinct `$HOME`. `dirs.ts` anchors home at module
 * load (`RESOLVER_HOME`, explicitly "stable across test mocks of os.homedir()"),
 * so an in-process spy would silently degrade every candidate to native-only and
 * the test would pass while proving nothing. A child process with its own HOME is
 * the only honest seam.
 */
describe("user-scope .omp compat", () => {
	const PROBE = `
const path = require("node:path");
await import("@oh-my-pi/pi-coding-agent/discovery");
const { loadCapability } = await import("@oh-my-pi/pi-coding-agent/capability");
const { ruleCapability } = await import("@oh-my-pi/pi-coding-agent/capability/rule");
const { slashCommandCapability } = await import("@oh-my-pi/pi-coding-agent/capability/slash-command");
const { extensionModuleCapability } = await import("@oh-my-pi/pi-coding-agent/capability/extension-module");
const { settingsCapability } = await import("@oh-my-pi/pi-coding-agent/capability/settings");
const { contextFileCapability } = await import("@oh-my-pi/pi-coding-agent/capability/context-file");
const cwd = path.join(process.env.PROBE_HOME, "project");
const origin = item => (item._source.path.includes(".zcode") ? "native" : "compat");
const rules = await loadCapability(ruleCapability.id, { cwd });
const contextFiles = await loadCapability(contextFileCapability.id, { cwd });
const out = {
  userRules: rules.items
    .filter(r => r._source.level === "user" && ["native-only", "compat-only", "dup"].includes(r.name))
    .map(r => r.name + ":" + origin(r))
    .sort(),
  userContext: contextFiles.items.filter(f => f.level === "user").map(origin),
  leaks: {},
};
for (const [label, capability] of [
  ["commands", slashCommandCapability],
  ["extensions", extensionModuleCapability],
  ["settings", settingsCapability],
]) {
  const loaded = await loadCapability(capability.id, { cwd });
  out.leaks[label] = JSON.stringify(loaded.items).replaceAll("\\\\\\\\", "/").includes("/.omp/");
}
console.log(JSON.stringify(out));
`;

	let home: string;

	beforeEach(async () => {
		home = await fs.mkdtemp(path.join(os.tmpdir(), "omp-user-compat-"));
		await fs.mkdir(path.join(home, "project"), { recursive: true });
		await writeRule(path.join(home, ".zcode", "agent", "rules", "native-only.md"), "native-only");
		await writeRule(path.join(home, ".omp", "agent", "rules", "compat-only.md"), "compat-only");
		await writeRule(path.join(home, ".zcode", "agent", "rules", "dup.md"), "native-dup");
		await writeRule(path.join(home, ".omp", "agent", "rules", "dup.md"), "compat-dup");
		// Declarative context that only exists in the legacy dir.
		await Bun.write(path.join(home, ".omp", "agent", "AGENTS.md"), "# legacy user context\n");
		// Executable / stateful config that must stay behind.
		await Bun.write(path.join(home, ".omp", "agent", "commands", "legacy-cmd.md"), "---\ndescription: x\n---\nrun\n");
		await Bun.write(path.join(home, ".omp", "agent", "extensions", "legacy-ext", "index.ts"), "export default {};\n");
		await Bun.write(path.join(home, ".omp", "agent", "settings.json"), '{"theme":"legacy"}\n');
	});

	afterEach(async () => {
		await fs.rm(home, { recursive: true, force: true });
	});

	test("merges declarative context, keeps executable and stateful config native-only", async () => {
		const child = Bun.spawn(["bun", "-e", PROBE], {
			cwd: path.join(import.meta.dir, "..", "..", "..", ".."),
			env: { ...process.env, HOME: home, USERPROFILE: home, PROBE_HOME: home },
			stdout: "pipe",
			stderr: "pipe",
		});
		const [stdout, stderr, exitCode] = await Promise.all([
			new Response(child.stdout).text(),
			new Response(child.stderr).text(),
			child.exited,
		]);
		expect(exitCode, `probe failed: ${stderr}`).toBe(0);
		const probe = JSON.parse(stdout.trim().split("\n").at(-1) ?? "{}");

		// Both dirs contribute; a same-named rule resolves to native.
		expect(probe.userRules).toEqual(["compat-only:compat", "dup:native", "native-only:native"]);
		// AGENTS.md exists only in the legacy dir, so it must still be picked up.
		expect(probe.userContext).toEqual(["compat"]);
		// The whole point of scoping compat to declarative files: no foreign
		// extension module gets loaded, no foreign settings blended in.
		expect(probe.leaks).toEqual({ commands: false, extensions: false, settings: false });
	});
});
