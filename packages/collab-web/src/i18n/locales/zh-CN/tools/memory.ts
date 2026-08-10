import type { Dict } from "../../../types";

/** Memory tool renderers: `recall`, `retain`, `reflect` (`report_tool_issue` has no static copy). */
export const toolMemory: Dict = {
	query: "查询",
	context: "上下文",
	"no matches": "无匹配",
	"{0} found": "找到 {0} 条",
	"as of {0} UTC": "截至 {0} UTC",
	"{0} memories": "{0} 条记忆",
	"{0} memories retained": "已记住 {0} 条记忆",
	"Reflect failed": "反思失败",
};
