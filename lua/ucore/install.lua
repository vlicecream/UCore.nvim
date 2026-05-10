local project = require("ucore.project")
local status = require("ucore.status")

local M = {}

local uv = vim.uv or vim.loop

local source = debug.getinfo(1, "S").source:sub(2)
local repo_root = vim.fn.fnamemodify(source, ":p:h:h:h"):gsub("\\", "/")
local plugin_source_dir = repo_root .. "/NvimSourceCodeAccess"
local asset_link_source_dir = repo_root .. "/NeovimLink"
local ignored_tree_dirs = {
	Binaries = true,
	Intermediate = true,
	Saved = true,
	[".vs"] = true,
}

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

local function collect_tree_files(src, files, total_bytes)
	src = normalize(src)
	files = files or {}
	total_bytes = total_bytes or 0

	for _, item in ipairs(scandir(src)) do
		local from = path_join(src, item.name)
		if item.kind == "directory" then
			if not ignored_tree_dirs[item.name] then
				files, total_bytes = collect_tree_files(from, files, total_bytes)
			end
		else
			local info = stat(from) or {}
			local size = tonumber(info.size or 0) or 0
			table.insert(files, {
				path = from,
				size = size,
			})
			total_bytes = total_bytes + size
		end
	end

	return files, total_bytes
end

local function format_mb(bytes)
	return string.format("%.1f MB", (tonumber(bytes or 0) or 0) / 1024 / 1024)
end

local function copy_tree(src, dst, progress)
	src = normalize(src)
	dst = normalize(dst)

	if not is_dir(src) then
		return false, "source directory missing: " .. tostring(src)
	end

	mkdirp(dst)

	local files, total_bytes = collect_tree_files(src)
	local copied_bytes = 0

	if type(progress) == "function" then
		progress({
			current_bytes = 0,
			total_bytes = total_bytes,
			current_file = nil,
		})
	end

	for _, item in ipairs(files) do
		local relative = item.path:sub(#src + 2)
		local target = path_join(dst, relative)
		local ok, err = copy_file(item.path, target)
		if not ok then
			return false, string.format("copy failed: %s -> %s (%s)", item.path, target, tostring(err))
		end

		copied_bytes = copied_bytes + item.size
		if type(progress) == "function" then
			progress({
				current_bytes = copied_bytes,
				total_bytes = total_bytes,
				current_file = target,
			})
		end
	end

	return true, dst
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

local function repo_plugin_exists()
	return is_dir(plugin_source_dir) and is_file(plugin_source_dir .. "/NvimSourceCodeAccess.uplugin")
end

local function repo_asset_link_exists()
	return is_dir(asset_link_source_dir) and is_file(asset_link_source_dir .. "/NeovimLink.uplugin")
end

local function plugin_manifest_path(dir)
	dir = normalize(dir)
	if not dir then
		return nil
	end

	return dir .. "/NvimSourceCodeAccess.uplugin"
end

local function asset_link_manifest_path(dir)
	dir = normalize(dir)
	if not dir then
		return nil
	end

	return dir .. "/NeovimLink.uplugin"
end

local function plugin_installed(dir)
	return is_file(plugin_manifest_path(dir))
end

local function asset_link_installed(dir)
	return is_file(asset_link_manifest_path(dir))
end

local function project_plugin_target(project_root)
	return path_join(project_root, "Plugins", "Developer", "NvimSourceCodeAccess")
end

local function project_asset_link_target(project_root)
	return path_join(project_root, "Plugins", "Developer", "NeovimLink")
end

local function engine_plugin_target(engine_root)
	return path_join(engine_root, "Engine", "Plugins", "Developer", "NvimSourceCodeAccess")
end

local function engine_asset_link_target(engine_root)
	return path_join(engine_root, "Engine", "Plugins", "Developer", "NeovimLink")
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

local function ensure_safe_asset_link_target(scope, root, target)
	local expected
	if scope == "engine" then
		expected = engine_asset_link_target(root)
	else
		expected = project_asset_link_target(root)
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

local function install_plugin(scope, progress)
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

	local copied, copy_err = copy_tree(plugin_source_dir, target_dir, progress)
	if not copied then
		return false, copy_err
	end

	return true, target_dir
end

local function install_asset_link_plugin(scope, progress)
	if not repo_asset_link_exists() then
		return false, "NeovimLink source is missing under UCore.nvim"
	end

	local root, _, err = resolve_install_root(scope)
	if not root then
		return false, err
	end

	local target_dir = scope == "engine" and engine_asset_link_target(root) or project_asset_link_target(root)
	local ok, safe_err = ensure_safe_asset_link_target(scope, root, target_dir)
	if not ok then
		return false, safe_err
	end

	rm_rf(target_dir)
	mkdirp(vim.fn.fnamemodify(target_dir, ":h"):gsub("\\", "/"))

	local copied, copy_err = copy_tree(asset_link_source_dir, target_dir, progress)
	if not copied then
		return false, copy_err
	end

	return true, target_dir
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

function M.asset_link_status(project_root)
	project_root = normalize(project_root)
		or project.find_project_root_from_context({
			registered_fallback = false,
		})

	if not project_root then
		return {
			ready = false,
			source_exists = repo_asset_link_exists(),
			message = "not inside an Unreal project",
		}
	end

	local status = {
		source_exists = repo_asset_link_exists(),
		project_root = project_root,
		project_path = project_asset_link_target(project_root),
		project_installed = asset_link_installed(project_asset_link_target(project_root)),
		engine_path = nil,
		engine_installed = false,
		engine_error = nil,
	}

	local engine, err = project.engine_metadata(project_root)
	if engine and engine.engine_root then
		status.engine_path = engine_asset_link_target(engine.engine_root)
		status.engine_installed = asset_link_installed(status.engine_path)
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

function M.install_plugin(scope, progress)
	return install_plugin(scope, progress)
end

function M.install_asset_link_plugin(scope, progress)
	return install_asset_link_plugin(scope, progress)
end

local function notify_result(lines, level)
	vim.notify(table.concat(lines, "\n"), level or vim.log.levels.INFO, {
		title = "UCore install",
	})
end

local function install_status_start(message)
	status.unreal_start(message or "Installing Unreal editor integration...")
end

local function install_status_progress(key, message)
	status.unreal_step(key, message)
end

local function install_status_finish(key, message)
	status.unreal_step_finish(key, message)
end

local function install_status_done(ok, message, detail)
	if ok then
		status.unreal_finish(message or "UCore Install Complete")
	else
		status.unreal_fail(message or "UCore Install Failed", detail)
	end
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
  :UCore install                      Install both Unreal integration plugins to current project
  :UCore install plugin               Install NvimSourceCodeAccess to current project
  :UCore install plugin engine        Install NvimSourceCodeAccess to current Engine
  :UCore install assetlink            Install NeovimLink to current project
  :UCore install assetlink engine     Install NeovimLink to current Engine
  :UCore install help                 Show this help
]])
		return
	end

	local handled = false
	local overall_ok = true
	local overall_detail = {}

	if mode == "all" or mode == "plugin" or mode == "assetlink" or mode == "asset" or mode == "neovimlink" then
		install_status_start("Installing Unreal editor integration...")
	end

	if mode == "all" or mode == "plugin" then
		handled = true
		install_status_progress("task:plugin", "Plugin install 0.0 MB / 0.0 MB")
		local ok, result = install_plugin(scope, function(progress)
			install_status_progress("task:plugin", string.format(
				"Plugin install %s / %s",
				format_mb(progress.current_bytes),
				format_mb(progress.total_bytes)
			))
		end)
		if ok then
			install_status_finish("task:plugin", "Plugin installed: " .. tostring(result))
		else
			install_status_finish("task:plugin", "Plugin install failed: " .. tostring(result))
		end
		overall_ok = overall_ok and ok
		table.insert(overall_detail, tostring(result))
		if mode == "plugin" then
			return install_status_done(ok, ok and "UCore Install Complete" or "UCore Install Failed", tostring(result))
		end
	end

	if mode == "all" or mode == "assetlink" or mode == "asset" or mode == "neovimlink" then
		handled = true
		install_status_progress("task:asset_bridge", "Asset bridge install 0.0 MB / 0.0 MB")
		local ok, result = install_asset_link_plugin(scope, function(progress)
			install_status_progress("task:asset_bridge", string.format(
				"Asset bridge install %s / %s",
				format_mb(progress.current_bytes),
				format_mb(progress.total_bytes)
			))
		end)
		if ok then
			install_status_finish("task:asset_bridge", "NeovimLink installed: " .. tostring(result))
		else
			install_status_finish("task:asset_bridge", "NeovimLink install failed: " .. tostring(result))
		end
		overall_ok = overall_ok and ok
		table.insert(overall_detail, tostring(result))
		if mode ~= "all" then
			return install_status_done(ok, ok and "UCore Install Complete" or "UCore Install Failed", tostring(result))
		end
	end

	if not handled then
		return notify_result({
			"Unknown install target: " .. tostring(mode),
			"Use :UCore install help",
		}, vim.log.levels.WARN)
	end

	return install_status_done(
		overall_ok,
		overall_ok and "UCore Install Complete" or "UCore Install Failed",
		table.concat(overall_detail, "\n")
	)
end

return M
