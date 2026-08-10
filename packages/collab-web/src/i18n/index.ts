/**
 * Tiny UI-localization store for collab-web.
 *
 * Keys are the **English source strings**, so an untranslated (or upstream-edited)
 * string silently renders in English instead of a stale translation. Placeholders
 * are positional: `{0}`, `{1}`, … filled by {@link tf}.
 *
 * Mirrors `lib/theme.ts`: one module-level store, `useSyncExternalStore` readers,
 * `localStorage` persistence, best-effort when storage is blocked.
 */
import { useSyncExternalStore } from "react";
import { zhCN } from "./locales/zh-CN";
import type { Dict, LocaleId, LocalePreference } from "./types";

export type { Dict, LocaleId, LocalePreference } from "./types";

const STORAGE_KEY = "omp-collab-locale";

const BUNDLES: Readonly<Record<LocaleId, Dict>> = { en: {}, "zh-CN": zhCN };

/** Cycle order for the header toggle. */
export const LOCALE_PREFERENCES: readonly LocalePreference[] = ["auto", "en", "zh-CN"];

/** Short glyph shown in the toggle button, and the English label for its tooltip. */
export const LOCALE_META: Readonly<Record<LocalePreference, { badge: string; label: string }>> = {
	auto: { badge: "A", label: "Auto (browser)" },
	en: { badge: "EN", label: "English" },
	"zh-CN": { badge: "中", label: "Simplified Chinese" },
};

/**
 * Simplified Chinese only when the tag is not explicitly Traditional. Sub-tag
 * boundaries are matched with a lookahead rather than `\b` because `_` is a word
 * character (`zh_CN` would otherwise miss).
 */
function matchLocaleTag(tag: string): LocaleId | null {
	const lower = tag.toLowerCase();
	if (!/^zh(?![a-z])/.test(lower)) return /^en(?![a-z])/.test(lower) ? "en" : null;
	if (/(?:^|[-_.])(?:hant|tw|hk|mo)(?![a-z0-9])/.test(lower)) return "en";
	return "zh-CN";
}

function detectLocale(): LocaleId {
	const nav: Navigator | undefined = globalThis.navigator;
	const tags = nav?.languages?.length ? nav.languages : nav?.language ? [nav.language] : [];
	for (const tag of tags) {
		const hit = matchLocaleTag(tag);
		if (hit !== null) return hit;
	}
	return "en";
}

function readStoredPreference(): LocalePreference {
	try {
		const stored = globalThis.localStorage.getItem(STORAGE_KEY);
		return stored === "en" || stored === "zh-CN" || stored === "auto" ? stored : "auto";
	} catch {
		// Private-mode or blocked storage: follow the browser.
		return "auto";
	}
}

let preference: LocalePreference = readStoredPreference();
let locale: LocaleId = preference === "auto" ? detectLocale() : preference;
let dict: Dict = BUNDLES[locale];
const listeners = new Set<() => void>();

export interface Translator {
	readonly locale: LocaleId;
	/** Look up `en`; returns `en` itself when untranslated. */
	t(en: string): string;
	/** Look up `en`, then substitute `{0}`, `{1}`, … with `args`. */
	tf(en: string, ...args: readonly (string | number)[]): string;
}

/** Rebuilt on every locale change so `memo` comparators can key on identity. */
let translator: Translator = buildTranslator();

function buildTranslator(): Translator {
	return { locale, t, tf };
}

function applyLocale(): void {
	locale = preference === "auto" ? detectLocale() : preference;
	dict = BUNDLES[locale];
	translator = buildTranslator();
	if (typeof document !== "undefined") document.documentElement.lang = locale;
}

if (typeof document !== "undefined") document.documentElement.lang = locale;

export function t(en: string): string {
	return dict[en] ?? en;
}

export function tf(en: string, ...args: readonly (string | number)[]): string {
	const template = dict[en] ?? en;
	return template.replace(/\{(\d+)\}/g, (whole, index: string) => {
		const value = args[Number(index)];
		return value === undefined ? whole : String(value);
	});
}

export function setLocalePreference(next: LocalePreference): void {
	preference = next;
	try {
		globalThis.localStorage.setItem(STORAGE_KEY, next);
	} catch {
		// Persistence is best-effort; still apply the in-memory preference.
	}
	applyLocale();
	for (const listener of listeners) listener();
}

function subscribe(callback: () => void): () => void {
	listeners.add(callback);
	return () => listeners.delete(callback);
}

/** Reactive translator; re-renders the caller when the locale changes. */
export function useI18n(): Translator {
	return useSyncExternalStore(
		subscribe,
		() => translator,
		() => translator,
	);
}

/** Reader + writer for the language preference (powers the toggle). */
export function useLocalePreference(): {
	preference: LocalePreference;
	locale: LocaleId;
	setPreference: (next: LocalePreference) => void;
} {
	const pref = useSyncExternalStore(
		subscribe,
		() => preference,
		() => preference,
	);
	const active = useSyncExternalStore(
		subscribe,
		() => locale,
		() => locale,
	);
	return { preference: pref, locale: active, setPreference: setLocalePreference };
}
