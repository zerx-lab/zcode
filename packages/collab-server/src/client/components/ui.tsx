import { Check, Copy } from "lucide-react";
import type React from "react";
import { useState } from "react";

export function Panel({
	title,
	subtitle,
	actions,
	children,
}: {
	title: string;
	subtitle?: string;
	actions?: React.ReactNode;
	children: React.ReactNode;
}) {
	return (
		<section className="cd-panel">
			<div className="cd-panel-header">
				<div className="cd-panel-header-titles">
					<h2 className="cd-panel-title">{title}</h2>
					{subtitle && <p className="cd-panel-subtitle">{subtitle}</p>}
				</div>
				{actions && <div className="cd-panel-actions">{actions}</div>}
			</div>
			<div className="cd-panel-body">{children}</div>
		</section>
	);
}

export function MetricCard({ label, value }: { label: string; value: React.ReactNode }) {
	return (
		<div className="cd-metric-card">
			<span className="cd-metric-label">{label}</span>
			<span className="cd-metric-value">{value}</span>
		</div>
	);
}

export type StatusVariant = "success" | "danger" | "warning" | "info" | "default";

export function StatusPill({ variant, children }: { variant: StatusVariant; children: React.ReactNode }) {
	return (
		<span className="cd-status-pill" data-variant={variant}>
			{children}
		</span>
	);
}

export interface TableColumn<T> {
	key: string;
	header: string;
	align?: "left" | "right";
	render: (row: T) => React.ReactNode;
}

export function DataTable<T>({
	columns,
	rows,
	rowKey,
	emptyMessage,
}: {
	columns: TableColumn<T>[];
	rows: T[];
	rowKey: (row: T) => string;
	emptyMessage: string;
}) {
	if (rows.length === 0) {
		return <div className="cd-table-empty">{emptyMessage}</div>;
	}
	return (
		<div className="cd-table-container">
			<table className="cd-table">
				<thead>
					<tr>
						{columns.map(col => (
							<th
								key={col.key}
								className={`cd-table-th ${col.align === "right" ? "cd-text-right" : "cd-text-left"}`}
							>
								{col.header}
							</th>
						))}
					</tr>
				</thead>
				<tbody>
					{rows.map(row => (
						<tr key={rowKey(row)} className="cd-table-tr">
							{columns.map(col => (
								<td
									key={col.key}
									className={`cd-table-td ${col.align === "right" ? "cd-text-right" : "cd-text-left"}`}
								>
									{col.render(row)}
								</td>
							))}
						</tr>
					))}
				</tbody>
			</table>
		</div>
	);
}

export function Skeleton({
	variant = "block",
	width,
	height,
}: {
	variant?: "text" | "circle" | "block";
	width?: number | string;
	height?: number | string;
}) {
	return (
		<span
			className="cd-skeleton"
			data-variant={variant}
			style={{ width, height, display: "inline-block" }}
			aria-hidden="true"
		/>
	);
}

export function EmptyState({ message }: { message: string }) {
	return (
		<div className="cd-empty-state">
			<p className="cd-empty-state-message">{message}</p>
		</div>
	);
}

export function ErrorState({ title, message }: { title: string; message: string }) {
	return (
		<div className="cd-error-state">
			<div className="cd-error-state-content">
				<h3 className="cd-error-state-title">{title}</h3>
				<p className="cd-error-state-message">{message}</p>
			</div>
		</div>
	);
}

export function CopyButton({ value, label = "复制" }: { value: string; label?: string }) {
	const [copied, setCopied] = useState(false);

	const handleCopy = async () => {
		try {
			await navigator.clipboard.writeText(value);
			setCopied(true);
			setTimeout(() => setCopied(false), 1500);
		} catch {
			// clipboard API unavailable (insecure context, permission denied) — silently no-op
		}
	};

	return (
		<button type="button" className="cd-copy-btn" onClick={handleCopy}>
			{copied ? <Check size={12} /> : <Copy size={12} />}
			{copied ? "已复制" : label}
		</button>
	);
}

export function CodeBlock({ code, label }: { code: string; label?: string }) {
	return (
		<div className="cd-code-block">
			{label && (
				<div className="cd-code-block-header">
					<span className="cd-code-block-label">{label}</span>
					<CopyButton value={code} />
				</div>
			)}
			<pre className="cd-code-block-pre">
				<code>{code}</code>
			</pre>
		</div>
	);
}
