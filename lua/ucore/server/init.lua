local config = require("ucore.config")
local project = require("ucore.project")

local M = {}

local job = nil
local log_file = nil

-- Check whether the managed server job is still running.
-- 检查当前由 nvim 管理的 server job 是否还在运行。
function M.is_running()
	return job ~= nil
end

-- Return the latest server log path known to this session.
-- 返回当前会话已知的 server 日志路径。
function M.log_path()
	return log_file
end

-- Build the server command.
-- 构造 server 启动命令。
local function build_cmd(port, registry)
	local cmd = vim.deepcopy(config.values.server_cmd)
	table.insert(cmd, tostring(port))
	table.insert(cmd, registry)
	return cmd
end

-- Append server output from vim.system callbacks safely.
-- 安全地追加写入 vim.system 回调里的 server 输出。
local function append_log(data)
	if not data or data == "" then
		return
	end

	vim.schedule(function()
		if log_file then
			vim.fn.writefile(vim.split(data, "\n"), log_file, "a")
		end
	end)
end

-- Start the Rust server as a background job.
-- 以后台 job 的方式启动 Rust server。
function M.start(callback, opts)
	callback = callback or function() end
	opts = opts or {}

	if M.is_running() then
		return callback(true, "Server already running")
	end

	local root = opts.project_root or project.find_project_root_from_context({
		registered_fallback = false,
	})
	if not root then
		return callback(false, "Could not find .uproject")
	end

	local paths = project.build_paths(root)
	local cmd = build_cmd(config.values.port, paths.registry_path)

	log_file = paths.log_path
	vim.fn.writefile({
		"UCore server log",
		"Started at: " .. os.date("%Y-%m-%d %H:%M:%S"),
		"Command: " .. table.concat(vim.tbl_map(tostring, cmd), " "),
		"",
	}, log_file, "a")

	job = vim.system(cmd, {
		cwd = config.values.scanner_dir,
		text = true,
		stdout = function(_, data)
			append_log(data)
		end,
		stderr = function(_, data)
			append_log(data)
		end,
	}, function(result)
		job = nil

		if result.code ~= 0 then
			vim.schedule(function()
				vim.notify("UCore server exited: " .. tostring(result.code), vim.log.levels.WARN)
			end)
		end
	end)

	callback(true, "Server starting")
end

-- Stop the managed Rust server job.
-- 停止由 nvim 管理的 Rust server job。
function M.stop(callback)
	callback = callback or function() end

	if not job then
		return callback(true, "Server is not managed by this nvim session")
	end

	job:kill(15)
	job = nil

	callback(true, "Server stopped")
end

-- Restart the Rust server.
-- 重启 Rust server。
function M.restart(callback)
	M.stop(function()
		M.start(callback)
	end)
end

return M
