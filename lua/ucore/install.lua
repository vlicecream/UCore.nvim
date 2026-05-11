local project = require("ucore.project")
local status = require("ucore.status")

local M = {}
local split_args

local uv = vim.uv or vim.loop
local remote_repo_url = vim.env.UCORE_INSTALL_REPO or "https://github.com/vlicecream/UCore.nvim.git"

local ignored_tree_dirs = {
	Binaries = true,
	Intermediate = true,
	Saved = true,
	[".vs"] = true,
}

local plugin_specs = {
	nvimsourcecodeaccess = {
		id = "nvimsourcecodeaccess",
		display_name = "NvimSourceCodeAccess",
		description = "Unreal SourceCodeAccess provider for opening C++ in Neovim",
		folder_name = "NvimSourceCodeAccess",
		manifest_name = "NvimSourceCodeAccess.uplugin",
		repo_subdir = "unreal-plugins/NvimSourceCodeAccess",
		task_key = "task:plugin",
		aliases = {
			"nvimsourcecodeaccess",
			"sourcecode",
		},
	},
	neovimlink = {
		id = "neovimlink",
		display_name = "NeovimLink",
		description = "Open Unreal Blueprint and asset paths from UCore in the editor",
		folder_name = "NeovimLink",
		manifest_name = "NeovimLink.uplugin",
		repo_subdir = "unreal-plugins/NeovimLink",
		task_key = "task:asset_bridge",
		aliases = {
			"neovimlink",
			"assetlink",
		},
	},
}

local plugin_order = {
	"nvimsourcecodeaccess",
	"neovimlink",
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

local function copy_tree_async(src, dst, progress, callback)
	src = normalize(src)
	dst = normalize(dst)

	if not is_dir(src) then
		return callback(false, "source directory missing: " .. tostring(src))
	end

	mkdirp(dst)

	local files, total_bytes = collect_tree_files(src)
	local copied_bytes = 0
	local index = 0

	local function report()
		if type(progress) == "function" then
			progress({
				phase = "copy",
				current_bytes = copied_bytes,
				total_bytes = total_bytes,
			})
		end
	end

	report()

	local function step()
		index = index + 1
		if index > #files then
			return callback(true, dst)
		end

		local item = files[index]
		local relative = item.path:sub(#src + 2)
		local target = path_join(dst, relative)
		local ok, err = copy_file(item.path, target)
		if not ok then
			return callback(false, string.format("copy failed: %s -> %s (%s)", item.path, target, tostring(err)))
		end

		copied_bytes = copied_bytes + item.size
		report()
		vim.schedule(step)
	end

	vim.schedule(step)
end

local function run_system(cmd, opts)
	opts = opts or {}
	return vim.system(cmd, {
		text = true,
		cwd = opts.cwd,
		env = opts.env,
	}):wait()
end

local function run_system_async(cmd, opts, callback)
	opts = opts or {}
	vim.system(cmd, {
		text = true,
		cwd = opts.cwd,
		env = opts.env,
	}, function(result)
		vim.schedule(function()
			callback(result)
		end)
	end)
end

local function command_error(result, fallback)
	local parts = {}
	if result then
		if result.stdout and result.stdout ~= "" then
			table.insert(parts, vim.trim(result.stdout))
		end
		if result.stderr and result.stderr ~= "" then
			table.insert(parts, vim.trim(result.stderr))
		end
	end

	local message = table.concat(parts, "\n")
	if message == "" then
		return fallback
	end

	return message
end

local function plugin_spec(name)
	local key = tostring(name or ""):gsub("%s+", ""):lower()
	if key == "" then
		return nil
	end

	for _, spec_key in ipairs(plugin_order) do
		local spec = plugin_specs[spec_key]
		for _, alias in ipairs(spec.aliases or {}) do
			if alias == key then
				return spec
			end
		end
	end

	return nil
end

local function plugin_manifest_path(spec, dir)
	dir = normalize(dir)
	if not dir then
		return nil
	end

	return dir .. "/" .. spec.manifest_name
end

local function plugin_installed(spec, dir)
	return is_file(plugin_manifest_path(spec, dir))
end

local function plugin_target(spec, scope, root)
	if scope == "engine" then
		return path_join(root, "Engine", "Plugins", "Developer", spec.folder_name)
	end

	return path_join(root, "Plugins", "Developer", spec.folder_name)
end

local function display_target_path(scope, root, target)
	scope = scope == "engine" and "engine" or "project"
	root = normalize(root)
	target = normalize(target)
	if not target then
		return nil
	end

	if scope == "engine" then
		local prefix = normalize(path_join(root or "", "Engine"))
		if prefix and target:sub(1, #prefix) == prefix then
			local suffix = target:sub(#prefix + 1):gsub("^/+", "")
			return suffix ~= "" and ("Engine/" .. suffix) or "Engine"
		end
		return target
	end

	if root and target:sub(1, #root) == root then
		local suffix = target:sub(#root + 1):gsub("^/+", "")
		return suffix ~= "" and ("Project/" .. suffix) or "Project"
	end

	return target
end

local function ensure_safe_target(spec, scope, root, target)
	local expected = normalize(plugin_target(spec, scope, root))
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
		return project_root, project_root, nil
	end

	local engine, err = project.engine_metadata(project_root)
	if not engine or not engine.engine_root then
		return nil, nil, err or "could not resolve engine root"
	end

	return engine.engine_root, project_root, nil
end

local function temp_install_dir(spec)
	local base = normalize(vim.fn.stdpath("cache") .. "/ucore/install")
	mkdirp(base)
	return normalize(string.format("%s/%s-%d-%d", base, spec.id, os.time(), vim.fn.getpid()))
end

local function fetch_remote_plugin(spec, progress)
	local temp_dir = temp_install_dir(spec)
	local repo_dir = path_join(temp_dir, "repo")

	rm_rf(temp_dir)
	mkdirp(temp_dir)

	if type(progress) == "function" then
		progress({
			message = "Downloading " .. spec.display_name .. " from GitHub...",
		})
	end

	local clone_result = run_system({
		"git",
		"clone",
		"--depth",
		"1",
		"--filter=blob:none",
		"--sparse",
		remote_repo_url,
		repo_dir,
	})
	if clone_result.code ~= 0 then
		rm_rf(temp_dir)
		return false, command_error(clone_result, "git clone failed")
	end

	local sparse_result = run_system({
		"git",
		"-C",
		repo_dir,
		"sparse-checkout",
		"set",
		spec.repo_subdir or spec.folder_name,
	})
	if sparse_result.code ~= 0 then
		rm_rf(temp_dir)
		return false, command_error(sparse_result, "git sparse-checkout failed")
	end

	local source_dir = path_join(repo_dir, spec.repo_subdir or spec.folder_name)
	if not plugin_installed(spec, source_dir) then
		rm_rf(temp_dir)
		return false, spec.display_name .. " is missing from remote repository"
	end

	return true, {
		temp_dir = temp_dir,
		source_dir = source_dir,
	}
end

local function fetch_remote_plugin_async(spec, progress, callback)
	local temp_dir = temp_install_dir(spec)
	local repo_dir = path_join(temp_dir, "repo")

	rm_rf(temp_dir)
	mkdirp(temp_dir)

	if type(progress) == "function" then
		progress({
			phase = "download",
			percent = 5,
			text = string.format("%s 5%% Preparing download", spec.display_name),
		})
	end

	run_system_async({
		"git",
		"clone",
		"--depth",
		"1",
		"--filter=blob:none",
		"--sparse",
		remote_repo_url,
		repo_dir,
	}, nil, function(clone_result)
		if clone_result.code ~= 0 then
			rm_rf(temp_dir)
			return callback(false, command_error(clone_result, "git clone failed"))
		end

		if type(progress) == "function" then
			progress({
				phase = "download",
				percent = 55,
				text = string.format("%s 55%% Repository downloaded", spec.display_name),
			})
		end

		run_system_async({
			"git",
			"-C",
			repo_dir,
			"sparse-checkout",
			"set",
			spec.repo_subdir or spec.folder_name,
		}, nil, function(sparse_result)
			if sparse_result.code ~= 0 then
				rm_rf(temp_dir)
				return callback(false, command_error(sparse_result, "git sparse-checkout failed"))
			end

			local source_dir = path_join(repo_dir, spec.repo_subdir or spec.folder_name)
			if not plugin_installed(spec, source_dir) then
				rm_rf(temp_dir)
				return callback(false, spec.display_name .. " is missing from remote repository")
			end

			if type(progress) == "function" then
				progress({
					phase = "download",
					percent = 70,
					text = string.format("%s 70%% Package ready", spec.display_name),
				})
			end

			callback(true, {
				temp_dir = temp_dir,
				source_dir = source_dir,
			})
		end)
	end)
end

local function install_spec(spec, scope, progress)
	local root, _, err = resolve_install_root(scope)
	if not root then
		return false, err
	end

	local target_dir = plugin_target(spec, scope, root)
	local ok, safe_err = ensure_safe_target(spec, scope, root, target_dir)
	if not ok then
		return false, safe_err
	end

	local fetched, fetched_result = fetch_remote_plugin(spec, progress)
	if not fetched then
		return false, fetched_result
	end

	local payload = fetched_result
	local copied = false
	local copy_result

	rm_rf(target_dir)
	mkdirp(vim.fn.fnamemodify(target_dir, ":h"):gsub("\\", "/"))

	if type(progress) == "function" then
		progress({
			message = "Installing " .. spec.display_name .. "...",
		})
	end

	copied, copy_result = copy_tree(payload.source_dir, target_dir, progress)
	rm_rf(payload.temp_dir)

	if not copied then
		return false, copy_result
	end

	return true, target_dir
end

local function install_spec_async(spec, scope, progress, callback)
	local root, _, err = resolve_install_root(scope)
	if not root then
		return callback(false, err)
	end

	local target_dir = plugin_target(spec, scope, root)
	local ok, safe_err = ensure_safe_target(spec, scope, root, target_dir)
	if not ok then
		return callback(false, safe_err)
	end

	fetch_remote_plugin_async(spec, progress, function(fetched, fetched_result)
		if not fetched then
			return callback(false, fetched_result)
		end

		local payload = fetched_result
		rm_rf(target_dir)
		mkdirp(vim.fn.fnamemodify(target_dir, ":h"):gsub("\\", "/"))

		if type(progress) == "function" then
			progress({
				phase = "copy",
				current_bytes = 0,
				total_bytes = 0,
				text = string.format("%s 72%% Installing files", spec.display_name),
			})
		end

		copy_tree_async(payload.source_dir, target_dir, progress, function(copied, copy_result)
			rm_rf(payload.temp_dir)
			if not copied then
				return callback(false, copy_result)
			end
			callback(true, target_dir)
		end)
	end)
end

local function status_for_spec(spec, project_root)
	project_root = normalize(project_root)
		or project.find_project_root_from_context({
			registered_fallback = false,
		})

	if not project_root then
		return {
			ready = false,
			source_exists = true,
			message = "not inside an Unreal project",
		}
	end

	local item = {
		source_exists = true,
		project_root = project_root,
		project_path = plugin_target(spec, "project", project_root),
		project_installed = plugin_installed(spec, plugin_target(spec, "project", project_root)),
		engine_path = nil,
		engine_installed = false,
		engine_error = nil,
	}

	local engine, err = project.engine_metadata(project_root)
	if engine and engine.engine_root then
		item.engine_path = plugin_target(spec, "engine", engine.engine_root)
		item.engine_installed = plugin_installed(spec, item.engine_path)
	else
		item.engine_error = err
	end

	item.ready = item.project_installed or item.engine_installed
	if item.project_installed then
		item.scope = "project"
		item.path = item.project_path
		item.message = "installed in current project"
	elseif item.engine_installed then
		item.scope = "engine"
		item.path = item.engine_path
		item.message = "installed in current engine"
	else
		item.scope = "project"
		item.path = item.project_path
		item.message = "plugin not installed yet"
	end

	return item
end

function M.plugin_status(project_root)
	return status_for_spec(plugin_specs.nvimsourcecodeaccess, project_root)
end

function M.asset_link_status(project_root)
	return status_for_spec(plugin_specs.neovimlink, project_root)
end

function M.install_plugin(scope, progress)
	return install_spec(plugin_specs.nvimsourcecodeaccess, scope, progress)
end

function M.install_asset_link_plugin(scope, progress)
	return install_spec(plugin_specs.neovimlink, scope, progress)
end

function M.resolve_plugin(name)
	local spec = plugin_spec(name)
	if not spec then
		return nil
	end
	return vim.deepcopy(spec)
end

function M.progress_message(name, progress)
	local spec = type(name) == "table" and name or plugin_spec(name)
	local display_name = spec and spec.display_name or tostring(name or "Plugin")
	progress = progress or {}

	if progress.text and progress.text ~= "" then
		return tostring(progress.text)
	end

	if progress.phase == "copy" then
		local total = tonumber(progress.total_bytes or 0) or 0
		local current = tonumber(progress.current_bytes or 0) or 0
		local percent = total > 0 and math.floor(72 + ((current / total) * 23)) or 72
		return string.format("%s %d%% %s / %s", display_name, percent, format_mb(current), format_mb(total))
	end

	if progress.phase == "download" then
		local percent = tonumber(progress.percent or 0) or 0
		return string.format("%s %d%% Downloading", display_name, percent)
	end

	return display_name .. " Working..."
end

function M.completion_items(tail, arglead)
	local raw = tostring(tail or "")
	local args = split_args(vim.trim(raw))
	local lead = tostring(arglead or "")
	local first = args[1] and args[1]:lower() or ""
	local second = args[2] or ""

	if vim.trim(raw) == "" then
		return { "all", "plugin", "help" }
	end

	if #args == 1 and first ~= "plugin" and not raw:match("%s$") then
		return vim.tbl_filter(function(item)
			return item:lower():find(first, 1, true) == 1
		end, { "all", "plugin", "help" })
	end

	if first == "plugin" then
		local items = {}
		for _, key in ipairs(plugin_order) do
			local spec = plugin_specs[key]
			table.insert(items, spec.display_name)
		end
		if second == "" then
			return items
		end
		local needle = lead ~= "" and lead:lower() or second:lower()
		return vim.tbl_filter(function(item)
			return item:lower():find(needle, 1, true) == 1
		end, items)
	end

	return {}
end

function M.install_named(name, scope, progress)
	local spec = plugin_spec(name)
	if not spec then
		return false, "unknown plugin: " .. tostring(name)
	end
	return install_spec(spec, scope, progress)
end

function M.install_named_async(name, scope, progress, callback)
	local spec = plugin_spec(name)
	if not spec then
		return callback(false, "unknown plugin: " .. tostring(name))
	end
	return install_spec_async(spec, scope, progress, callback)
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

split_args = function(tail)
	local items = {}
	for token in tostring(tail or ""):gmatch("%S+") do
		table.insert(items, token)
	end
	return items
end

local function print_help()
	print([[
UCore install:
  :UCore install all                              Pick plugin, then choose Project / Engine
  :UCore install plugin NeovimLink                Install NeovimLink from GitHub
  :UCore install plugin NvimSourceCodeAccess      Install NvimSourceCodeAccess from GitHub
  :UCore install help                             Show this help
]])
end

local function prompt_scope(callback)
	local items = {
		{ id = "project", label = "Project" },
		{ id = "engine", label = "Engine" },
	}

	vim.ui.select(items, {
		prompt = "UCore install target",
		format_item = function(item)
			return item.label
		end,
	}, function(choice)
		callback(choice and choice.id or nil)
	end)
end

local function prompt_plugin(callback)
	local items = {}
	for _, key in ipairs(plugin_order) do
		table.insert(items, plugin_specs[key])
	end

	vim.ui.select(items, {
		prompt = "UCore install plugin",
		format_item = function(item)
			return item.display_name
		end,
	}, function(choice)
		callback(choice)
	end)
end

local function run_install(spec, scope)
	local function run_install_many(specs, install_scope)
		local install_root = resolve_install_root(install_scope)
		install_status_start("Installing Unreal editor integration...")
		local index = 0
		local all_ok = true
		local details = {}

		local function step()
			index = index + 1
			if index > #specs then
				return install_status_done(
					all_ok,
					all_ok and "UCore Install Complete" or "UCore Install Failed",
					table.concat(details, "\n")
				)
			end

			local current = specs[index]
			install_status_progress(current.task_key, "Preparing " .. current.display_name .. "...")
			install_spec_async(current, install_scope, function(progress)
				install_status_progress(current.task_key, M.progress_message(current, progress))
			end, function(ok, result)
				if ok then
					local shown = display_target_path(install_scope, install_root, result) or tostring(result)
					install_status_finish(current.task_key, current.display_name .. " installed: " .. shown)
					table.insert(details, current.display_name .. ": " .. shown)
				else
					install_status_finish(current.task_key, current.display_name .. " install failed")
					table.insert(details, current.display_name .. ": " .. tostring(result))
				end
				all_ok = all_ok and ok
				step()
			end)
		end

		step()
	end

	return run_install_many({ spec }, scope)
end

function M.run(tail)
	local args = split_args(tail)
	local command = args[1] and args[1]:lower() or ""

	if command == "help" then
		return print_help()
	end

	local requested_spec
	if command == "all" then
		requested_spec = nil
	elseif command == "plugin" then
		requested_spec = plugin_spec(args[2])
		if args[2] and not requested_spec then
			return notify_result({
				"Unknown plugin: " .. tostring(args[2]),
				"Use :UCore install help",
			}, vim.log.levels.WARN)
		end
	else
		return notify_result({
			"Unknown install command: " .. tostring(command == "" and "install" or command),
			"Use :UCore install help",
		}, vim.log.levels.WARN)
	end

	local function continue_with_spec(spec)
		if not spec then
			return
		end

		prompt_scope(function(scope)
			if not scope then
				return
			end

			run_install(spec, scope)
		end)
	end

	if command == "all" then
		local specs = {}
		for _, key in ipairs(plugin_order) do
			table.insert(specs, plugin_specs[key])
		end

		return prompt_scope(function(scope)
			if not scope then
				return
			end

			local install_root = resolve_install_root(scope)
			install_status_start("Installing Unreal editor integration...")
			local index = 0
			local all_ok = true
			local details = {}

			local function step()
				index = index + 1
				if index > #specs then
					return install_status_done(
						all_ok,
						all_ok and "UCore Install Complete" or "UCore Install Failed",
						table.concat(details, "\n")
					)
				end

				local spec = specs[index]
				install_status_progress(spec.task_key, "Preparing " .. spec.display_name .. "...")
				install_spec_async(spec, scope, function(progress)
					install_status_progress(spec.task_key, M.progress_message(spec, progress))
				end, function(ok, result)
					if ok then
						local shown = display_target_path(scope, install_root, result) or tostring(result)
						install_status_finish(spec.task_key, spec.display_name .. " installed: " .. shown)
						table.insert(details, spec.display_name .. ": " .. shown)
					else
						install_status_finish(spec.task_key, spec.display_name .. " install failed")
						table.insert(details, spec.display_name .. ": " .. tostring(result))
					end
					all_ok = all_ok and ok
					step()
				end)
			end

			step()
		end)
	end

	if requested_spec then
		return continue_with_spec(requested_spec)
	end

	return prompt_plugin(continue_with_spec)
end

return M
