import { chromiumExecutableProbeForTest } from "@oh-my-pi/pi-coding-agent/tools/browser/launch";

const platform = process.env.OMP_BROWSER_PROBE_PLATFORM;
if (platform) Object.defineProperty(process, "platform", { value: platform });

const target = process.argv[2];
if (!target) throw new Error("missing target path argument");

const result = await chromiumExecutableProbeForTest(target);
process.stdout.write(String(result));
