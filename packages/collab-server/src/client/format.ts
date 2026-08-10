/** Human-readable byte size, e.g. `1.2 MB`. */
export function formatBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes < 0) return "—";
	if (bytes === 0) return "0 B";
	const units = ["B", "KB", "MB", "GB", "TB"];
	const exp = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
	const value = bytes / 1024 ** exp;
	return `${exp === 0 ? value : value.toFixed(value < 10 ? 2 : 1)} ${units[exp]}`;
}

/** Human-readable duration from a millisecond count, e.g. `3 天 4 小时 12 分钟`. */
export function formatDuration(ms: number): string {
	if (!Number.isFinite(ms) || ms < 0) return "—";
	const seconds = Math.floor(ms / 1000);
	const days = Math.floor(seconds / 86400);
	const hours = Math.floor((seconds % 86400) / 3600);
	const minutes = Math.floor((seconds % 3600) / 60);
	const secs = seconds % 60;

	const parts: string[] = [];
	if (days > 0) parts.push(`${days} 天`);
	if (hours > 0 || days > 0) parts.push(`${hours} 小时`);
	if (minutes > 0 || hours > 0 || days > 0) parts.push(`${minutes} 分钟`);
	if (days === 0 && hours === 0) parts.push(`${secs} 秒`);
	return parts.join(" ");
}

/** Formats an ISO timestamp as a localized date-time string; falls back to the raw input on parse failure. */
export function formatDateTime(iso: string | null): string {
	if (!iso) return "永久";
	const date = new Date(iso);
	if (Number.isNaN(date.getTime())) return iso;
	return date.toLocaleString();
}

/** Truncates an opaque id to its first `length` characters, appending an ellipsis when clipped. */
export function truncateId(id: string, length = 8): string {
	return id.length > length ? `${id.slice(0, length)}…` : id;
}
