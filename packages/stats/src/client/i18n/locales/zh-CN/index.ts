/** zh-CN 语言包：按仪表盘区域分文件，这里合并成单字典。 */
import { appDict } from "./app";
import { behaviorDict } from "./behavior";
import { chartsDict } from "./charts";
import { costsDict } from "./costs";
import { errorsDict } from "./errors";
import { gainDict } from "./gain";
import { modelsDict } from "./models";
import { overviewDict } from "./overview";
import { projectsDict } from "./projects";
import { providersDict } from "./providers";
import { requestsDict } from "./requests";
import { toolsDict } from "./tools";
import { uiDict } from "./ui";

export const zhCN: Record<string, string> = {
	...appDict,
	...uiDict,
	...chartsDict,
	...overviewDict,
	...requestsDict,
	...errorsDict,
	...modelsDict,
	...providersDict,
	...toolsDict,
	...costsDict,
	...behaviorDict,
	...projectsDict,
	...gainDict,
};
