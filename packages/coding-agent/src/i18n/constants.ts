import type { LanguageSetting } from "./types";

/**
 * 零依赖常量模块。`settings-schema.ts` 只 import 这里 —— 若它引用 `locale.ts`
 * 会形成 settings → settings-schema → locale → settings 的初始化环（TDZ 风险）。
 */

/** 语言设置项在 `SETTINGS_SCHEMA` 中的 path。 */
export const LANGUAGE_SETTING_PATH = "ui.language";

/** 设置项候选值，供 schema 的 `values` 与 `ui.options` 复用。 */
export const LANGUAGE_SETTING_VALUES: readonly LanguageSetting[] = ["auto", "en", "zh-CN"];

/** 设置面板里的语言选项元数据（英文；中文由 zh-CN 语言包覆盖）。 */
export const LANGUAGE_SETTING_OPTIONS: ReadonlyArray<{ value: LanguageSetting; label: string; description: string }> = [
	{ value: "auto", label: "System", description: "Follow the system locale (LANG / LC_ALL, else ICU)" },
	{ value: "en", label: "English", description: "English" },
	{ value: "zh-CN", label: "简体中文", description: "Simplified Chinese" },
];
