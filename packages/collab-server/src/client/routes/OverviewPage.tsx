import { useQuery } from "@tanstack/react-query";
import { describeAdminError } from "../admin-error";
import { fetchRooms, fetchStatus, getSessionToken } from "../api";
import { DataTable, EmptyState, ErrorState, MetricCard, Panel, Skeleton, StatusPill } from "../components/ui";
import { formatBytes, formatDateTime, formatDuration, truncateId } from "../format";

const STATUS_POLL_MS = 5000;

export function OverviewPage() {
	const hasSession = getSessionToken() !== null;

	const statusQuery = useQuery({
		queryKey: ["status"],
		queryFn: fetchStatus,
		refetchInterval: STATUS_POLL_MS,
	});

	const roomsQuery = useQuery({
		queryKey: ["admin", "rooms"],
		queryFn: fetchRooms,
		enabled: hasSession,
		refetchInterval: STATUS_POLL_MS,
	});

	if (statusQuery.isPending) return <OverviewSkeleton />;

	if (statusQuery.isError) {
		return (
			<ErrorState
				title="无法获取服务状态"
				message={statusQuery.error instanceof Error ? statusQuery.error.message : "未知错误"}
			/>
		);
	}

	const status = statusQuery.data;

	return (
		<div className="cd-page-stack">
			<div className="cd-metric-grid">
				<MetricCard label="在线房间" value={status.rooms.count} />
				<MetricCard label="在线访客" value={status.rooms.guests} />
				<MetricCard label="分享数" value={status.shares.count} />
				<MetricCard label="分享占用" value={formatBytes(status.shares.bytes)} />
			</div>

			<Panel
				title="服务信息"
				actions={<StatusPill variant={status.ok ? "success" : "danger"}>{status.ok ? "健康" : "异常"}</StatusPill>}
			>
				<div className="cd-kv-grid">
					<KvRow label="版本" value={status.version} />
					<KvRow label="运行时长" value={formatDuration(status.uptimeMs)} />
					<KvRow label="启动时间" value={formatDateTime(status.startedAt)} />
					<KvRow label="分享大小上限" value={formatBytes(status.limits.shareMaxBytes)} />
					<KvRow label="单房间访客上限" value={String(status.limits.maxGuests)} />
					<KvRow label="分享保留天数" value={`${status.limits.shareTtlDays} 天`} />
				</div>
			</Panel>

			{hasSession && (
				<Panel title="房间列表" subtitle="当前在线的协作房间">
					{roomsQuery.isPending ? (
						<Skeleton height={120} />
					) : roomsQuery.isError ? (
						<EmptyState message={describeAdminError(roomsQuery.error)} />
					) : (
						<DataTable
							columns={[
								{ key: "id", header: "房间 ID", render: room => truncateId(room.id) },
								{ key: "guests", header: "访客数", align: "right", render: room => room.guests },
								{ key: "createdAt", header: "创建时间", render: room => formatDateTime(room.createdAt) },
							]}
							rows={roomsQuery.data.rooms}
							rowKey={room => room.id}
							emptyMessage="暂无在线房间"
						/>
					)}
				</Panel>
			)}
		</div>
	);
}

function KvRow({ label, value }: { label: string; value: string }) {
	return (
		<div className="cd-kv-row">
			<span className="cd-kv-key">{label}</span>
			<span className="cd-kv-value">{value}</span>
		</div>
	);
}

function OverviewSkeleton() {
	return (
		<div className="cd-page-stack">
			<div className="cd-metric-grid">
				{Array.from({ length: 4 }, (_, i) => (
					<div key={i} className="cd-metric-card">
						<Skeleton variant="text" width="60%" />
						<Skeleton height={24} width="40%" />
					</div>
				))}
			</div>
			<Skeleton height={180} />
		</div>
	);
}
