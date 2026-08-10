import { BRAND_DISPLAY_NAME, BRAND_MARK } from "@oh-my-pi/pi-utils/brand-consts";
import type { FormEvent, ReactNode } from "react";
import { useState } from "react";
import { useI18n } from "../../i18n";
import { LangToggle } from "./LangToggle";
import { ThemeToggle } from "./ThemeToggle";

export interface ConnectScreenProps {
	defaultName: string;
	error: string | null;
	onConnect(link: string, name: string): void;
}

export function ConnectScreen({ defaultName, error, onConnect }: ConnectScreenProps): ReactNode {
	const { t, tf } = useI18n();
	const [link, setLink] = useState("");
	const [name, setName] = useState(defaultName);
	const [localError, setLocalError] = useState<string | null>(null);

	const submit = (e: FormEvent<HTMLFormElement>): void => {
		e.preventDefault();
		const trimmed = link.trim();
		if (!trimmed) {
			setLocalError(t("paste a join link first"));
			return;
		}
		setLocalError(null);
		onConnect(trimmed, name.trim() || "guest");
	};

	const shown = localError ?? error;

	return (
		<div className="sh-connect">
			<form className="sh-connect-card" onSubmit={submit}>
				<div className="sh-connect-head">
					<div className="sh-lockup">
						<span className="sh-lockup-mark" aria-hidden="true" />
						<span className="sh-lockup-pi">{BRAND_MARK}</span> {BRAND_DISPLAY_NAME} collab
					</div>
					<div className="sh-connect-toggles">
						<LangToggle />
						<ThemeToggle />
					</div>
				</div>
				<div className="sh-connect-sub">{t("live agent session, in your browser")}</div>
				<label className="sh-field">
					<span className="sh-field-label">{t("join link")}</span>
					<input
						className="sh-input sh-input-mono"
						type="text"
						value={link}
						onChange={e => setLink(e.target.value)}
						placeholder="ws://host:port/r/room.key"
						spellCheck={false}
						autoComplete="off"
						autoFocus
					/>
					<span className="sh-field-hint">
						{tf("paste a /collab link from any {0} session", BRAND_DISPLAY_NAME)}
					</span>
				</label>
				<label className="sh-field">
					<span className="sh-field-label">{t("display name")}</span>
					<input
						className="sh-input"
						type="text"
						value={name}
						onChange={e => setName(e.target.value)}
						placeholder="guest"
						spellCheck={false}
						autoComplete="off"
						maxLength={32}
					/>
				</label>
				{shown && <div className="sh-connect-error">{shown}</div>}
				<button className="sh-btn sh-btn-primary sh-connect-submit" type="submit">
					{t("Connect")}
				</button>
			</form>
		</div>
	);
}
