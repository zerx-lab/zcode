import { BRAND_DISPLAY_NAME } from "@oh-my-pi/pi-utils/brand-consts";
import { Link } from "@tanstack/react-router";
import { navRoutes } from "../nav-routes";

export function NavRail({ className = "", onNavigate }: { className?: string; onNavigate?: () => void }) {
	return (
		<aside className={`cd-nav-rail ${className}`}>
			<div className="cd-nav-rail-header">
				<div className="cd-logo-container">
					<span className="cd-logo-text">{BRAND_DISPLAY_NAME.toUpperCase()} COLLAB</span>
					<span className="cd-logo-subtext">运维面板</span>
				</div>
			</div>

			<nav className="cd-nav-rail-menu">
				{navRoutes.map(route => {
					const Icon = route.icon;
					return (
						<Link
							key={route.path}
							to={route.path}
							activeOptions={{ exact: route.path === "/" }}
							activeProps={{ "data-active": "true" }}
							className="cd-nav-rail-item"
							onClick={onNavigate}
						>
							<Icon size={16} className="cd-nav-rail-item-icon" />
							<span>{route.label}</span>
						</Link>
					);
				})}
			</nav>

			<div className="cd-nav-rail-footer">
				<span className="cd-version-tag">{BRAND_DISPLAY_NAME.toUpperCase()} Collab Server</span>
			</div>
		</aside>
	);
}
