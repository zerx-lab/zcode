import type { LocaleBundle } from "../../types";
import { ZH_CN_CHROME } from "./chrome";
import { ZH_CN_COMMANDS } from "./commands";
import { ZH_CN_SETTINGS } from "./settings";
import { ZH_CN_GROUPS, ZH_CN_TABS } from "./tabs";
import { ZH_CN_TIPS } from "./tips";

/** 简体中文语言包。 */
export const ZH_CN: LocaleBundle = {
	id: "zh-CN",
	settings: ZH_CN_SETTINGS,
	tabs: ZH_CN_TABS,
	groups: ZH_CN_GROUPS,
	chrome: ZH_CN_CHROME,
	tips: ZH_CN_TIPS,
	commands: ZH_CN_COMMANDS,
};
