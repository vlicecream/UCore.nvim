local project = require("ucore.project")
local status = require("ucore.status")

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

local function collect_tree_files(src, files, total_bytes)
	src = normalize(src)
	files = files or {}
	total_bytes = total_bytes or 0

	for _, item in ipairs(scandir(src)) do
		local from = path_join(src, item.name)
		if item.kind == "directory" then
			files, total_bytes = collect_tree_files(from, files, total_bytes)
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

local nvr_install_attempts

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

local function install_nvr()
	if command_exists("nvr") then
		return true, "nvr already available"
	end

	local attempts = nvr_install_attempts()

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

local function unit_to_mb(value, unit)
	value = tonumber(value or 0) or 0
	unit = tostring(unit or "MB"):lower()
	if unit == "gb" then
		return value * 1024
	end
	if unit == "kb" then
		return value / 1024
	end
	if unit == "b" then
		return value / 1024 / 1024
	end
	return value
end

local function parse_download_progress(line, state)
	line = tostring(line or ""):gsub("\r", "")
	if line == "" then
		return false
	end

	local total_value, total_unit = line:match("%(([%d%.]+)%s*([kKmMgGbB][bB]?)%)")
	if total_value and total_unit and line:lower():find("download", 1, true) then
		state.total_mb = unit_to_mb(total_value, total_unit)
		state.current_mb = state.current_mb or 0
		return true
	end

	local current_value, progress_total_value, progress_unit = line:match("([%d%.]+)%s*/%s*([%d%.]+)%s*([kKmMgGbB][bB]?)")
	if current_value and progress_total_value and progress_unit then
		state.current_mb = unit_to_mb(current_value, progress_unit)
		state.total_mb = unit_to_mb(progress_total_value, progress_unit)
		return true
	end

	return false
end

nvr_install_attempts = function()
	local attempts = {}

	if command_exists("pipx") then
		table.insert(attempts, {
			cmd = { "pipx", "install", "neovim-remote" },
			label = "pipx install neovim-remote",
		})
	end

	if command_exists("py") then
		table.insert(attempts, {
			cmd = { "py", "-m", "pip", "install", "--progress-bar", "on", "--user", "neovim-remote" },
			label = "py -m pip install --user neovim-remote",
		})
	end

	if command_exists("python") then
		table.insert(attempts, {
			cmd = { "python", "-m", "pip", "install", "--progress-bar", "on", "--user", "neovim-remote" },
			label = "python -m pip install --user neovim-remote",
		})
	end

	return attempts
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

function M.install_plugin(scope, progress)
	return install_plugin(scope, progress)
end

function M.install_nvr()
	return install_nvr()
end

function M.install_nvr_async(callback, opts)
	callback = callback or function() end
	opts = opts or {}

	if command_exists("nvr") then
		return callback(true, "nvr already available")
	end

	local attempts = nvr_install_attempts()
	if #attempts == 0 then
		return callback(false, "no pipx/python launcher found for installing nvr")
	end

	local errors = {}
	local index = 0

	local function run_next()
		index = index + 1
		local attempt = attempts[index]
		if not attempt then
			return callback(false, table.concat(errors, "\n"))
		end

		local output = {}
		local state = {
			current_mb = 0,
			total_mb = nil,
		}

		local function handle_line(line)
			line = tostring(line or ""):gsub("\r", "")
			if line == "" then
				return
			end

			table.insert(output, line)
			if parse_download_progress(line, state) and type(opts.on_progress) == "function" then
				opts.on_progress({
					attempt = attempt.label,
					current_mb = state.current_mb or 0,
					total_mb = state.total_mb,
					line = line,
				})
			elseif type(opts.on_output) == "function" then
				opts.on_output({
					attempt = attempt.label,
					line = line,
				})
			end
		end

		local job_id = vim.fn.jobstart(attempt.cmd, {
			stdout_buffered = false,
			stderr_buffered = false,
			on_stdout = function(_, data)
				for _, line in ipairs(data or {}) do
					handle_line(line)
				end
			end,
			on_stderr = function(_, data)
				for _, line in ipairs(data or {}) do
					handle_line(line)
				end
			end,
			on_exit = function(_, code)
				vim.schedule(function()
					if code == 0 then
						return callback(true, attempt.label)
					end

					local merged = vim.trim(table.concat(output, "\n"))
					table.insert(
						errors,
						string.format("%s -> %s", attempt.label, merged ~= "" and merged or ("exit " .. tostring(code)))
					)
					run_next()
				end)
			end,
		})

		if job_id <= 0 then
			table.insert(errors, string.format("%s -> failed to start process", attempt.label))
			run_next()
		end
	end

	run_next()
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
  :UCore install                 Install NvimSourceCodeAccess to current project and install nvr
  :UCore install plugin          Install NvimSourceCodeAccess to current project
  :UCore install plugin engine   Install NvimSourceCodeAccess to current Engine
  :UCore install nvr             Install neovim-remote (nvr)
  :UCore install help            Show this help
]])
		return
	end

	local overall_ok = true
	local handled = false
	local failure_messages = {}

	if mode == "all" or mode == "plugin" then
		handled = true
		install_status_start("Installing Unreal editor integration...")
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
			overall_ok = false
			table.insert(failure_messages, tostring(result))
			install_status_finish("task:plugin", "Plugin install failed: " .. tostring(result))
		end
		if mode == "plugin" then
			return install_status_done(ok, ok and "UCore Install Complete" or "UCore Install Failed", tostring(result))
		end
	end

	if mode == "all" or mode == "nvr" then
		handled = true
		if mode == "nvr" then
			install_status_start("Installing Unreal editor integration...")
		end
		install_status_progress("task:nvr", "nvr install preparing...")
		return M.install_nvr_async(function(ok, result)
			if ok then
				install_status_finish("task:nvr", "nvr ready: " .. tostring(result))
			else
				overall_ok = false
				table.insert(failure_messages, tostring(result))
				install_status_finish("task:nvr", "nvr install failed: " .. tostring(result))
			end
			install_status_done(
				overall_ok,
				overall_ok and "UCore Install Complete" or "UCore Install Failed",
				#failure_messages > 0 and table.concat(failure_messages, " | ") or nil
			)
		end, {
			on_progress = function(progress)
				if progress.total_mb then
					install_status_progress("task:nvr", string.format(
						"nvr download %.1f MB / %.1f MB",
						progress.current_mb or 0,
						progress.total_mb
					))
				else
					install_status_progress("task:nvr", string.format("nvr download %.1f MB / ...", progress.current_mb or 0))
				end
			end,
			on_output = function(progress)
				if tostring(progress.line or "") ~= "" then
					install_status_progress("task:nvr", "nvr install running...")
				end
			end,
		})
	end

	if not handled then
		return notify_result({
			"Unknown install target: " .. tostring(mode),
			"Use :UCore install help",
		}, vim.log.levels.WARN)
	end
end

return M
