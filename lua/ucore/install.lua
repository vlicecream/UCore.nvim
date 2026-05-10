local project = require("ucore.project")

local M = {}

local uv = vim.uv or vim.loop

local source = debug.getinfo(1, "S").source:sub(2)
local repo_root = vim.fn.fnamemodify(source, ":p:h:h:h"):gsub("\\", "/")
local plugin_source_dir = repo_root .. "/NvimSourceCodeAccess"

local function normalize(path)
	if not path or path == "" then
		return nil
	end

	return vim.fn.fnamemodify(path, ":p"):gsub("\\", "/"):gsub("/+$", "")
end

local function path_join(...)
	local parts = { ... }
	local result = table.concat(parts, "/")
	return normalize(result)
end

local function stat(path)
	return path and uv.fs_stat(path) or nil
end

local function is_dir(path)
	local info = stat(path)
	return info and info.type == "directory" or false
end

local function is_file(path)
	local info = stat(path)
	return info and info.type == "file" or false
end

local function mkdirp(path)
	path = normalize(path)
	if not path or path == "" or is_dir(path) then
		return true
	end

	local parent = vim.fn.fnamemodify(path, ":h"):gsub("\\", "/")
	if parent and parent ~= "" and parent ~= path then
		mkdirp(parent)
	end

	local ok = uv.fs_mkdir(path, 493)
	return ok or is_dir(path)
end

local function scandir(path)
	local items = {}
	local handle = uv.fs_scandir(path)
	if not handle then
		return items
	end

	while true do
		local name, kind = uv.fs_scandir_next(handle)
		if not name then
			break
		end
		table.insert(items, {
			name = name,
			kind = kind,
		})
	end

	return items
end

local function rm_rf(path)
	path = normalize(path)
	if not path then
		return true
	end

	local info = stat(path)
	if not info then
		return true
	end

	if info.type == "directory" then
		for _, item in ipairs(scandir(path)) do
			rm_rf(path_join(path, item.name))
		end
		local ok = uv.fs_rmdir(path)
		return ok ~= nil or not stat(path)
	end

	local ok = uv.fs_unlink(path)
	return ok ~= nil or not stat(path)
end

local function copy_file(src, dst)
	local parent = vim.fn.fnamemodify(dst, ":h"):gsub("\\", "/")
	mkdirp(parent)

	local ok, err = uv.fs_copyfile(src, dst)
	if ok then
		return true
	end

	return false, err
end

local function copy_tree(src, dst)
	src = normalize(src)
	dst = normalize(dst)

	if not is_dir(src) then
		return false, "source directory missing: " .. tostring(src)
	end

	mkdirp(dst)

	for _, item in ipairs(scandir(src)) do
		local from = path_join(src, item.name)
		local to = path_join(dst, item.name)

		if item.kind == "directory" then
			local ok, err = copy_tree(from, to)
			if not ok then
				return false, err
			end
		else
			local ok, err = copy_file(from, to)
			if not ok then
				return false, string.format("copy failed: %s -> %s (%s)", from, to, tostring(err))
			end
		end
	end

	return true
end

local function run_system(cmd, opts)
	opts = opts or {}
	local result = vim.system(cmd, {
		text = true,
		cwd = opts.cwd,
		env = opts.env,
	}):wait()

	return result
end

local function command_exists(cmd)
	return vim.fn.executable(cmd) == 1
end

local function repo_plugin_exists()
	return is_dir(plugin_source_dir) and is_file(plugin_source_dir .. "/NvimSourceCodeAccess.uplugin")
end

local function plugin_manifest_path(dir)
	dir = normalize(dir)
	if not dir then
		return nil
	end

	return dir .. "/NvimSourceCodeAccess.uplugin"
end

local function plugin_installed(dir)
	return is_file(plugin_manifest_path(dir))
end

local function project_plugin_target(project_root)
	return path_join(project_root, "Plugins", "Developer", "NvimSourceCodeAccess")
end

local function engine_plugin_target(engine_root)
	return path_join(engine_root, "Engine", "Plugins", "Developer", "NvimSourceCodeAccess")
end

local function ensure_safe_target(scope, root, target)
	local expected
	if scope == "engine" then
		expected = engine_plugin_target(root)
	else
		expected = project_plugin_target(root)
	end

	expected = normalize(expected)
	target = normalize(target)

	if expected ~= target then
		return false, string.format("refusing to write outside expected plugin path: %s", tostring(target))
	end

	return true
end

local function resolve_install_root(scope)
	scope = scope == "engine" and "engine" or "project"

	local project_root = project.find_project_root_from_context({
		registered_fallback = false,
	})

	if not project_root then
		return nil, nil, "not inside an Unreal project"
	end

	if scope == "project" then
		return project_root, project_plugin_target(project_root), nil
	end

	local engine, err = project.engine_metadata(project_root)
	if not engine or not engine.engine_root then
		return nil, nil, err or "could not resolve engine root"
	end

	return engine.engine_root, engine_plugin_target(engine.engine_root), nil
end

local function install_plugin(scope)
	if not repo_plugin_exists() then
		return false, "NvimSourceCodeAccess source is missing under UCore.nvim"
	end

	local root, target_dir, err = resolve_install_root(scope)
	if not root then
		return false, err
	end

	local ok, safe_err = ensure_safe_target(scope, root, target_dir)
	if not ok then
		return false, safe_err
	end

	rm_rf(target_dir)
	mkdirp(vim.fn.fnamemodify(target_dir, ":h"):gsub("\\", "/"))

	local copied, copy_err = copy_tree(plugin_source_dir, target_dir)
	if not copied then
		return false, copy_err
	end

	return true, target_dir
end

local function install_nvr()
	if command_exists("nvr") then
		return true, "nvr already available"
	end

	local attempts = {}

	if command_exists("pipx") then
		table.insert(attempts, {
			cmd = { "pipx", "install", "neovim-remote" },
			label = "pipx install neovim-remote",
		})
	end

	if command_exists("py") then
		table.insert(attempts, {
			cmd = { "py", "-m", "pip", "install", "--user", "neovim-remote" },
			label = "py -m pip install --user neovim-remote",
		})
	end

	if command_exists("python") then
		table.insert(attempts, {
			cmd = { "python", "-m", "pip", "install", "--user", "neovim-remote" },
			label = "python -m pip install --user neovim-remote",
		})
	end

	if #attempts == 0 then
		return false, "no pipx/python launcher found for installing nvr"
	end

	local errors = {}
	for _, attempt in ipairs(attempts) do
		local result = run_system(attempt.cmd)
		if result.code == 0 then
			return true, attempt.label
		end

		local stderr = vim.trim((result.stderr or "") ~= "" and result.stderr or (result.stdout or ""))
		table.insert(errors, string.format("%s -> %s", attempt.label, stderr ~= "" and stderr or ("exit " .. tostring(result.code))))
	end

	return false, table.concat(errors, "\n")
end

function M.has_nvr()
	return command_exists("nvr")
end

function M.plugin_status(project_root)
	project_root = normalize(project_root)
		or project.find_project_root_from_context({
			registered_fallback = false,
		})

	if not project_root then
		return {
			ready = false,
			source_exists = repo_plugin_exists(),
			message = "not inside an Unreal project",
		}
	end

	local status = {
		source_exists = repo_plugin_exists(),
		project_root = project_root,
		project_path = project_plugin_target(project_root),
		project_installed = plugin_installed(project_plugin_target(project_root)),
		engine_path = nil,
		engine_installed = false,
		engine_error = nil,
	}

	local engine, err = project.engine_metadata(project_root)
	if engine and engine.engine_root then
		status.engine_path = engine_plugin_target(engine.engine_root)
		status.engine_installed = plugin_installed(status.engine_path)
	else
		status.engine_error = err
	end

	status.ready = status.project_installed or status.engine_installed
	if status.project_installed then
		status.scope = "project"
		status.path = status.project_path
		status.message = "installed in current project"
	elseif status.engine_installed then
		status.scope = "engine"
		status.path = status.engine_path
		status.message = "installed in current engine"
	else
		status.scope = "project"
		status.path = status.project_path
		status.message = status.source_exists and "plugin not installed yet" or "plugin source missing in UCore.nvim"
	end

	return status
end

function M.install_plugin(scope)
	return install_plugin(scope)
end

function M.install_nvr()
	return install_nvr()
end

local function notify_result(lines, level)
	vim.notify(table.concat(lines, "\n"), level or vim.log.levels.INFO, {
		title = "UCore install",
	})
end

local function split_args(tail)
	local items = {}
	for token in tostring(tail or ""):gmatch("%S+") do
		table.insert(items, token:lower())
	end
	return items
end

function M.run(tail)
	local args = split_args(tail)
	local mode = args[1] or "all"
	local scope = "project"

	if mode == "engine" or mode == "project" then
		scope = mode
		mode = "all"
	end

	for _, item in ipairs(args) do
		if item == "engine" then
			scope = "engine"
		elseif item == "project" then
			scope = "project"
		end
	end

	if mode == "help" then
		print([[
UCore install:
  :UCore install                 Install NvimSourceCodeAccess to current project and install nvr
  :UCore install plugin          Install NvimSourceCodeAccess to current project
  :UCore install plugin engine   Install NvimSourceCodeAccess to current Engine
  :UCore install nvr             Install neovim-remote (nvr)
  :UCore install help            Show this help
]])
		return
	end

	local lines = {}
	local overall_ok = true

	if mode == "all" or mode == "plugin" then
		local ok, result = install_plugin(scope)
		if ok then
			table.insert(lines, "Plugin installed: " .. tostring(result))
		else
			overall_ok = false
			table.insert(lines, "Plugin install failed: " .. tostring(result))
		end
	end

	if mode == "all" or mode == "nvr" then
		local ok, result = install_nvr()
		if ok then
			table.insert(lines, "nvr ready: " .. tostring(result))
		else
			overall_ok = false
			table.insert(lines, "nvr install failed: " .. tostring(result))
		end
	end

	if #lines == 0 then
		return notify_result({
			"Unknown install target: " .. tostring(mode),
			"Use :UCore install help",
		}, vim.log.levels.WARN)
	end

	notify_result(lines, overall_ok and vim.log.levels.INFO or vim.log.levels.WARN)
end

return M
