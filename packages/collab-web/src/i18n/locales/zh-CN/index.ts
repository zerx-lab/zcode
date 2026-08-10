import type { Dict } from "../../types";
import { chrome } from "./chrome";
import { toolAgents } from "./tools/agents";
import { toolCore } from "./tools/core";
import { toolExec } from "./tools/exec";
import { toolFs } from "./tools/fs";
import { toolMemory } from "./tools/memory";
import { toolNet } from "./tools/net";

/**
 * Simplified Chinese bundle. Keys are the English source strings; later spreads
 * win, so `chrome` stays authoritative for words shared with tool renderers
 * (`running`, `error`, `context`, …).
 */
export const zhCN: Dict = { ...toolCore, ...toolFs, ...toolExec, ...toolAgents, ...toolNet, ...toolMemory, ...chrome };
