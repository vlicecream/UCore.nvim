local config = require("ucore.config")
local project = require("ucore.project")
local server = require("ucore.server")

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

local function has_module(name)
	return pcall(require, name)
end

local function yes_no(value)
	return value and "yes" or "no"
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

local function log_path_for_current_project()
	local session_log = server.log_path()
	if session_log and session_log ~= "" then
		return session_log
	end

	local root = project.find_project_root()
	if root then
		return project.build_paths(root).log_path
	end

	return nil
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

	info("backend mode: " .. tostring(config.values.backend_mode))
	info("cache dir: " .. tostring(config.values.cache_dir))
	if is_dir(config.values.cache_dir) then
		ok("cache dir exists")
	else
		info("cache dir does not exist yet")
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
	local registry = project.read_registry()
	local registered = registry.projects and registry.projects[project_root]

	if type(registered) == "table" then
		ok("project registered in UCore registry")
	else
		warn("project is not registered in UCore registry", {
			"Run :UCore once inside the project.",
		})
	end

	info("project registry: " .. project.global_registry_path())
	info("server registry: " .. project.server_registry_path())

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

	local engine, engine_err = project.engine_metadata(project_root)
	if not engine then
		error("failed to resolve Unreal Engine root: " .. tostring(engine_err))
		return
	end

	ok("EngineAssociation resolved: " .. tostring(engine.engine_association or ""))
	ok("engine root: " .. tostring(engine.engine_root))
	info("engine id: " .. tostring(engine.engine_id))

	local engine_paths = project.build_engine_paths(engine)
	if project.engine_needs_refresh(engine) then
		warn("engine index needs refresh", {
			"Run :UCore inside this project and wait for engine indexing to finish.",
		})
	else
		ok("engine index metadata is current")
	end

	if readable(engine_paths.db_path) then
		ok("engine database found: " .. engine_paths.db_path)
	else
		warn("engine database not found: " .. engine_paths.db_path)
	end

	if readable(engine_paths.cache_db_path) then
		ok("engine cache database found: " .. engine_paths.cache_db_path)
	else
		info("engine cache database not found yet: " .. engine_paths.cache_db_path)
	end
end

-- Report server reachability.
-- 输出 server 可达性检查。
local function report_server_checks()
	start("Rust server")

	info("configured port: " .. tostring(config.values.port))
	info("managed by this nvim: " .. yes_no(server.is_running()))

	local log_path = log_path_for_current_project()
	if log_path then
		if readable(log_path) then
			ok("server log found: " .. log_path)
		else
			info("server log path: " .. log_path)
		end
	else
		info("server log path is not known yet")
	end

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

local function report_ui_checks()
	start("UCore UI")

	local picker = config.values.ui and config.values.ui.picker or "auto"
	info("configured picker: " .. tostring(picker))

	if has_module("telescope.pickers") then
		ok("telescope.nvim available")
	else
		info("telescope.nvim not available")
	end

	if has_module("fzf-lua") then
		ok("fzf-lua available")
	else
		info("fzf-lua not available")
	end

	if picker == "telescope" and not has_module("telescope.pickers") then
		warn("configured picker is telescope, but telescope.nvim is not available")
	end

	if picker == "fzf-lua" and not has_module("fzf-lua") then
		warn("configured picker is fzf-lua, but fzf-lua is not available")
	end
end

local function report_completion_checks()
	start("UCore completion")

	local completion = config.values.completion or {}
	info("enabled: " .. yes_no(completion.enable ~= false))
	info("manual keymap: " .. tostring(completion.keymap or ""))

	if has_module("ucore.completion.blink") then
		ok("UCore blink provider can be required")
	else
		error("UCore blink provider cannot be required")
	end

	if has_module("blink.cmp") then
		ok("blink.cmp available")
	else
		info("blink.cmp not available; manual completion can still be used")
	end
end

local function report_treesitter_checks()
	start("UCore Treesitter")

	local buffer = vim.api.nvim_get_current_buf()
	local filetype = vim.bo[buffer].filetype
	info("current buffer filetype: " .. tostring(filetype))

	local ok_parsers, parsers = pcall(require, "nvim-treesitter.parsers")
	if not ok_parsers then
		warn("nvim-treesitter.parsers cannot be required", {
			"Install nvim-treesitter if you want Unreal C++ highlighting.",
		})
		return
	end

	local configs = type(parsers.get_parser_configs) == "function" and parsers.get_parser_configs() or parsers
	if configs and configs.unreal_cpp then
		ok("unreal_cpp parser config registered")
	else
		warn("unreal_cpp parser config is not registered", {
			"Call require('ucore.treesitter').setup() before nvim-treesitter setup.",
		})
	end

	local parser_dir = vim.fn.stdpath("data") .. "/site/parser"
	local parser_file = parser_dir .. "/unreal_cpp.so"
	if vim.fn.has("win32") == 1 then
		parser_file = parser_dir .. "/unreal_cpp.dll"
	end

	if readable(parser_file) then
		ok("unreal_cpp parser installed: " .. parser_file)
	else
		warn("unreal_cpp parser binary not found: " .. parser_file, {
			"Run :TSInstall unreal_cpp after registering the parser.",
		})
	end

	if filetype == "unreal_cpp" then
		local ok_parser, parser_or_err = pcall(vim.treesitter.get_parser, buffer, "unreal_cpp")
		if ok_parser and parser_or_err then
			ok("unreal_cpp parser can attach to current buffer")
		else
			warn("unreal_cpp parser cannot attach: " .. tostring(parser_or_err))
		end
	end
end

-- Neovim calls this function for :checkhealth ucore.
-- Neovim 会在 :checkhealth ucore 时调用这个函数。
function M.check()
	report_static_checks()
	report_project_checks()
	report_server_checks()
	report_ui_checks()
	report_completion_checks()
	report_treesitter_checks()
end

return M
