local project = require("ucore.project")
local tree = require("ucore.explorer.tree")

local M = {}

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function ini_files(path, label)
	path = normalize(path)
	if not path or vim.fn.isdirectory(path) ~= 1 then
		return nil
	end

	local files = {}
	for _, file in ipairs(vim.fn.glob(path .. "/**/*.ini", false, true) or {}) do
		file = normalize(file)
		table.insert(files, {
			id = "config-file:" .. file,
			label = vim.fn.fnamemodify(file, ":t"),
			path = file,
			type = "file",
			children = {},
		})
	end

	table.sort(files, function(a, b)
		return (a.path or a.label):lower() < (b.path or b.label):lower()
	end)

	return tree.virtual_group(label, files, {
		id = "config-group:" .. label .. ":" .. path,
	})
end

local function plugin_config_groups(root)
	local groups = {}
	local plugin_dirs = vim.fn.glob(root .. "/Plugins/*/Config", false, true)
	for _, dir in ipairs(plugin_dirs or {}) do
		local plugin_name = vim.fn.fnamemodify(vim.fn.fnamemodify(dir, ":h"), ":t")
		local group = ini_files(dir, plugin_name .. "/Config")
		if group then
			table.insert(groups, group)
		end
	end
	return groups
end

function M.load()
	local root = project.find_project_root_from_context()
	if not root then
		return tree.message("Config", "No Unreal project detected")
	end

	root = normalize(root)
	local children = {}

	local project_config = ini_files(root .. "/Config", "Project Config")
	if project_config then
		table.insert(children, project_config)
	end

	local project_platform = {}
	for _, dir in ipairs(vim.fn.glob(root .. "/Config/*", false, true) or {}) do
		if vim.fn.isdirectory(dir) == 1 then
			local group = ini_files(dir, "Config/" .. vim.fn.fnamemodify(dir, ":t"))
			if group then
				table.insert(project_platform, group)
			end
		end
	end
	table.insert(children, tree.virtual_group("Project Platform Config", project_platform, {
		id = "config-project-platform:" .. root,
	}))

	table.insert(children, tree.virtual_group("Plugin Config", plugin_config_groups(root), {
		id = "config-plugin:" .. root,
	}))

	local engine, _ = project.engine_metadata(root)
	if engine and engine.engine_root then
		local engine_root = normalize(engine.engine_root)
		local engine_config = ini_files(engine_root .. "/Engine/Config", "Engine Config")
			or ini_files(engine_root .. "/Config", "Engine Config")
		if engine_config then
			table.insert(children, engine_config)
		end

		local platform_root = engine_root .. "/Engine/Config"
		local engine_platform = {}
		for _, dir in ipairs(vim.fn.glob(platform_root .. "/*", false, true) or {}) do
			if vim.fn.isdirectory(dir) == 1 then
				local group = ini_files(dir, "Engine/Config/" .. vim.fn.fnamemodify(dir, ":t"))
				if group then
					table.insert(engine_platform, group)
				end
			end
		end
		table.insert(children, tree.virtual_group("Engine Platform Config", engine_platform, {
			id = "config-engine-platform:" .. engine_root,
		}))
	end

	return tree.virtual_group("Config", children, {
		id = "config-root:" .. root,
	})
end

return M
