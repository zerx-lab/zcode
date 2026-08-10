import { Languages } from "lucide-react";
import type { ReactNode } from "react";
import { LOCALE_META, LOCALE_PREFERENCES, useI18n, useLocalePreference } from "../../i18n";

/** Cycles auto → English → 简体中文, mirroring the theme toggle's single-button pattern. */
export function LangToggle(): ReactNode {
	const { t, tf } = useI18n();
	const { preference, setPreference } = useLocalePreference();
	const meta = LOCALE_META[preference];
	const next = LOCALE_PREFERENCES[(LOCALE_PREFERENCES.indexOf(preference) + 1) % LOCALE_PREFERENCES.length];
	const hint = tf("{0} — click to switch", tf("Language: {0}", t(meta.label)));

	return (
		<button
			type="button"
			className="sh-theme-toggle sh-lang-toggle"
			onClick={() => setPreference(next ?? "auto")}
			aria-label={hint}
			title={hint}
		>
			<Languages size={16} />
			<span className="sh-lang-badge">{meta.badge}</span>
		</button>
	);
}
