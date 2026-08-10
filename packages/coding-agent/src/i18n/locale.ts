import { $env, logger } from "@oh-my-pi/pi-utils";
import { settings } from "../config/settings";
import { LANGUAGE_SETTING_PATH } from "./constants";
import { ZH_CN } from "./locales/zh-CN";
import type { LanguageSetting, LocaleBundle, LocaleId } from "./types";

/** 非 `en` 的语言包。`en` 无字典：取词一律回退英文原文。 */
const BUNDLES: ReadonlyMap<LocaleId, LocaleBundle> = new Map([[ZH_CN.id, ZH_CN]]);

/**
 * 明确的繁体标签。没有繁体语言包，宁可回退英文也不拿简体冒充。
 *
 * 子标签边界写成 `(?![a-z0-9])` 而不是 `\b`：`_` 是单词字符，`zh_Hant_TW` 在
 * `\b` 下会漏判成简体（`zh-Hant-TW` 反而命中，行为按分隔符分裂）。
 */
const ZH_TRADITIONAL = /^zh[-_](?:hant|tw|hk|mo)(?![a-z0-9])/i;

/** 任意简体/未标注地区的中文标签。同理不能用 `\b`，否则 `zh_CN` 漏判。 */
const ZH_ANY = /^zh(?:[-_.]|$)/i;

/** 明确的英文标签。用于把「用户就要英文」与「这门语言没有语言包」区分开。 */
const EN_ANY = /^en(?:[-_.]|$)/i;

/**
 * 语言标签（POSIX `zh_CN.UTF-8` 或 BCP-47 `zh-Hans-CN`）→ 受支持的 locale，
 * 无法支持时返回 `null`。
 *
 * 区分「明确英文」与「不支持」是 `LANGUAGE` 列表需要的：`zh_TW:zh_CN` 里第一项
 * 没有繁体包，必须继续看第二项；而 `en_US:zh_CN` 的第一项是用户的真实首选。
 */
function matchSupportedTag(tag: string): LocaleId | null {
	if (ZH_TRADITIONAL.test(tag)) return null;
	if (ZH_ANY.test(tag)) return "zh-CN";
	return EN_ANY.test(tag) ? "en" : null;
}

/** 单标签解析：不受支持的语言回落 `en`。 */
function matchLocaleTag(tag: string): LocaleId {
	return matchSupportedTag(tag) ?? "en";
}

/**
 * `auto` 的解析源。POSIX 环境变量优先（用户显式意图），Windows 上通常没有这些，
 * 由 ICU 解析出的系统 locale 兜底。
 */
function detectSystemLocale(): LocaleId {
	// LC_ALL / LC_MESSAGES / LANG 各自是单个 locale；LANGUAGE 是 GNU gettext 的
	// 冒号分隔**优先列表**，必须逐项挑第一个有语言包的候选，否则 `de_DE:zh_CN`
	// 会被当成一个无法识别的标签而丢掉可用的第二候选。
	const single = $env.LC_ALL || $env.LC_MESSAGES || $env.LANG;
	if (single) return matchLocaleTag(single);

	const preferences = $env.LANGUAGE;
	if (preferences) {
		for (const tag of preferences.split(":")) {
			const matched = tag && matchSupportedTag(tag);
			if (matched) return matched;
		}
		return "en";
	}

	try {
		return matchLocaleTag(new Intl.DateTimeFormat().resolvedOptions().locale);
	} catch {
		return "en";
	}
}

function resolveLocale(): LocaleId {
	let setting: LanguageSetting;
	try {
		setting = settings.get(LANGUAGE_SETTING_PATH);
	} catch {
		// No Settings instance (early CLI boot, library embedding, unit tests):
		// there is no user preference to honour, so stay on the identity language.
		// Probing the host locale here would flip UI-string tests to Chinese on a
		// zh machine depending on which file ran first.
		return "en";
	}
	const resolved = setting === "auto" ? detectSystemLocale() : setting;
	return BUNDLES.has(resolved) || resolved === "en" ? resolved : "en";
}

let cachedLocale: LocaleId | undefined;

/** 当前界面语言。首次调用后缓存；设置变更后调用 {@link refreshLocale}。 */
export function getLocale(): LocaleId {
	cachedLocale ??= resolveLocale();
	return cachedLocale;
}

/** 重新解析语言并返回新值。设置项变更后由 UI 调用。 */
export function refreshLocale(): LocaleId {
	const previous = cachedLocale;
	cachedLocale = undefined;
	const next = getLocale();
	if (previous !== undefined && previous !== next) {
		logger.debug("i18n locale changed", { from: previous, to: next });
	}
	return next;
}

/** 当前语言包；`en`（或缺字典）返回 undefined，调用方回退英文原文。 */
export function activeBundle(): LocaleBundle | undefined {
	return BUNDLES.get(getLocale());
}
