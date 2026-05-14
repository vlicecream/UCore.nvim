local config = require("ucore.config")
local rpc = require("ucore.client.rpc")
local project = require("ucore.project")

local M = {}

local job = nil
local log_file = nil
local expected_exit = false

-- Force-kill a process tree on Windows when graceful termination is unreliable.
-- Windows 上优雅终止不稳定时，强制结束整棵进程树。
local function kill_process_tree(pid)
	if not pid or vim.fn.has("win32") ~= 1 then
		return
	end

	local shell = vim.fn.executable("pwsh") == 1 and "pwsh" or "powershell"
	vim.system({
		shell,
		"-NoProfile",
		"-Command",
		string.format("taskkill /PID %d /T /F *> $null", pid),
	}, { text = true })
end

local function find_windows_listener(port, callback)
	local shell = vim.fn.executable("pwsh") == 1 and "pwsh" or "powershell"
	local command = table.concat({
		"$port = " .. tostring(port),
		"$conn = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1",
		"if (-not $conn) {",
		"  $line = netstat -ano -p tcp | Select-String (':'+$port+'\\s+.*LISTENING\\s+(\\d+)$') | Select-Object -First 1",
		"  if ($line) {",
		"    $match = [regex]::Match($line.Line, '(\\d+)$')",
		"    if ($match.Success) {",
		"      $pid = [int]$match.Groups[1].Value",
		"      $proc = Get-CimInstance Win32_Process -Filter ('ProcessId = ' + $pid) -ErrorAction SilentlyContinue",
		"      if ($proc) { $proc | Select-Object ProcessId, Name, CommandLine | ConvertTo-Json -Compress; exit 0 }",
		"    }",
		"  }",
		"  exit 0",
		"}",
		"$pid = [int]$conn.OwningProcess",
		"$proc = Get-CimInstance Win32_Process -Filter ('ProcessId = ' + $pid) -ErrorAction SilentlyContinue",
		"if ($proc) { $proc | Select-Object ProcessId, Name, CommandLine | ConvertTo-Json -Compress }",
	}, " ")

	vim.system({
		shell,
		"-NoProfile",
		"-Command",
		command,
	}, { text = true }, function(result)
		vim.schedule(function()
			if result.code ~= 0 then
				return callback(nil, result.stderr ~= "" and result.stderr or result.stdout)
			end

			local stdout = vim.trim(result.stdout or "")
			if stdout == "" then
				return callback(nil, nil)
			end

			local ok, decoded = pcall(vim.json.decode, stdout)
			if not ok or type(decoded) ~= "table" then
				return callback(nil, "Failed to decode listener info: " .. stdout)
			end

			callback(decoded, nil)
		end)
	end)
end

local function find_listener(port, callback)
	if vim.fn.has("win32") == 1 then
		return find_windows_listener(port, callback)
	end

	callback(nil, "Finding external UCore server is only implemented on Windows")
end

local function is_ucore_server_process(proc)
	if type(proc) ~= "table" then
		return false
	end

	local name = tostring(proc.Name or proc.name or ""):lower()
	local command_line = tostring(proc.CommandLine or proc.commandline or ""):lower()
	return name:find("u_core_server", 1, true) ~= nil or command_line:find("u_core_server", 1, true) ~= nil
end

local function kill_port_owner(port, callback)
	find_listener(port, function(proc, err)
		if err then
			return callback(false, err)
		end

		if not proc then
			return callback(true, "No server is listening on the UCore port")
		end

		local pid = tonumber(proc.ProcessId or proc.processid)
		if not pid then
			return callback(false, "Could not resolve the listening process id")
		end

		if job and job.pid == pid then
			return M.stop(function(ok, stop_message)
				if not ok then
					return callback(false, stop_message)
				end

				vim.defer_fn(function()
					callback(true, stop_message)
				end, 500)
			end)
		end

		if not is_ucore_server_process(proc) then
			return callback(
				false,
				string.format(
					"Port %d is occupied by a non-UCore process: %s",
					port,
					tostring(proc.Name or proc.name or pid)
				)
			)
		end

		kill_process_tree(pid)
		vim.defer_fn(function()
			callback(true, string.format("Stopped external UCore server pid %d", pid))
		end, 500)
	end)
end

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
	if #cmd == 0 then
		config.refresh_backend_commands()
		cmd = vim.deepcopy(config.values.server_cmd)
	end

	if #cmd == 0 then
		return nil, "UScanner server command is not available"
	end

	table.insert(cmd, tostring(port))
	table.insert(cmd, registry)
	return cmd
end

local function build_server_env()
	local env = vim.fn.environ()
	local progress_config = config.values.progress or {}
	env.UCORE_FAST_FIND_LOG = progress_config.log == true and "1" or "0"
	env.UCORE_QUERY_LOG = progress_config.log == true and "1" or "0"
	return env
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

	config.refresh_backend_commands()

	local root = opts.project_root or project.find_project_root_from_context({
		registered_fallback = false,
	})
	if not root then
		return callback(false, "Could not find .uproject")
	end

	local paths = project.build_paths(root)
	local cmd, build_err = build_cmd(config.values.port, paths.registry_path)
	if not cmd then
		return callback(false, build_err)
	end

	log_file = paths.log_path
	vim.fn.writefile({
		"UCore server log",
		"Started at: " .. os.date("%Y-%m-%d %H:%M:%S"),
		"Command: " .. table.concat(vim.tbl_map(tostring, cmd), " "),
		"",
	}, log_file, "a")

	job = vim.system(cmd, {
		cwd = config.values.backend_cwd,
		env = build_server_env(),
		text = true,
		stdout = function(_, data)
			append_log(data)
		end,
		stderr = function(_, data)
			append_log(data)
		end,
	}, function(result)
		local was_expected = expected_exit
		job = nil
		expected_exit = false

		-- Allow auto_boot to re-trigger (e.g. after lazy sync rebuild kills server).
		-- 允许 auto_boot 重新触发（如 lazy sync 重构杀掉了 server）。
		vim.schedule(function()
			pcall(function()
				require("ucore.autocmd").reset()
			end)
		end)

		if result.code ~= 0 and not was_expected then
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

	expected_exit = true
	local pid = job.pid
	job:kill(15)
	vim.defer_fn(function()
		if job and job.pid == pid then
			kill_process_tree(pid)
		end
	end, 300)
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

-- Replace whatever server is currently listening with the latest local build.
-- 用当前本地最新构建替换正在监听端口的 server。
function M.replace(callback, opts)
	callback = callback or function() end
	opts = opts or {}

	local root = opts.project_root or project.find_project_root_from_context({
		registered_fallback = false,
	})
	if not root then
		return callback(false, "Could not find .uproject")
	end

	rpc.close()

	local function start_latest()
		kill_port_owner(config.values.port, function(ok, message)
			if not ok then
				return callback(false, message)
			end

			M.start(callback, {
				project_root = root,
			})
		end)
	end

	if M.is_running() then
		return M.stop(function(ok, message)
			if not ok then
				return callback(false, message)
			end

			start_latest()
		end)
	end

	start_latest()
end

return M
