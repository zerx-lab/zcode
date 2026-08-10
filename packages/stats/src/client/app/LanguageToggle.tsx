import { Languages } from "lucide-react";
import { LOCALE_LABELS, type LocaleId, setLocale, useLocale } from "../i18n";

const NEXT_LOCALE: Record<LocaleId, LocaleId> = {
	en: "zh-CN",
	"zh-CN": "en",
};

/** 界面语言切换（en ⇄ zh-CN）。提示文案用**目标语言**书写，缺译也能看懂。 */
export function LanguageToggle() {
	const locale = useLocale();
	const next = NEXT_LOCALE[locale];
	const hint = next === "zh-CN" ? "切换到简体中文" : "Switch to English";

	return (
		<button
			type="button"
			className="stats-theme-toggle"
			onClick={() => setLocale(next)}
			aria-label={`${LOCALE_LABELS[locale]} → ${LOCALE_LABELS[next]}`}
			title={hint}
		>
			<Languages size={16} />
		</button>
	);
}
