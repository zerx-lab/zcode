import { TAB_METADATA } from "../config/settings-schema";
import { activeBundle } from "./locale";

export * from "./constants";
export * from "./locale";
export * from "./types";

/**
 * 界面固定文案取词。键是英文原文：上游改了原文即回退英文显示，
 * 永远不会显示与新原文对不上的旧译文。
 */
export function t(text: string): string {
	return activeBundle()?.chrome[text] ?? text;
}

/**
 * 带位置参数的取词。模板用 `{0}` / `{1}` 占位，译文可自由调整参数顺序。
 * 未命中字典时对英文原文做同样的插值。
 */
export function tf(template: string, ...args: readonly string[]): string {
	return t(template).replace(/\{(\d+)\}/g, (whole, index: string) => args[Number(index)] ?? whole);
}

/** 设置面板 tab 标题；未翻译回退 `TAB_METADATA` 的英文 label。 */
export function tTab(tab: keyof typeof TAB_METADATA): string {
	return activeBundle()?.tabs[tab] ?? TAB_METADATA[tab].label;
}

/** 设置面板分组标题；未翻译回退英文组名。 */
export function tGroup(group: string): string {
	return activeBundle()?.groups[group] ?? group;
}

/** 尾随的 "what's new" 标记，翻译时必须原样保留（欢迎页据此高亮）。 */
const NEW_TIP_MARKER = /\s*\[NEW\]\s*$/;

/**
 * 欢迎页 tip 取词。`[NEW]` 标记先剥离再查表、命中后原样拼回，
 * 这样上游给某条 tip 加/去标记不会让译文失配。
 */
export function tTip(tip: string): string {
	const marker = NEW_TIP_MARKER.exec(tip);
	const body = marker ? tip.slice(0, marker.index) : tip;
	const translated = activeBundle()?.tips[body];
	if (translated === undefined) return tip;
	return marker ? `${translated} [NEW]` : translated;
}

/** 设置项译文可覆盖的字段子集。泛型化以避免依赖 UI 层的 `SettingDef` 类型。 */
interface LocalizableSettingDef {
	readonly path: string;
	readonly label: string;
	readonly description: string;
	readonly options?: ReadonlyArray<{ value: string; label: string; description?: string }>;
}

/**
 * 把一批设置项定义整体本地化：label / description / 枚举选项。
 *
 * 这是设置面板全部文案的**唯一注入点** —— 上游 `settings-defs.ts` 只需在
 * schema→UI 转换出口调用一次。缺失译文逐字段回退英文，允许字典增量补全。
 *
 * 刻意**不翻译 `group`**：`getSettingsForTab` 用 `TAB_GROUPS` 的英文组名给分组
 * 排序，翻译会让 `indexOf` 全部落空、分组顺序退化。组名在渲染点经 {@link tGroup} 取词。
 */
export function localizeSettingDefs<T extends LocalizableSettingDef>(defs: readonly T[]): T[] {
	const bundle = activeBundle();
	if (!bundle) return [...defs];
	return defs.map(def => {
		const entry = bundle.settings[def.path];
		if (!entry) return def;
		const options = def.options?.map(option => {
			const text = entry.options?.[option.value];
			if (!text) return option;
			return { ...option, label: text.label ?? option.label, description: text.description ?? option.description };
		});
		return {
			...def,
			label: entry.label ?? def.label,
			description: entry.description ?? def.description,
			...(options === undefined ? {} : { options }),
		} as T;
	});
}

/** 内置 slash 命令里可被译文覆盖的字段子集。泛型化以免依赖 slash-commands 层的类型。 */
interface LocalizableSlashCommand {
	readonly name: string;
	readonly description?: string;
	readonly subcommands?: ReadonlyArray<{ name: string; description?: string }>;
}

/**
 * 把一批内置 slash 命令本地化：命令描述与子命令描述。
 *
 * 命令名、别名、参数提示一律不动 —— 它们是用户敲进输入框的 token。
 * 上游 `builtin-registry.ts` 在构建 TUI 命令表的出口调用一次即可。
 */
export function localizeSlashCommands<T extends LocalizableSlashCommand>(commands: readonly T[]): T[] {
	const bundle = activeBundle();
	if (!bundle) return [...commands];
	return commands.map(command => {
		const entry = bundle.commands[command.name];
		if (!entry) return command;
		const subcommands = command.subcommands?.map(sub => {
			const description = entry.subcommands?.[sub.name];
			return description === undefined ? sub : { ...sub, description };
		});
		return {
			...command,
			...(entry.description === undefined ? {} : { description: entry.description }),
			...(subcommands === undefined ? {} : { subcommands }),
		} as T;
	});
}
