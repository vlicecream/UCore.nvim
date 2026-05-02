local project = require("ucore.project")
local tree = require("ucore.explorer.tree")

local M = {}

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function sorted_glob(pattern)
	local entries = vim.fn.glob(pattern, false, true) or {}
	table.sort(entries, function(a, b)
		return tostring(a):lower() < tostring(b):lower()
	end)
	return entries
end

local function read_dir(path)
	path = normalize(path)
	if not path or vim.fn.isdirectory(path) ~= 1 then
		return {}
	end

	local ok, names = pcall(vim.fn.readdir, path)
	if not ok then
		return {}
	end

	local entries = {}
	for _, name in ipairs(names or {}) do
		table.insert(entries, normalize(path .. "/" .. name))
	end

	table.sort(entries, function(a, b)
		return tostring(a):lower() < tostring(b):lower()
	end)

	return entries
end

local function file_node(path)
	path = normalize(path)
	return {
		id = "config-file:" .. tostring(path),
		label = vim.fn.fnamemodify(path, ":t"),
		path = path,
		type = "file",
		children = {},
	}
end

local function config_file_group(path, label, opts)
	opts = opts or {}
	path = normalize(path)
	if not path or vim.fn.isdirectory(path) ~= 1 then
		return nil
	end

	return tree.virtual_group(label, {}, {
		id = opts.id or ("config-files:" .. label .. ":" .. path),
		loaded = false,
		load_children = function()
			local children = {}

			for _, file in ipairs(read_dir(path)) do
				file = normalize(file)
				if vim.fn.isdirectory(file) ~= 1 and file:lower():match("%.ini$") then
					table.insert(children, file_node(file))
				end
			end

			return children
		end,
	})
end

local function config_dir_node(path, label, opts)
	opts = opts or {}
	path = normalize(path)
	if not path or vim.fn.isdirectory(path) ~= 1 then
		return nil
	end

	return tree.virtual_group(label, {}, {
		id = opts.id or ("config-dir:" .. label .. ":" .. path),
		loaded = false,
		load_children = function()
			local children = {}

			for _, dir in ipairs(read_dir(path)) do
				dir = normalize(dir)
				if vim.fn.isdirectory(dir) == 1 then
					local child = config_dir_node(dir, vim.fn.fnamemodify(dir, ":t"), {
						id = "config-dir:" .. dir,
					})
					if child then
						table.insert(children, child)
					end
				elseif dir:lower():match("%.ini$") then
					table.insert(children, file_node(dir))
				end
			end

			return children
		end,
	})
end

local function plugin_config_groups(root)
	local groups = {}
	for _, dir in ipairs(sorted_glob(root .. "/Plugins/*/Config")) do
		dir = normalize(dir)
		if vim.fn.isdirectory(dir) == 1 then
			local plugin_name = vim.fn.fnamemodify(vim.fn.fnamemodify(dir, ":h"), ":t")
			local group = config_dir_node(dir, plugin_name .. "/Config", {
				id = "config-plugin:" .. plugin_name .. ":" .. dir,
			})
			if group then
				table.insert(groups, group)
			end
		end
	end
	return groups
end

local function grouped_children(label, children, id)
	return tree.virtual_group(label, children, {
		id = id,
		loaded = true,
	})
end

function M.load()
	local root = project.find_project_root_from_context()
	if not root then
		return tree.message("Config", "No Unreal project detected")
	end

	root = normalize(root)
	local children = {}

	local project_config = config_file_group(root .. "/Config", "Project Config", {
		id = "config-project-root:" .. root,
	})
	if project_config then
		table.insert(children, project_config)
	end

	local project_platform = {}
	for _, dir in ipairs(read_dir(root .. "/Config")) do
		dir = normalize(dir)
		if vim.fn.isdirectory(dir) == 1 then
			local group = config_dir_node(dir, "Config/" .. vim.fn.fnamemodify(dir, ":t"), {
				id = "config-project-platform-dir:" .. dir,
			})
			if group then
				table.insert(project_platform, group)
			end
		end
	end
	table.insert(children, grouped_children("Project Platform Config", project_platform, "config-project-platform:" .. root))

	table.insert(children, grouped_children("Plugin Config", plugin_config_groups(root), "config-plugin-root:" .. root))

	local engine, _ = project.engine_metadata(root)
	if engine and engine.engine_root then
		local engine_root = normalize(engine.engine_root)
		local engine_config = config_file_group(engine_root .. "/Engine/Config", "Engine Config", {
			id = "config-engine-root:" .. engine_root,
		}) or config_file_group(engine_root .. "/Config", "Engine Config", {
			id = "config-engine-root-fallback:" .. engine_root,
		})
		if engine_config then
			table.insert(children, engine_config)
		end

		local engine_platform = {}
		local platform_root = normalize(engine_root .. "/Engine/Config")
		for _, dir in ipairs(read_dir(platform_root)) do
			dir = normalize(dir)
			if vim.fn.isdirectory(dir) == 1 then
				local group = config_dir_node(dir, "Engine/Config/" .. vim.fn.fnamemodify(dir, ":t"), {
					id = "config-engine-platform-dir:" .. dir,
				})
				if group then
					table.insert(engine_platform, group)
				end
			end
		end
		table.insert(
			children,
			grouped_children("Engine Platform Config", engine_platform, "config-engine-platform:" .. tostring(engine_root))
		)
	end

	return grouped_children("Config", children, "config-root:" .. root)
end

return M
