local M = {}

-- Resolve the plugin repository root from this file path.
-- 从当前文件路径推导插件仓库根目录。
local source = debug.getinfo(1, "S").source:sub(2)
local repo_root = vim.fn.fnamemodify(source, ":p:h:h:h")
local scanner_dir = repo_root .. "/u-scanner"

-- Default runtime configuration for the Rust bridge.
-- Rust 桥接层的默认运行配置。
M.values = {
	port = 30110,
	scanner_dir = scanner_dir,
	db_dir_name = ".ucore",

	-- Development mode: call Cargo directly so code changes are picked up.
	-- 开发模式：直接调用 Cargo，方便 Rust 代码修改后立即生效。
	scanner_cmd = {
		"cargo",
		"run",
		"--quiet",
		"--bin",
		"u_scanner",
		"--",
	},

	-- Server command kept here for later start/stop integration.
	-- server 启动命令先放这里，后面接 :UCoreStart 时复用。
	server_cmd = {
		"cargo",
		"run",
		"--bin",
		"u_core_server",
		"--",
	},
}

-- Merge user options into the default config.
-- 将用户配置合并到默认配置里。
function M.setup(opts)
	M.values = vim.tbl_deep_extend("force", M.values, opts or {})
end

return M
