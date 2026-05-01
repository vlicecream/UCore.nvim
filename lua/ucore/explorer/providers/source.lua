local project = require("ucore.project")
local tree = require("ucore.explorer.tree")

local M = {}

local function engine_content_root(engine_root)
	engine_root = engine_root:gsub("\\", "/")
	if vim.fn.isdirectory(engine_root .. "/Source") == 1 then
		return engine_root
	end
	if vim.fn.isdirectory(engine_root .. "/Engine/Source") == 1 then
		return engine_root .. "/Engine"
	end
	return engine_root
end

local function child_if_dir(root, name)
	local path = root .. "/" .. name
	if vim.fn.isdirectory(path) == 1 then
		return tree.from_path(path, {
			id = "source:" .. path,
			label = name,
		})
	end
	return nil
end

function M.load()
	local root = project.find_project_root_from_context()
	if not root then
		return tree.message("Source", "No Unreal project detected")
	end

	local engine, err = project.engine_metadata(root)
	if not engine or not engine.engine_root then
		return tree.message("Source", tostring(err or "Unreal Engine root not resolved"))
	end

	local engine_root = engine_content_root(engine.engine_root)
	local children = {}
	for _, name in ipairs({ "Source", "Plugins", "Config", "Build" }) do
		local node = child_if_dir(engine_root, name)
		if node then
			table.insert(children, node)
		end
	end

	return tree.virtual_group(vim.fn.fnamemodify(engine_root, ":t"), children, {
		id = "source-root:" .. engine_root,
	})
end

return M
