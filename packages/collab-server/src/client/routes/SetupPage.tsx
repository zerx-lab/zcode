import { CodeBlock, DataTable, Panel } from "../components/ui";

interface AccessLinks {
	relayUrl: string;
	serverUrl: string;
	webUrl: string;
}

/** Derives the relay/share/web links from the current origin, downgrading to ws:// over plain HTTP. */
function computeAccessLinks(): AccessLinks {
	if (typeof window === "undefined") {
		return { relayUrl: "wss://<本站域名>", serverUrl: "https://<本站域名>/s", webUrl: "https://<本站域名>" };
	}
	const { protocol, host } = window.location;
	const wsProtocol = protocol === "https:" ? "wss:" : "ws:";
	return {
		relayUrl: `${wsProtocol}//${host}`,
		serverUrl: `${protocol}//${host}/s`,
		webUrl: `${protocol}//${host}`,
	};
}

interface EnvVarDoc {
	name: string;
	description: string;
	example: string;
}

const ENV_VARS: EnvVarDoc[] = [
	{ name: "COLLAB_PORT", description: "服务监听端口", example: "8787" },
	{ name: "COLLAB_DATA_DIR", description: "数据存储目录（房间快照、分享 blob）", example: "/var/lib/zcode-collab" },
	{ name: "COLLAB_ADMIN_USER", description: "管理员用户名（默认 admin）", example: "admin" },
	{
		name: "COLLAB_ADMIN_PASSWORD",
		description: "管理员密码；不设置时管理功能整体禁用（登录与管理端点固定返回 403）",
		example: "openssl rand -hex 16 生成",
	},
	{ name: "COLLAB_SHARE_TTL_DAYS", description: "分享默认过期天数", example: "30" },
	{ name: "COLLAB_MAX_GUESTS", description: "单个房间最大同时在线访客数", example: "16" },
	{ name: "COLLAB_MAX_SHARE_BYTES", description: "单次分享允许的最大字节数", example: "10485760" },
];

export function SetupPage() {
	const { relayUrl, serverUrl, webUrl } = computeAccessLinks();
	const settingsSnippet = [
		"{",
		`  "collab.relayUrl": "${relayUrl}",`,
		`  "share.serverUrl": "${serverUrl}",`,
		`  "collab.webUrl": "${webUrl}"`,
		"}",
	].join("\n");

	return (
		<div className="cd-page-stack">
			<Panel
				title="接入配置"
				subtitle="把下面的配置写入 zcode 的 settings.json，或在 /settings 图形界面里填入对应字段"
			>
				<CodeBlock label="settings.json 片段" code={settingsSnippet} />
				<p className="cd-hint-text">
					当本站通过 HTTP（非 HTTPS）访问时，relay 地址会自动降级为 <code>ws://</code>；生产部署请务必启用
					HTTPS/WSS——浏览器端的剪贴板与 WebCrypto API 都要求安全上下文。
				</p>
			</Panel>

			<Panel title="服务端环境变量" subtitle="部署本服务时可配置的环境变量">
				<DataTable
					columns={[
						{ key: "name", header: "变量", render: v => <code>{v.name}</code> },
						{ key: "description", header: "说明", render: v => v.description },
						{ key: "example", header: "示例", render: v => <code>{v.example}</code> },
					]}
					rows={ENV_VARS}
					rowKey={v => v.name}
					emptyMessage="—"
				/>
			</Panel>

			<Panel title="部署提示">
				<ul className="cd-tip-list">
					<li>
						推荐用 <code>bun run collab:server:build</code> 产出的单文件 Linux x64 二进制
						<code>dist/collab-server</code>：前端资源已内嵌，上传这一个文件即可运行（首次启动释放到系统 tmp
						目录）。
					</li>
					<li>
						反向代理（Nginx / Caddy / Traefik）必须支持 WebSocket upgrade（透传 <code>Connection: Upgrade</code>
						头），否则协作 relay 连接无法建立。
					</li>
					<li>
						反向代理的请求体大小上限需 ≥ 分享大小上限（<code>COLLAB_MAX_SHARE_BYTES</code>），Nginx 对应
						<code>client_max_body_size</code>，Caddy 默认无限制。
					</li>
					<li>生产环境务必启用 HTTPS/WSS；自签名证书或明文 HTTP 会导致浏览器端安全上下文相关 API 不可用。</li>
					<li>
						管理员凭据写在服务运行目录的 <code>.env</code> 文件（Bun 自动加载）：<code>COLLAB_ADMIN_USER</code> /
						<code>COLLAB_ADMIN_PASSWORD</code>。不设置密码时本面板的房间 / 分享管理功能整体禁用。
					</li>
				</ul>
			</Panel>
		</div>
	);
}
