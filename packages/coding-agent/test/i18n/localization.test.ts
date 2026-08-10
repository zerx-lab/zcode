import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { resetSettingsForTest, Settings, settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import { SETTINGS_SCHEMA, type SettingPath } from "@oh-my-pi/pi-coding-agent/config/settings-schema";
import { getSettingsForTab } from "@oh-my-pi/pi-coding-agent/modes/components/settings-defs";
import { SettingsSelectorComponent } from "@oh-my-pi/pi-coding-agent/modes/components/settings-selector";
import { WelcomeComponent } from "@oh-my-pi/pi-coding-agent/modes/components/welcome";
import { initTheme } from "@oh-my-pi/pi-coding-agent/modes/theme/theme";
import {
	BUILTIN_SLASH_COMMAND_DEFS,
	buildTuiBuiltinSlashCommands,
} from "@oh-my-pi/pi-coding-agent/slash-commands/builtin-registry";
import type { TuiSlashCommandRuntime } from "@oh-my-pi/pi-coding-agent/slash-commands/types";
import { CombinedAutocompleteProvider } from "@oh-my-pi/pi-tui/autocomplete";
import {
	getLocale,
	localizeSettingDefs,
	localizeSlashCommands,
	refreshLocale,
	t,
	tf,
	tGroup,
	tTab,
	tTip,
} from "../../src/i18n";
import { ZH_CN } from "../../src/i18n/locales/zh-CN";

const ANSI = /\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g;
const plain = (lines: readonly string[]): string => lines.map(line => line.replace(ANSI, "")).join("\n");

function makeSelector(): SettingsSelectorComponent {
	return new SettingsSelectorComponent(
		{
			availableThinkingLevels: [],
			thinkingLevel: undefined,
			availableThemes: ["titanium", "light"],
			providers: ["openai"],
			cwd: process.cwd(),
		},
		{ onChange: () => {}, onCancel: () => {} },
	);
}

/**
 * Minimal interactive-mode context satisfying every builtin command's
 * `getTuiAutocompleteDescription`. Mutate fields per assertion; the autocomplete
 * provider fuzzy-matches, so a partial stub would throw on unrelated candidates.
 */
function makeCommandContext() {
	return {
		settings: { get: (path: string) => path !== "plan.enabled" },
		planModeEnabled: false,
		goalModeEnabled: false,
		vibeModeEnabled: false,
		loopModeEnabled: false,
		loopModePaused: false,
		loopLimit: undefined as unknown,
		loopPrompt: undefined as string | undefined,
		todoPhases: [] as Array<{ tasks: Array<{ status: string }> }>,
		collabHost: undefined as { participants: unknown[] } | undefined,
		collabGuest: undefined as { readOnly?: boolean } | undefined,
		oauthManualInput: { hasPending: () => false, pendingProviderId: undefined as string | undefined },
		session: {
			model: undefined as { provider: string; id: string } | undefined,
			isStreaming: false,
			getActiveToolNames: () => [] as string[],
			getAllToolNames: () => [] as string[],
			getContextUsage: () => undefined as { percent: number; tokens: number; contextWindow: number } | undefined,
			getAsyncJobSnapshot: () => undefined as { running: unknown[]; recent: unknown[] } | undefined,
			getAdvisorStats: () => ({ active: false, configured: false, advisors: [] as unknown[], model: undefined }),
			getGoalModeState: () => undefined,
			isFastModeEnabled: () => false,
			inspectImageState: () => ({ mode: "auto" }),
			settings: { get: () => false },
		},
	};
}

/** Drive the cursor with `key` until `probe` matches the rendered frame. */
function moveUntil(selector: SettingsSelectorComponent, key: string, probe: RegExp, limit: number): boolean {
	for (let i = 0; i < limit; i++) {
		if (probe.test(plain(selector.render(96)))) return true;
		selector.handleInput(key);
	}
	return probe.test(plain(selector.render(96)));
}

/**
 * The resolver reads LC_ALL > LC_MESSAGES > LANG > LANGUAGE, so controlling only
 * LANG lets a CI-provided LC_ALL decide the result. Clear all four, apply `tag`
 * to the variable under test, and restore every one afterwards.
 */
const LOCALE_ENV_VARS = ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] as const;

function withLocaleEnv(vars: Partial<Record<(typeof LOCALE_ENV_VARS)[number], string>>, body: () => void): void {
	const saved = new Map(LOCALE_ENV_VARS.map(name => [name, process.env[name]]));
	try {
		for (const name of LOCALE_ENV_VARS) delete process.env[name];
		for (const [name, value] of Object.entries(vars)) process.env[name] = value;
		body();
	} finally {
		for (const [name, value] of saved) {
			if (value === undefined) delete process.env[name];
			else process.env[name] = value;
		}
	}
}

describe("i18n", () => {
	beforeEach(async () => {
		resetSettingsForTest();
		await Settings.init({ inMemory: true });
		settings.set("ui.language", "en");
		refreshLocale();
		await initTheme();
	});

	afterEach(() => {
		// Restore English before tearing settings down so the module-level locale
		// cache never leaks a Chinese UI into later test files on a zh machine.
		try {
			settings.set("ui.language", "en");
		} catch {
			// A test may have reset Settings already; the refresh below still lands
			// on "en" because the resolver has no instance to read.
		}
		refreshLocale();
		resetSettingsForTest();
	});

	it("resolves the locale from the setting, not the environment", () => {
		expect(getLocale()).toBe("en");
		settings.set("ui.language", "zh-CN");
		expect(refreshLocale()).toBe("zh-CN");
		settings.set("ui.language", "en");
		expect(refreshLocale()).toBe("en");
	});

	it("follows the host locale only when the setting is auto", () => {
		withLocaleEnv({ LANG: "zh_CN.UTF-8" }, () => {
			settings.set("ui.language", "auto");
			expect(refreshLocale()).toBe("zh-CN");

			// An explicit choice wins over the environment.
			settings.set("ui.language", "en");
			expect(refreshLocale()).toBe("en");
		});
		withLocaleEnv({ LANG: "en_US.UTF-8" }, () => {
			settings.set("ui.language", "auto");
			expect(refreshLocale()).toBe("en");
		});
	});

	it("honours the POSIX locale variable precedence", () => {
		settings.set("ui.language", "auto");
		// LC_ALL outranks everything below it.
		withLocaleEnv({ LC_ALL: "en_US.UTF-8", LC_MESSAGES: "zh_CN.UTF-8", LANG: "zh_CN.UTF-8" }, () => {
			expect(refreshLocale()).toBe("en");
		});
		withLocaleEnv({ LC_MESSAGES: "zh_CN.UTF-8", LANG: "en_US.UTF-8" }, () => {
			expect(refreshLocale()).toBe("zh-CN");
		});
		// LANGUAGE is the last resort, and a single tag still resolves.
		withLocaleEnv({ LANGUAGE: "zh_CN" }, () => {
			expect(refreshLocale()).toBe("zh-CN");
		});
		// Any of the single-valued variables outranks the LANGUAGE list.
		withLocaleEnv({ LANG: "en_US.UTF-8", LANGUAGE: "zh_CN" }, () => {
			expect(refreshLocale()).toBe("en");
		});
	});

	it("walks LANGUAGE as a colon-separated preference list", () => {
		settings.set("ui.language", "auto");
		// First entry has no bundle: fall through instead of giving up on the list.
		withLocaleEnv({ LANGUAGE: "de_DE:zh_CN" }, () => {
			expect(refreshLocale()).toBe("zh-CN");
		});
		// Traditional Chinese has no bundle either, so the Simplified fallback wins.
		withLocaleEnv({ LANGUAGE: "zh_Hant_TW:zh_CN" }, () => {
			expect(refreshLocale()).toBe("zh-CN");
		});
		// An explicit English preference ahead of Chinese must be honoured.
		withLocaleEnv({ LANGUAGE: "en_US:zh_CN" }, () => {
			expect(refreshLocale()).toBe("en");
		});
		// Nothing in the list is supported.
		withLocaleEnv({ LANGUAGE: "de_DE:fr_FR" }, () => {
			expect(refreshLocale()).toBe("en");
		});
		// Empty segments must not short-circuit the walk.
		withLocaleEnv({ LANGUAGE: ":::zh_CN" }, () => {
			expect(refreshLocale()).toBe("zh-CN");
		});
	});

	it("accepts both POSIX and BCP-47 spellings, and does not fake Traditional Chinese", () => {
		settings.set("ui.language", "auto");
		for (const tag of ["zh", "zh_CN.UTF-8", "zh-Hans-CN", "zh_Hans_CN", "zh_SG", "ZH_CN"]) {
			withLocaleEnv({ LANG: tag }, () => expect(refreshLocale()).toBe("zh-CN"));
		}
		// No Traditional bundle: English beats serving Simplified to a zh-Hant user.
		// Both separators must behave the same — `\b` would let `zh_Hant_TW` slip
		// through to Simplified because `_` is a word character.
		for (const tag of [
			"zh_TW.UTF-8",
			"zh-Hant",
			"zh_Hant",
			"zh-Hant-TW",
			"zh_Hant_TW",
			"zh-Hant-HK",
			"zh_HK.UTF-8",
			"zh_MO",
			"ZH_HANT_TW",
		]) {
			withLocaleEnv({ LANG: tag }, () => expect(refreshLocale()).toBe("en"));
		}
		for (const tag of ["en_US.UTF-8", "zhosa", "de_DE.UTF-8"]) {
			withLocaleEnv({ LANG: tag }, () => expect(refreshLocale()).toBe("en"));
		}
	});

	it("stays on English when Settings is not initialized, whatever the host locale is", () => {
		withLocaleEnv({ LC_ALL: "zh_CN.UTF-8", LANG: "zh_CN.UTF-8" }, () => {
			resetSettingsForTest();
			// No Settings instance: `settings.get` throws and the resolver must not
			// fall through to host detection, or UI-string tests become order- and
			// machine-dependent.
			expect(refreshLocale()).toBe("en");
		});
	});

	it("is the identity transform under English", () => {
		expect(t("Settings")).toBe("Settings");
		expect(tTab("appearance")).toBe("Appearance");
		expect(tGroup("Status Line")).toBe("Status Line");
		expect(tTip("Press shift+tab to cycle through reasoning effort levels")).toBe(
			"Press shift+tab to cycle through reasoning effort levels",
		);
	});

	it("falls back to the English source for keys the bundle does not carry", () => {
		settings.set("ui.language", "zh-CN");
		refreshLocale();
		expect(t("An upstream string added after this fork translated the panel")).toBe(
			"An upstream string added after this fork translated the panel",
		);
		expect(tTip("A brand new upstream tip")).toBe("A brand new upstream tip");
		expect(tGroup("Freshly Added Group")).toBe("Freshly Added Group");
	});

	it("keeps the [NEW] marker on translated tips", () => {
		settings.set("ui.language", "zh-CN");
		refreshLocale();
		const source = "Press shift+tab to cycle through reasoning effort levels";
		expect(tTip(source)).toBe("按 shift+tab 在推理强度档位之间轮换");
		expect(tTip(`${source} [NEW]`)).toBe("按 shift+tab 在推理强度档位之间轮换 [NEW]");
	});

	it("substitutes positional arguments in both languages", () => {
		expect(tf("Limit: {0}", "4")).toBe("Limit: 4");
		settings.set("ui.language", "zh-CN");
		refreshLocale();
		expect(tf("Limit: {0}", "4")).toBe("上限：4");
	});

	it("localizes setting defs field-by-field and leaves group untouched", () => {
		settings.set("ui.language", "zh-CN");
		refreshLocale();
		const [localized] = localizeSettingDefs([
			{
				path: "symbolPreset",
				label: "Symbol Preset",
				description: "Glyph set for icons and symbols (Unicode, Nerd Font, or ASCII)",
				group: "Theme",
				options: [{ value: "ascii", label: "ASCII", description: "Maximum compatibility" }],
			},
		]);
		expect(localized?.label).toBe("符号预设");
		// group stays English so TAB_GROUPS ordering keeps resolving.
		expect(localized?.group).toBe("Theme");
		expect(localized?.options?.[0]?.label).toBe("ASCII");
		expect(localized?.options?.[0]?.description).toBe("最大兼容性");
	});

	it("switches the whole settings panel live, inside one component instance", () => {
		const selector = makeSelector();
		const before = plain(selector.render(96));
		expect(before).toContain("Settings");
		expect(before).toContain("Appearance");
		expect(before).toContain("Status Line");
		expect(before).toContain("Dark Theme");

		// Real user path: type to search, land on Language, open the submenu, pick 简体中文.
		for (const ch of "language") selector.handleInput(ch);
		expect(moveUntil(selector, "\x1b[A", /❯\s+Language\b/, 12)).toBe(true);
		selector.handleInput("\r");
		expect(moveUntil(selector, "\x1b[B", /❯\s+简体中文/, 6)).toBe(true);
		selector.handleInput("\r");

		expect(settings.get("ui.language")).toBe("zh-CN");
		expect(getLocale()).toBe("zh-CN");

		const after = plain(selector.render(96));
		expect(after).toContain("设置");
		expect(after).toContain("外观");
		expect(after).toContain("状态栏");
		expect(after).toContain("深色主题");
		expect(after).toContain("界面语言");
		// No half-translated frame left behind.
		expect(after).not.toContain("Settings");
		expect(after).not.toContain("Status Line");
	});

	it("re-renders the welcome box in the new language without an explicit invalidate", () => {
		const welcome = new WelcomeComponent("0.0.0-test", "claude-opus-5", "anthropic", [
			{ name: "demo", timeAgo: "2h ago" },
		]);
		expect(plain(welcome.render(96))).toContain("Welcome back!");

		settings.set("ui.language", "zh-CN");
		refreshLocale();
		// No invalidate(): nothing in the settings panel touches this component, so
		// the render cache must key on the locale by itself.
		const after = plain(welcome.render(96));
		expect(after).toContain("欢迎回来！");
		expect(after).toContain("最近会话");
		expect(after).toContain("LSP 服务");
		// The tip is picked at random; the body must still be Chinese, which only
		// holds because the pick caches the English source and translates on read.
		const tipBody = /^\s*提示:\s*(.+)$/m.exec(after)?.[1] ?? "";
		expect(tipBody).toMatch(/[\u4e00-\u9fff]/);
	});

	it("renders translated group headings while keeping TAB_GROUPS ordering", () => {
		settings.set("ui.language", "zh-CN");
		refreshLocale();
		const groups = [...new Set(getSettingsForTab("appearance").map(def => def.group))];
		// Ordering comes from TAB_GROUPS, so the defs must still carry English groups.
		expect(groups).toEqual(["Theme", "Status Line", "Display", "Images"]);
		expect(groups.map(group => tGroup(group as string))).toEqual(["主题", "状态栏", "显示", "图像"]);
	});

	it("covers every UI setting in the zh-CN bundle with no dead keys", () => {
		const uiPaths = new Set<string>();
		for (const path in SETTINGS_SCHEMA) {
			if ("ui" in SETTINGS_SCHEMA[path as SettingPath]) uiPaths.add(path);
		}
		const dictPaths = Object.keys(ZH_CN.settings);

		// Dead keys are always a fork bug: the path no longer exists upstream.
		expect(dictPaths.filter(path => !uiPaths.has(path))).toEqual([]);
		// Missing entries silently fall back to English — this is the sync signal
		// to translate whatever upstream just added.
		expect([...uiPaths].filter(path => !ZH_CN.settings[path])).toEqual([]);
	});

	it("covers every enum option the settings panel can show", () => {
		const missing: string[] = [];
		for (const path in SETTINGS_SCHEMA) {
			const ui = (SETTINGS_SCHEMA[path as SettingPath] as { ui?: { options?: unknown } }).ui;
			const options = ui?.options;
			if (!Array.isArray(options)) continue;
			for (const option of options as ReadonlyArray<{ value: string }>) {
				if (!ZH_CN.settings[path]?.options?.[option.value]?.label) missing.push(`${path}::${option.value}`);
			}
		}
		expect(missing).toEqual([]);
	});

	it("translates every tip shipped in tips.txt", async () => {
		const tipsFile = new URL("../../src/modes/components/tips.txt", import.meta.url);
		const sources = (await Bun.file(tipsFile).text())
			.split("\n")
			.map(line => line.trim().replace(/\s*\[NEW\]\s*$/, ""))
			.filter(line => line.length > 0);
		expect(sources.length).toBeGreaterThan(0);
		expect(sources.filter(source => !ZH_CN.tips[source])).toEqual([]);
	});

	it("only rebuilds the command table in the new language after the locale cache is refreshed", () => {
		const runtime = { ctx: {} } as unknown as TuiSlashCommandRuntime;
		const quit = (): string | undefined =>
			buildTuiBuiltinSlashCommands(runtime).find(command => command.name === "quit")?.description;
		expect(quit()).toBe("Quit the application");

		// The settings panel persists the value and fires `callbacks.onChange`
		// *before* its own #relocalize(), so a rebuild at that instant still sees
		// the previous locale — this is why selector-controller.handleSettingChange
		// calls refreshLocale() itself before refreshSlashCommandState().
		settings.set("ui.language", "zh-CN");
		expect(quit()).toBe("Quit the application");

		refreshLocale();
		expect(quit()).toBe("退出应用");
	});

	it("localizes builtin slash command descriptions without touching invocable tokens", () => {
		const runtime = { ctx: {} } as unknown as TuiSlashCommandRuntime;

		const english = buildTuiBuiltinSlashCommands(runtime);
		const quitEn = english.find(command => command.name === "quit");
		expect(quitEn?.description).toBe("Quit the application");

		settings.set("ui.language", "zh-CN");
		refreshLocale();

		const chinese = buildTuiBuiltinSlashCommands(runtime);
		expect(chinese.find(command => command.name === "quit")?.description).toBe("退出应用");
		// Names and aliases are what the user types — they must never be translated.
		expect(chinese.map(command => command.name)).toEqual(english.map(command => command.name));
		expect(chinese.map(command => command.aliases ?? [])).toEqual(english.map(command => command.aliases ?? []));
		// Subcommand names stay, their descriptions translate.
		const security = chinese.find(command => command.name === "security");
		const securityEn = english.find(command => command.name === "security");
		expect(securityEn?.subcommands?.length).toBeGreaterThan(0);
		expect(security?.subcommands?.map(sub => sub.name)).toEqual(securityEn?.subcommands?.map(sub => sub.name));
		expect(security?.subcommands?.every(sub => /[\u4e00-\u9fff]/.test(sub.description ?? ""))).toBe(true);
	});

	it("localizes the dynamic autocomplete description the provider actually renders", async () => {
		// The provider prefers getAutocompleteDescription() over the static
		// `description`, so translating only the static field leaves `/model`
		// English. Drive the real provider to prove the live path is covered.
		// The provider fuzzy-matches, so every candidate's dynamic description runs;
		// the context has to satisfy all of them, not just /model.
		const ctx = makeCommandContext();
		const runtime = { ctx } as unknown as TuiSlashCommandRuntime;
		const describe = async (): Promise<string> => {
			const provider = new CombinedAutocompleteProvider([...buildTuiBuiltinSlashCommands(runtime)], process.cwd());
			const result = await provider.getSuggestions(["/model"], 0, 6);
			const item = result?.items.find(entry => entry.value === "model");
			if (!item) throw new Error("expected /model in the completion list");
			return item.description ?? "";
		};

		expect(await describe()).toBe("Model: none selected");
		ctx.session.model = { provider: "anthropic", id: "opus-5" };
		expect(await describe()).toBe("Model: anthropic/opus-5");

		settings.set("ui.language", "zh-CN");
		refreshLocale();

		// With a model: the label translates, the model id stays verbatim.
		expect(await describe()).toBe("模型：anthropic/opus-5");
		ctx.session.model = undefined;
		expect(await describe()).toBe("模型：未选择");
	});

	it("localizes every dynamic autocomplete branch and keeps runtime values verbatim", () => {
		// `getTuiAutocompleteDescription` bypasses the static `description`, so each
		// branch needs its own dictionary entry. Drive the real functions with a
		// controlled context instead of scanning the source for `t()` calls.
		const ctx = makeCommandContext();
		const runtime = { ctx } as unknown as TuiSlashCommandRuntime;
		const describe = (name: string): string => {
			const command = buildTuiBuiltinSlashCommands(runtime).find(entry => entry.name === name);
			if (!command?.getAutocompleteDescription) throw new Error(`no dynamic description for /${name}`);
			return command.getAutocompleteDescription() ?? "";
		};

		settings.set("ui.language", "zh-CN");
		refreshLocale();

		// Model: label translates, the provider/id token stays verbatim.
		expect(describe("model")).toBe("模型：未选择");
		ctx.session.model = { provider: "anthropic", id: "opus-5" };
		expect(describe("model")).toBe("模型：anthropic/opus-5");
		expect(describe("switch")).toBe("模型：anthropic/opus-5");

		// Loop: off / paused / iterations / duration / prompt / waiting.
		expect(describe("loop")).toBe("循环模式：关");
		ctx.loopModeEnabled = true;
		expect(describe("loop")).toBe("循环模式：开（等待下一条提示词）");
		ctx.loopPrompt = "keep going";
		expect(describe("loop")).toBe("循环模式：开（重复同一提示词）");
		ctx.loopLimit = { kind: "iterations", initial: 10, remaining: 3 };
		expect(describe("loop")).toBe("循环模式：开（剩余 3/10 次）");
		ctx.loopLimit = { kind: "duration", durationMs: 600_000, deadlineMs: Date.now() + 600_000 };
		expect(describe("loop")).toBe("循环模式：开（限制 10 分钟）");
		ctx.loopModePaused = true;
		expect(describe("loop")).toBe("循环模式：已暂停");

		// Counts: the numbers survive, the surrounding words translate.
		expect(describe("tools")).toBe("工具：无可用工具");
		expect(describe("force")).toBe("强制工具：当前无可用工具");
		ctx.session.getActiveToolNames = () => ["read", "bash"];
		ctx.session.getAllToolNames = () => ["read", "bash", "edit"];
		expect(describe("tools")).toBe("工具：2 个启用 / 共 3 个");
		expect(describe("force")).toBe("强制工具：2 个可用工具");

		expect(describe("todo")).toBe("待办：无");
		ctx.todoPhases = [{ tasks: [{ status: "pending" }, { status: "in_progress" }, { status: "completed" }] }];
		expect(describe("todo")).toBe("待办：2 项未完成（1 项进行中，1 项已完成）");

		expect(describe("context")).toBe("上下文：不可用");
		ctx.session.getContextUsage = () => ({ percent: 42.4, tokens: 84_000, contextWindow: 200_000 });
		expect(describe("context")).toMatch(/^上下文：42%（.+\/.+）$/);
		expect(describe("compact")).toBe("压缩：上下文已用 42%");

		// Mode gates.
		expect(describe("vibe")).toBe("Vibe 模式：关");
		ctx.vibeModeEnabled = true;
		expect(describe("vibe")).toBe("Vibe 模式：开");
		expect(describe("plan")).toBe("计划模式：已在设置中禁用");
		expect(describe("plan-review")).toBe("计划复审：计划模式未启用");

		// Every branch above must be a dictionary hit, never a passthrough.
		for (const name of ["model", "loop", "tools", "force", "todo", "context", "compact", "vibe", "plan"]) {
			expect(describe(name)).toMatch(/[\u4e00-\u9fff]/);
		}
	});

	it("leaves unknown commands on their English description", () => {
		settings.set("ui.language", "zh-CN");
		refreshLocale();
		const [localized] = localizeSlashCommands([
			{ name: "a-command-upstream-just-added", description: "Do the new thing" },
		]);
		expect(localized?.description).toBe("Do the new thing");
	});

	it("covers every builtin slash command and subcommand with no dead keys", () => {
		const names = new Set(BUILTIN_SLASH_COMMAND_DEFS.map(command => command.name));
		expect(Object.keys(ZH_CN.commands).filter(name => !names.has(name))).toEqual([]);

		const missing: string[] = [];
		for (const command of BUILTIN_SLASH_COMMAND_DEFS) {
			const entry = ZH_CN.commands[command.name];
			if (command.description && !entry?.description) missing.push(command.name);
			for (const sub of command.subcommands ?? []) {
				if (sub.description && !entry?.subcommands?.[sub.name]) missing.push(`${command.name}::${sub.name}`);
			}
		}
		expect(missing).toEqual([]);
	});
});
