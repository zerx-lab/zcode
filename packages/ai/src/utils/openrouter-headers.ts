import { BRAND_DISPLAY_NAME } from "@oh-my-pi/pi-utils/brand";
import packageJson from "../../package.json" with { type: "json" };

export function getOpenRouterHeaders(): Record<string, string> {
	return {
		"User-Agent": `${BRAND_DISPLAY_NAME}/${packageJson.version}`,
		"HTTP-Referer": "https://github.com/zerx-lab/oh-my-pi",
		"X-OpenRouter-Title": BRAND_DISPLAY_NAME,
		"X-OpenRouter-Categories": "cli-agent",
		"X-OpenRouter-Cache": "true",
		"X-OpenRouter-Cache-TTL": "3600",
	};
}
