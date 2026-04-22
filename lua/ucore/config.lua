local M = {}

-- Resolve the plugin repository root from this file path.
-- 从当前文件路径推导插件仓库根目录。
local source = debug.getinfo(1, "S").source:sub(2)
local repo_root = vim.fn.fnamemodify(source, ":p:h:h:h")
local scanner_dir = repo_root .. "/u-scanner"

-- Return the platform executable suffix.
-- 返回当前平台的可执行文件后缀。
local function exe_suffix()
	return package.config:sub(1, 1) == "\\" and ".exe" or ""
end

-- Check whether a file exists and is readable.
-- 检查文件是否存在且可读。
local function readable(path)
	return vim.fn.filereadable(path) == 1
end

-- Build a release binary path under u-scanner/target/release.
-- 构造 u-scanner/target/release 下的 release binary 路径。
local function release_binary(scanner_root, name)
	return scanner_root .. "/target/release/" .. name .. exe_suffix()
end

-- Build the development CLI command.
-- 构造开发模式 CLI 命令。
local function cargo_scanner_cmd()
	return {
		"cargo",
		"run",
		"--quiet",
		"--bin",
		"u_scanner",
		"--",
	}
end

-- Build the development server command.
-- 构造开发模式 server 命令。
local function cargo_server_cmd()
	return {
		"cargo",
		"run",
		"--bin",
		"u_core_server",
		"--",
	}
end

-- Default runtime configuration for the Rust bridge.
-- Rust 桥接层的默认运行配置。
M.values = {
	port = 30110,
	scanner_dir = scanner_dir,
	db_dir_name = ".ucore",

	-- Prefer release binaries when they exist.
	-- release binary 存在时优先使用它们。
	use_release_binary = true,

	-- Current backend mode: "cargo" or "release".
	-- 当前后端模式："cargo" 或 "release"。
	backend_mode = "cargo",

	-- Automatically boot UCore when opening a file inside an Unreal project.
	-- 打开 Unreal 工程里的文件时自动启动 UCore。
	auto_boot = false,

	-- Delay auto boot slightly to avoid fighting startup/plugin loading.
	-- 稍微延迟自动启动，避免和 nvim 启动/插件加载抢时机。
	auto_boot_delay_ms = 300,

	-- Maximum number of readiness checks during boot.
	-- boot 期间最多检查多少次 server ready。
	boot_ready_attempts = 1200,

	-- Delay between readiness checks in milliseconds.
	-- 每次 server ready 检查之间的延迟毫秒数。
	boot_ready_interval_ms = 100,

	-- Events that may trigger auto boot.
	-- 可能触发自动启动的事件。
	auto_boot_events = {
		"BufReadPost",
		"BufNewFile",
	},

	-- Development mode: call Cargo directly so code changes are picked up.
	-- 开发模式：直接调用 Cargo，方便 Rust 代码修改后立即生效。
	scanner_cmd = cargo_scanner_cmd(),

	-- Server command kept here for later start/stop integration.
	-- server 启动命令先放这里，后面接 :UCoreStart 时复用。
	server_cmd = cargo_server_cmd(),
}

-- Return the release binary path for one backend executable.
-- 返回某个后端可执行文件的 release binary 路径。
function M.release_binary(name)
	return release_binary(M.values.scanner_dir, name)
end

-- Return true when both backend release binaries exist.
-- 当两个后端 release binary 都存在时返回 true。
function M.has_release_binaries()
	return readable(M.release_binary("u_scanner")) and readable(M.release_binary("u_core_server"))
end

-- Refresh backend commands from the current config.
-- 根据当前配置刷新后端命令。
function M.refresh_backend_commands(opts)
	opts = opts or {}

	local update_scanner = opts.scanner ~= false
	local update_server = opts.server ~= false

	if M.values.use_release_binary and M.has_release_binaries() then
		if update_scanner then
			M.values.scanner_cmd = { M.release_binary("u_scanner") }
		end
		if update_server then
			M.values.server_cmd = { M.release_binary("u_core_server") }
		end
		M.values.backend_mode = "release"
		return
	end

	if update_scanner then
		M.values.scanner_cmd = cargo_scanner_cmd()
	end
	if update_server then
		M.values.server_cmd = cargo_server_cmd()
	end
	M.values.backend_mode = "cargo"
end

-- Merge user options into the default config.
-- 将用户配置合并到默认配置里。
function M.setup(opts)
	local has_custom_scanner_cmd = opts and opts.scanner_cmd ~= nil
	local has_custom_server_cmd = opts and opts.server_cmd ~= nil

	M.values = vim.tbl_deep_extend("force", M.values, opts or {})
	M.refresh_backend_commands({
		scanner = not has_custom_scanner_cmd,
		server = not has_custom_server_cmd,
	})
end

M.refresh_backend_commands()

return M
