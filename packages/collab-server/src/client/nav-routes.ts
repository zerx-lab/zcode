import { Cable, LayoutDashboard, Share2 } from "lucide-react";
import type React from "react";

export interface NavRoute {
	path: "/" | "/shares" | "/setup";
	label: string;
	icon: React.ComponentType<{ size?: number; className?: string }>;
}

/** Nav rail entries, in display order. Titles double as the top-bar page title. */
export const navRoutes: NavRoute[] = [
	{ path: "/", label: "总览", icon: LayoutDashboard },
	{ path: "/shares", label: "分享管理", icon: Share2 },
	{ path: "/setup", label: "接入指南", icon: Cable },
];
