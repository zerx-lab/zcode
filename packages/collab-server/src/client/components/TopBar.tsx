import { useRouterState } from "@tanstack/react-router";
import { Menu } from "lucide-react";
import { navRoutes } from "../nav-routes";
import { ThemeToggle } from "./ThemeToggle";

export function TopBar({ onMenuToggle }: { onMenuToggle: () => void }) {
	const pathname = useRouterState({ select: state => state.location.pathname });
	const title = navRoutes.find(route => route.path === pathname)?.label ?? "运维面板";

	return (
		<header className="cd-top-bar">
			<div className="cd-top-bar-left">
				<button type="button" onClick={onMenuToggle} className="cd-mobile-menu-btn" aria-label="打开导航菜单">
					<Menu size={20} />
				</button>
				<h1 className="cd-page-title">{title}</h1>
			</div>

			<div className="cd-top-bar-right">
				<ThemeToggle />
			</div>
		</header>
	);
}
