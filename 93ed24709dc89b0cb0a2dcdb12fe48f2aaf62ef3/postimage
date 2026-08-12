import { USER_AGENT } from "@oh-my-pi/pi-utils";
import { BRAND_DISPLAY_NAME, BRAND_REPO } from "@oh-my-pi/pi-utils/brand";

export function getOpenRouterHeaders(): Record<string, string> {
	return {
		"User-Agent": USER_AGENT,
		"HTTP-Referer": `https://github.com/${BRAND_REPO}`,
		"X-OpenRouter-Title": BRAND_DISPLAY_NAME,
		"X-OpenRouter-Categories": "cli-agent",
		"X-OpenRouter-Cache": "true",
		"X-OpenRouter-Cache-TTL": "3600",
	};
}
