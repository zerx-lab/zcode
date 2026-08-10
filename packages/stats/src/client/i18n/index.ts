/**
 * stats 仪表盘客户端 i18n（fork 新增，上游不存在此目录）。
 *
 * 设计与 docs/fork/i18n.md 的 chrome 字典一致：**键 = 英文原文**，缺译静默回退
 * 英文——上游改文案的后果是回退，不是错译。语言切换通过 App 根节点的
 * `key={locale}` 整树重挂生效，组件内不需要逐个订阅 locale。
 *
 * 约束：
 * - 禁止在模块顶层调用 t()/tf()（只求值一次，切换语言后不会刷新）；
 *   一律在渲染点取词。
 * - 运行时值（模型 id、数字、时间）经 tf 的 `{n}` 占位注入，不参与翻译。
 */

import { useSyncExternalStore } from "react";
import { zhCN } from "./locales/zh-CN";

export type LocaleId = "en" | "zh-CN";

export const LOCALE_LABELS: Record<LocaleId, string> = {
	en: "English",
	"zh-CN": "简体中文",
};

const STORAGE_KEY = "omp-stats-locale";
const DICTS: Partial<Record<LocaleId, Record<string, string>>> = { "zh-CN": zhCN };

function detectLocale(): LocaleId {
	try {
		const stored = localStorage.getItem(STORAGE_KEY);
		if (stored === "en" || stored === "zh-CN") return stored;
	} catch {
		// localStorage 不可用（隐私模式等）→ 走浏览器语言探测
	}
	if (typeof navigator === "undefined") return "en";
	const tag = (navigator.language ?? "").toLowerCase();
	if (!tag.startsWith("zh")) return "en";
	// 明确繁体（zh-Hant / zh-TW / zh-HK / zh-MO）回退英文，不拿简体冒充。
	if (/hant|[-_](tw|hk|mo)(?![a-z0-9])/.test(tag)) return "en";
	return "zh-CN";
}

let current: LocaleId = detectLocale();
const listeners = new Set<() => void>();

try {
	document.documentElement.lang = current;
} catch {
	// 非浏览器环境（类型检查、SSR 假设）忽略
}

export function getLocale(): LocaleId {
	return current;
}

export function setLocale(locale: LocaleId): void {
	if (locale === current) return;
	current = locale;
	try {
		localStorage.setItem(STORAGE_KEY, locale);
		document.documentElement.lang = locale;
	} catch {
		// 持久化失败不阻塞切换
	}
	for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
	listeners.add(listener);
	return () => listeners.delete(listener);
}

/** React 绑定：App 根用返回值做 `key`，语言切换 → 整树重挂 → 所有 t() 重新求值。 */
export function useLocale(): LocaleId {
	return useSyncExternalStore(subscribe, getLocale, getLocale);
}

/** 取词：键为英文原文；缺译回退原文。 */
export function t(text: string): string {
	if (current === "en") return text;
	return DICTS[current]?.[text] ?? text;
}

/** 带插值取词：模板用 `{0}` `{1}` 占位。 */
export function tf(template: string, ...values: Array<string | number>): string {
	return t(template).replace(/\{(\d+)\}/g, (match, index) => {
		const value = values[Number(index)];
		return value === undefined ? match : String(value);
	});
}
