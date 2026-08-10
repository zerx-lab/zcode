import type { SettingTextDict } from "../../../types";
import { APPEARANCE_SETTINGS } from "./appearance";
import { CONTEXT_SETTINGS } from "./context";
import { FILES_SETTINGS } from "./files";
import { INTERACTION_SETTINGS } from "./interaction";
import { MEMORY_SETTINGS } from "./memory";
import { MODEL_SETTINGS } from "./model";
import { PROVIDERS_SETTINGS } from "./providers";
import { SHELL_SETTINGS } from "./shell";
import { TASKS_SETTINGS } from "./tasks";
import { TOOLS_SETTINGS } from "./tools";

/**
 * 全部设置项译文，按 setting path 索引。按设置面板 tab 分文件维护，
 * 与 `SETTINGS_SCHEMA` 的 tab 划分一一对应，便于跟随上游增删条目。
 */
export const ZH_CN_SETTINGS: SettingTextDict = {
	...APPEARANCE_SETTINGS,
	...MODEL_SETTINGS,
	...INTERACTION_SETTINGS,
	...CONTEXT_SETTINGS,
	...MEMORY_SETTINGS,
	...FILES_SETTINGS,
	...SHELL_SETTINGS,
	...TOOLS_SETTINGS,
	...TASKS_SETTINGS,
	...PROVIDERS_SETTINGS,
};
