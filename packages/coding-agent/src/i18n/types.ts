/**
 * Fork i18n 层的字典类型（见 docs/fork/i18n.md）。
 *
 * 上游不存在 `src/i18n/` → rebase 永不冲突。所有翻译内容只落在本目录，
 * 上游文件里只保留「取词」这一行接线，不搬运任何文案。
 */

/** 受支持的界面语言。`en` 是恒等语言（无字典，直接回退原文）。 */
export type LocaleId = "en" | "zh-CN";

/** 语言设置项取值：`auto` 跟随系统 locale。 */
export type LanguageSetting = "auto" | LocaleId;

/** 单个设置项的译文。缺省字段回退英文原文，便于增量翻译。 */
export interface SettingTextEntry {
	label?: string;
	description?: string;
	/** 按 option value 索引的枚举选项译文。 */
	options?: Record<string, { label?: string; description?: string }>;
}

/** 按 `SettingPath` 索引。用 path 而非英文原文作键：上游改文案不会静默丢译文。 */
export type SettingTextDict = Record<string, SettingTextEntry>;

/** 原文作键的字典（UI chrome、tips）。上游改原文即回退英文，永不显示错译。 */
export type TextDict = Record<string, string>;

/** 单条内置 slash 命令的译文。命令名与子命令名是用户输入的 token，永不翻译。 */
export interface CommandTextEntry {
	description?: string;
	/** 子命令描述，按子命令名索引。 */
	subcommands?: Record<string, string>;
}

/** 按内置 slash 命令名索引。同设置项，用稳定标识而非英文原文作键。 */
export type CommandTextDict = Record<string, CommandTextEntry>;

export interface LocaleBundle {
	readonly id: LocaleId;
	/** 设置项译文，按 setting path 索引。 */
	readonly settings: SettingTextDict;
	/** 设置面板 tab 译文，按 `SettingTab` 索引。 */
	readonly tabs: TextDict;
	/** 设置面板分组标题译文，按 `TAB_GROUPS` 里的英文组名索引。 */
	readonly groups: TextDict;
	/** 界面固定文案，按英文原文索引。 */
	readonly chrome: TextDict;
	/** 欢迎页 tips，按英文原文索引（已剥离 `[NEW]` 标记）。 */
	readonly tips: TextDict;
	/** 内置 slash 命令描述，按命令名索引。 */
	readonly commands: CommandTextDict;
}
