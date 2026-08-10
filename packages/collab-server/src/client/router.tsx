import { createRootRoute, createRoute, createRouter } from "@tanstack/react-router";
import { AppLayout } from "./components/AppLayout";
import { OverviewPage } from "./routes/OverviewPage";
import { SetupPage } from "./routes/SetupPage";
import { SharesPage } from "./routes/SharesPage";

const rootRoute = createRootRoute({
	component: AppLayout,
});

const overviewRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/",
	component: OverviewPage,
});

const sharesRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/shares",
	component: SharesPage,
});

const setupRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/setup",
	component: SetupPage,
});

const routeTree = rootRoute.addChildren([overviewRoute, sharesRoute, setupRoute]);

export const router = createRouter({
	routeTree,
	basepath: "/dash",
});

declare module "@tanstack/react-router" {
	interface Register {
		router: typeof router;
	}
}
