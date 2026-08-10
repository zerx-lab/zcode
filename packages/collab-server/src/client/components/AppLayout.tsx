import { BRAND_DISPLAY_NAME } from "@oh-my-pi/pi-utils/brand-consts";
import { Outlet } from "@tanstack/react-router";
import { X } from "lucide-react";
import { useState } from "react";
import { NavRail } from "./NavRail";
import { TopBar } from "./TopBar";

export function AppLayout() {
	const [menuOpen, setMenuOpen] = useState(false);

	return (
		<div className="cd-app-container">
			{/* Desktop Rail */}
			<NavRail className="cd-desktop-nav" />

			{/* Mobile Nav Drawer */}
			{menuOpen && (
				<div className="cd-mobile-drawer-overlay" onClick={() => setMenuOpen(false)} role="presentation">
					<div
						className="cd-mobile-drawer"
						onClick={e => e.stopPropagation()}
						role="dialog"
						aria-modal="true"
						aria-label="导航菜单"
					>
						<div className="cd-mobile-drawer-header">
							<div className="cd-logo-container">
								<span className="cd-logo-text">{BRAND_DISPLAY_NAME.toUpperCase()} COLLAB</span>
								<span className="cd-logo-subtext">运维面板</span>
							</div>
							<button
								type="button"
								onClick={() => setMenuOpen(false)}
								className="cd-drawer-close-btn"
								aria-label="关闭导航菜单"
							>
								<X size={18} />
							</button>
						</div>
						<NavRail className="cd-mobile-nav" onNavigate={() => setMenuOpen(false)} />
					</div>
				</div>
			)}

			{/* Main Layout Pane */}
			<div className="cd-main-pane">
				<TopBar onMenuToggle={() => setMenuOpen(true)} />
				<main className="cd-content-area">
					<div className="cd-content-inner">
						<Outlet />
					</div>
				</main>
			</div>
		</div>
	);
}
