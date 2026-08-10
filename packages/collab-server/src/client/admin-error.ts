import { ApiError } from "./api";

/** Turns an admin-endpoint fetch failure into a Chinese, user-facing message. */
export function describeAdminError(error: unknown): string {
	if (error instanceof ApiError) {
		if (error.status === 401) return "登录已过期或凭据无效，请重新登录";
		if (error.status === 403) return "服务未启用管理功能（需在 .env 设置 COLLAB_ADMIN_PASSWORD）";
		return error.message;
	}
	return error instanceof Error ? error.message : "未知错误";
}

/** True when the error indicates the stored admin session should be discarded and the user re-authenticated. */
export function isAuthError(error: unknown): boolean {
	return error instanceof ApiError && (error.status === 401 || error.status === 403);
}
