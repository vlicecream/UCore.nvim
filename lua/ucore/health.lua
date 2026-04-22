local config = require("ucore.config")
local project = require("ucore.project")

local M = {}

-- Compatibility layer for Neovim health API name changes.
-- 兼容不同 Neovim 版本的 health API 命名。
local health = vim.health or {}

local start = health.start or health.report_start
local ok = health.ok or health.report_ok
local warn = health.warn or health.report_warn
local error = health.error or health.report_error
local info = health.info or health.report_info

-- Check whether an executable exists on PATH.
-- 检查某个可执行文件是否存在于 PATH。
local function executable(name)
	return vim.fn.executable(name) == 1
end

-- Check whether a file exists and is readable.
-- 检查文件是否存在且可读。
local function readable(path)
	return path and vim.fn.filereadable(path) == 1
end

-- Check whether a directory exists.
-- 检查目录是否存在。
local function is_dir(path)
	return path and vim.fn.isdirectory(path) == 1
end

-- Return the first command executable from a command array.
-- 从命令数组中取出第一个可执行命令。
local function command_head(cmd)
	if type(cmd) ~= "table" then
		return nil
	end

	return cmd[1]
end

-- Build paths for release binaries.
-- 构造 release binary 路径。
local function release_binary(name)
	local suffix = vim.loop.os_uname().version:match("Windows") and ".exe" or ""
	return config.values.scanner_dir .. "/target/release/" .. name .. suffix
end

-- Run a synchronous TCP check against the configured server port.
-- 对配置的 server 端口做一次同步 TCP 检查。
local function tcp_check()
	local port = tostring(config.values.port)

	if vim.fn.has("win32") == 1 then
		local shell = executable("pwsh") and "pwsh" or "powershell"
		local script = table.concat({
			"$client = [Net.Sockets.TcpClient]::new();",
			"try {",
			"  $async = $client.BeginConnect('127.0.0.1', " .. port .. ", $null, $null);",
			"  if (-not $async.AsyncWaitHandle.WaitOne(1000, $false)) { exit 1 }",
			"  $client.EndConnect($async);",
			"  exit 0",
			"} catch {",
			"  exit 1",
			"} finally {",
			"  $client.Close()",
			"}",
		}, " ")

		local result = vim.system({ shell, "-NoProfile", "-Command", script }, { text = true }):wait()
		return result.code == 0, result.stderr ~= "" and result.stderr or result.stdout
	end

	local result = vim.system({
		"sh",
		"-c",
		"timeout 1 bash -c '</dev/tcp/127.0.0.1/" .. port .. "'",
	}, { text = true }):wait()

	return result.code == 0, result.stderr ~= "" and result.stderr or result.stdout
end

-- Report static installation and project checks.
-- 输出静态安装状态和工程状态检查。
local function report_static_checks()
	start("UCore.nvim")

	ok("Plugin health module loaded")

	if is_dir(config.values.scanner_dir) then
		ok("u-scanner directory found: " .. config.values.scanner_dir)
	else
		error("u-scanner directory not found: " .. tostring(config.values.scanner_dir))
	end

	local cargo_toml = config.values.scanner_dir .. "/Cargo.toml"
	if readable(cargo_toml) then
		ok("u-scanner Cargo.toml found")
	else
		error("u-scanner Cargo.toml not found: " .. cargo_toml)
	end

	if executable("cargo") then
		ok("cargo found on PATH")
	else
		warn("cargo not found on PATH", {
			"Install Rust from https://rustup.rs/",
			"Or configure UCore to use prebuilt u_scanner/u_core_server binaries.",
		})
	end

	local scanner_head = command_head(config.values.scanner_cmd)
	if scanner_head and (executable(scanner_head) or readable(scanner_head)) then
		ok("scanner command available: " .. scanner_head)
	else
		warn("scanner command may not be directly executable: " .. tostring(scanner_head))
	end

	local server_head = command_head(config.values.server_cmd)
	if server_head and (executable(server_head) or readable(server_head)) then
		ok("server command available: " .. server_head)
	else
		warn("server command may not be directly executable: " .. tostring(server_head))
	end

	local release_scanner = release_binary("u_scanner")
	local release_server = release_binary("u_core_server")

	if readable(release_scanner) then
		ok("release u_scanner found: " .. release_scanner)
	else
		info("release u_scanner not found; development mode can use cargo run")
	end

	if readable(release_server) then
		ok("release u_core_server found: " .. release_server)
	else
		info("release u_core_server not found; development mode can use cargo run")
	end
end

-- Report current Unreal project checks.
-- 输出当前 Unreal 工程相关检查。
local function report_project_checks()
	start("Current Unreal project")

	local buffer_path = vim.api.nvim_buf_get_name(0)
	if buffer_path == "" then
		warn("Current buffer has no file path")
		return
	end

	info("Current buffer: " .. buffer_path)

	local project_file = project.find_project_file(buffer_path)
	if not project_file then
		warn("No .uproject found upward from current buffer", {
			"Open a file inside an Unreal project.",
			"Then run :UCore or :checkhealth ucore again.",
		})
		return
	end

	ok(".uproject found: " .. project_file)

	local project_root = project.find_project_root(buffer_path)
	if project_root then
		ok("project root: " .. project_root)
	else
		error("failed to derive project root from .uproject")
		return
	end

	local paths = project.build_paths(project_root)

	if readable(paths.db_path) then
		ok("database found: " .. paths.db_path)
	else
		warn("database not found: " .. paths.db_path, {
			"Run :UCore to boot and refresh the project.",
		})
	end

	if readable(paths.cache_db_path) then
		ok("cache database found: " .. paths.cache_db_path)
	else
		info("cache database not found yet: " .. paths.cache_db_path)
	end
end

-- Report server reachability.
-- 输出 server 可达性检查。
local function report_server_checks()
	start("Rust server")

	info("configured port: " .. tostring(config.values.port))

	local reachable, err = tcp_check()
	if reachable then
		ok("server accepts TCP connections on 127.0.0.1:" .. tostring(config.values.port))
	else
		warn("server is not reachable: " .. tostring(err), {
			"Run :UCore to boot the server.",
			"For debugging, run :UCore debug start and :UCore debug rpc-status.",
		})
	end
end

-- Neovim calls this function for :checkhealth ucore.
-- Neovim 会在 :checkhealth ucore 时调用这个函数。
function M.check()
	report_static_checks()
	report_project_checks()
	report_server_checks()
end

return M
