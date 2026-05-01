local project = require("ucore.project")
local tree = require("ucore.explorer.tree")

local M = {}

function M.load()
	local root = project.find_project_root_from_context()
	if not root then
		return tree.message("Project", "No Unreal project detected")
	end

	return tree.from_path(root, {
		id = "project:" .. root,
		label = vim.fn.fnamemodify(root, ":t"),
	})
end

return M
