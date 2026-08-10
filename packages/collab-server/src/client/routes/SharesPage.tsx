import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, LogOut, Trash2 } from "lucide-react";
import type React from "react";
import { useState } from "react";
import { describeAdminError, isAuthError } from "../admin-error";
import { deleteShare, fetchShares, getSessionToken, login, logout, setSessionToken } from "../api";
import { DataTable, ErrorState, Panel, Skeleton } from "../components/ui";
import { formatBytes, formatDateTime, truncateId } from "../format";

export function SharesPage() {
	const [authed, setAuthed] = useState(() => getSessionToken() !== null);

	if (!authed) {
		return <LoginGate onLoggedIn={() => setAuthed(true)} />;
	}

	return <SharesTable onLoggedOut={() => setAuthed(false)} />;
}

function LoginGate({ onLoggedIn }: { onLoggedIn: () => void }) {
	const [username, setUsername] = useState("admin");
	const [password, setPassword] = useState("");

	const loginMutation = useMutation({
		mutationFn: () => login(username.trim(), password),
		onSuccess: onLoggedIn,
	});

	const handleSubmit = (event: React.FormEvent<HTMLFormElement>) => {
		event.preventDefault();
		if (username.trim().length === 0 || password.length === 0) return;
		loginMutation.mutate();
	};

	return (
		<Panel title="管理员登录" subtitle="需要登录后才能查看和删除分享">
			<form className="cd-token-form" onSubmit={handleSubmit}>
				<label className="cd-token-form-label" htmlFor="admin-username-input">
					<KeyRound size={14} />
					管理员账号
				</label>
				<div className="cd-token-form-row">
					<input
						id="admin-username-input"
						type="text"
						className="cd-token-input"
						placeholder="用户名（COLLAB_ADMIN_USER，默认 admin）"
						value={username}
						onChange={event => setUsername(event.target.value)}
						autoComplete="username"
					/>
				</div>
				<div className="cd-token-form-row">
					<input
						id="admin-password-input"
						type="password"
						className="cd-token-input"
						placeholder="密码（COLLAB_ADMIN_PASSWORD）"
						value={password}
						onChange={event => setPassword(event.target.value)}
						autoComplete="current-password"
					/>
					<button
						type="submit"
						className="cd-button cd-button-primary"
						disabled={loginMutation.isPending || username.trim().length === 0 || password.length === 0}
					>
						{loginMutation.isPending ? "登录中…" : "登录"}
					</button>
				</div>
				{loginMutation.isError && <p className="cd-form-error">{describeAdminError(loginMutation.error)}</p>}
				<p className="cd-hint-text">凭据在服务端 .env 中配置；浏览器只保存登录后的临时会话，不保存密码。</p>
			</form>
		</Panel>
	);
}

function SharesTable({ onLoggedOut }: { onLoggedOut: () => void }) {
	const queryClient = useQueryClient();

	const sharesQuery = useQuery({
		queryKey: ["admin", "shares"],
		queryFn: fetchShares,
	});

	const deleteMutation = useMutation({
		mutationFn: deleteShare,
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["admin", "shares"] });
		},
	});

	const handleLogout = async () => {
		await logout().catch(() => {});
		queryClient.removeQueries({ queryKey: ["admin"] });
		onLoggedOut();
	};

	const handleSessionReset = () => {
		setSessionToken(null);
		queryClient.removeQueries({ queryKey: ["admin"] });
		onLoggedOut();
	};

	const handleDelete = (id: string) => {
		if (!window.confirm(`确定删除分享 ${truncateId(id)} 吗？此操作不可撤销。`)) return;
		deleteMutation.mutate(id);
	};

	if (sharesQuery.isError) {
		return (
			<Panel title="分享管理">
				<ErrorState title="无法获取分享列表" message={describeAdminError(sharesQuery.error)} />
				{isAuthError(sharesQuery.error) && (
					<div className="cd-panel-footer-actions">
						<button type="button" className="cd-button cd-button-secondary" onClick={handleSessionReset}>
							重新登录
						</button>
					</div>
				)}
			</Panel>
		);
	}

	return (
		<Panel
			title="分享管理"
			subtitle={sharesQuery.data ? `共 ${sharesQuery.data.shares.length} 个分享` : undefined}
			actions={
				<button type="button" className="cd-button cd-button-secondary" onClick={handleLogout}>
					<LogOut size={14} />
					退出登录
				</button>
			}
		>
			{sharesQuery.isPending ? (
				<Skeleton height={160} />
			) : (
				<DataTable
					columns={[
						{ key: "id", header: "分享 ID", render: share => truncateId(share.id) },
						{ key: "bytes", header: "大小", align: "right", render: share => formatBytes(share.bytes) },
						{ key: "createdAt", header: "创建时间", render: share => formatDateTime(share.createdAt) },
						{ key: "expiresAt", header: "过期时间", render: share => formatDateTime(share.expiresAt) },
						{ key: "downloads", header: "下载次数", align: "right", render: share => share.downloads },
						{
							key: "actions",
							header: "",
							align: "right",
							render: share => (
								<button
									type="button"
									className="cd-icon-btn cd-icon-btn-danger"
									onClick={() => handleDelete(share.id)}
									disabled={deleteMutation.isPending && deleteMutation.variables === share.id}
									aria-label="删除分享"
									title="删除分享"
								>
									<Trash2 size={14} />
								</button>
							),
						},
					]}
					rows={sharesQuery.data.shares}
					rowKey={share => share.id}
					emptyMessage="暂无分享"
				/>
			)}
		</Panel>
	);
}
