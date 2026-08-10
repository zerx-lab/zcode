/** `lsp` — language-server queries: diagnostics, definitions, references, hover, rename, … */
import type { ReactNode } from "react";
import { useI18n } from "../../i18n";
import type { Tone } from "../parts";
import { Badge, InvalidArg, Kv, KvGrid, Output, PathText, ResultText, Row } from "../parts";
import type { ToolRenderer, ToolRenderProps } from "../types";
import { detailsRecord, normalizeWs, num, resultTextOf, str, truncate } from "../util";

/** `file:line:col [severity] message` — the diagnostics line format the tool emits. */
const DIAG_RE = /^(.*):(\d+):(\d+)\s+\[(\w+)\]\s*(.*)$/;
/** Bare `file:line:col` location line (references, definitions, implementations). */
const LOC_RE = /^(.+):(\d+):(\d+)$/;
/** Actions whose result text is a list of locations. */
const LOCATION_ACTIONS: Record<string, true> = {
	definition: true,
	references: true,
	type_definition: true,
	implementation: true,
};

const MAX_ROWS = 24;

interface DiagRow {
	file: string;
	line: string;
	col: string;
	severity: string;
	message: string;
}

interface LocRow {
	file: string;
	line: string;
	col: string;
}

function parseDiagnostics(text: string): DiagRow[] {
	const rows: DiagRow[] = [];
	for (const raw of text.split("\n")) {
		const m = raw.trim().match(DIAG_RE);
		if (m) rows.push({ file: m[1], line: m[2], col: m[3], severity: m[4].toLowerCase(), message: normalizeWs(m[5]) });
	}
	return rows;
}

function parseLocations(text: string): LocRow[] {
	const rows: LocRow[] = [];
	for (const raw of text.split("\n")) {
		const m = raw.trim().match(LOC_RE);
		if (m) rows.push({ file: m[1], line: m[2], col: m[3] });
	}
	return rows;
}

function severityTone(severity: string): Tone | undefined {
	switch (severity) {
		case "error":
			return "err";
		case "warning":
			return "warn";
		case "info":
			return "accent";
		default:
			return undefined;
	}
}

/** Kv row for an optional arg: hidden when absent, InvalidArg when mistyped. */
function ArgKv({ k, raw, val }: { k: string; raw: unknown; val: ReactNode }): ReactNode {
	if (raw === undefined) return null;
	return <Kv k={k}>{val == null || val === false ? <InvalidArg what={k} /> : val}</Kv>;
}

function DiagnosticRows({
	text,
	rows,
	tf,
}: {
	text: string;
	rows: DiagRow[];
	tf: (en: string, ...args: readonly (string | number)[]) => string;
}): ReactNode {
	const errMatch = text.match(/(\d+)\s+error\(s\)/);
	const warnMatch = text.match(/(\d+)\s+warning\(s\)/);
	const shown = rows.slice(0, MAX_ROWS);
	return (
		<>
			{(errMatch || warnMatch) && (
				<span className="tv-badges">
					{errMatch && <Badge tone="err">{tf("{0} errors", errMatch[1])}</Badge>}
					{warnMatch && <Badge tone="warn">{tf("{0} warnings", warnMatch[1])}</Badge>}
				</span>
			)}
			<div className="tv-list">
				{shown.map((d, i) => (
					<Row key={i} k={<Badge tone={severityTone(d.severity)}>{d.severity}</Badge>}>
						<PathText path={d.file} sel={`${d.line}:${d.col}`} />
						{d.message && <span className="tv-muted"> {truncate(d.message, 160)}</span>}
					</Row>
				))}
				{rows.length > shown.length && (
					<Row>
						<span className="tv-faint">{tf("… {0} more", rows.length - shown.length)}</span>
					</Row>
				)}
			</div>
		</>
	);
}

function LocationRows({
	text,
	rows,
	tf,
}: {
	text: string;
	rows: LocRow[];
	tf: (en: string, ...args: readonly (string | number)[]) => string;
}): ReactNode {
	const refMatch = text.match(/(\d+)\s+reference\(s\)/);
	const shown = rows.slice(0, MAX_ROWS);
	return (
		<>
			{refMatch && (
				<span className="tv-badges">
					<Badge tone="accent">{tf("{0} references", refMatch[1])}</Badge>
				</span>
			)}
			<div className="tv-list">
				{shown.map((l, i) => (
					<Row key={i}>
						<PathText path={l.file} sel={`${l.line}:${l.col}`} />
					</Row>
				))}
				{rows.length > shown.length && (
					<Row>
						<span className="tv-faint">{tf("… {0} more", rows.length - shown.length)}</span>
					</Row>
				)}
			</div>
		</>
	);
}

function Summary({ args }: ToolRenderProps): ReactNode {
	const { t, tf } = useI18n();
	const action = str(args.action);
	const file = str(args.file);
	const line = num(args.line);
	const symbol = str(args.symbol);
	const query = str(args.query);
	const newName = str(args.new_name);
	return (
		<>
			<Badge tone="accent">{action ? action.replace(/_/g, " ") : t("request")}</Badge>
			{file === "*" && <Badge>{t("workspace")}</Badge>}
			{file && file !== "*" && <PathText path={file} from={line} />}
			{!file && line != null && <span className="tv-faint">{tf("line {0}", line)}</span>}
			{symbol && <span className="tv-pattern">{truncate(normalizeWs(symbol), 48)}</span>}
			{query && <span className="tv-muted">{truncate(normalizeWs(query), 48)}</span>}
			{newName && <span className="tv-muted">→ {truncate(normalizeWs(newName), 48)}</span>}
		</>
	);
}

function Body({ args, result }: ToolRenderProps): ReactNode {
	const { t, tf } = useI18n();
	const details = detailsRecord(result);
	const file = str(args.file);
	const line = num(args.line);
	const symbol = str(args.symbol);
	const query = str(args.query);
	const newName = str(args.new_name);
	const apply = typeof args.apply === "boolean" ? args.apply : null;
	const timeout = num(args.timeout);
	const payload = str(args.payload);
	const serverName = details ? str(details.serverName) : null;
	const action = str(args.action) ?? (details ? str(details.action) : null);

	const text = result && !result.isError ? resultTextOf(result) : "";
	const diags = text ? parseDiagnostics(text) : [];
	const locs =
		diags.length === 0 && text && ((action != null && LOCATION_ACTIONS[action]) || /\d+\s+reference\(s\)/.test(text))
			? parseLocations(text)
			: [];

	return (
		<>
			<KvGrid>
				<ArgKv k="action" raw={args.action} val={str(args.action)?.replace(/_/g, " ")} />
				<ArgKv
					k="file"
					raw={args.file}
					val={file === "*" ? <Badge>{t("workspace")}</Badge> : file && <PathText path={file} from={line} />}
				/>
				{!file && <ArgKv k="line" raw={args.line} val={line} />}
				<ArgKv k="symbol" raw={args.symbol} val={symbol && truncate(normalizeWs(symbol), 120)} />
				<ArgKv k="query" raw={args.query} val={query && truncate(normalizeWs(query), 120)} />
				<ArgKv k="new name" raw={args.new_name} val={newName && truncate(normalizeWs(newName), 120)} />
				<ArgKv k="apply" raw={args.apply} val={apply == null ? null : apply ? t("yes") : t("no")} />
				<ArgKv k="timeout" raw={args.timeout} val={timeout != null && `${timeout}s`} />
				{args.payload !== undefined && payload == null && (
					<Kv k="payload">
						<InvalidArg what="payload" />
					</Kv>
				)}
				{serverName && <Kv k={t("server")}>{serverName}</Kv>}
			</KvGrid>
			{payload && <Output text={payload} lang="json" variant="code" maxLines={8} title="payload" />}
			{diags.length > 0 ? (
				<DiagnosticRows text={text} rows={diags} tf={tf} />
			) : locs.length > 0 ? (
				<LocationRows text={text} rows={locs} tf={tf} />
			) : (
				<ResultText result={result} maxLines={12} />
			)}
		</>
	);
}

export const lspRenderer: ToolRenderer = { Summary, Body };
