import type { CommandTextDict } from "../../../types";
import { COMMANDS_PART1 } from "./part1";
import { COMMANDS_PART2 } from "./part2";
import { COMMANDS_PART3 } from "./part3";

/**
 * 内置 slash 命令描述译文，按命令名索引。
 *
 * 分片纯粹是为了让文件保持可读，与上游的 `builtin-*.ts` 分组无关 ——
 * 上游把某条命令挪到另一个 `builtin-*.ts` 不会影响这里。
 */
export const ZH_CN_COMMANDS: CommandTextDict = {
	...COMMANDS_PART1,
	...COMMANDS_PART2,
	...COMMANDS_PART3,
};
