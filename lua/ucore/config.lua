local M = {}

-- Resolve the plugin repository root from this file path.
-- 从当前文件路径推导插件仓库根目录。
local source = debug.getinfo(1, "S").source:sub(2)
local repo_root = vim.fn.fnamemodify(source, ":p:h:h:h"):gsub("\\", "/")

-- Return the platform executable suffix.
-- 返回当前平台的可执行文件后缀。
local function exe_suffix()
	return package.config:sub(1, 1) == "\\" and ".exe" or ""
end

-- Normalize one filesystem path to an absolute slash-separated path.
-- 把文件系统路径规范成绝对路径并统一为正斜杠。
local function normalize(path)
	if not path or path == "" then
		return nil
	end

	local absolute = vim.fn.fnamemodify(path, ":p")
	absolute = absolute:gsub("\\", "/")
	local trimmed = absolute:gsub("/+$", "")
	return trimmed
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

-- Add one unique normalized path into a list.
-- 往列表里追加一个唯一的规范化路径。
local function push_unique(list, path)
	local normalized = normalize(path)
	if not normalized then
		return
	end

	for _, item in ipairs(list) do
		if item == normalized then
			return
		end
	end

	table.insert(list, normalized)
end

-- Build an executable path inside a release directory.
-- 在 release 目录下构造可执行文件路径。
local function release_binary(release_dir, name)
	if not release_dir then
		return nil
	end

	return normalize(release_dir .. "/" .. name .. exe_suffix())
end

-- Return true when both backend release binaries exist in one directory.
-- 当某个目录下两个后端 release binary 都存在时返回 true。
local function has_release_binaries_in(dir)
	return readable(release_binary(dir, "u_scanner")) and readable(release_binary(dir, "u_core_server"))
end

local function mtime(path)
	path = normalize(path)
	if not path then
		return nil
	end

	local stat = vim.loop.fs_stat(path)
	return stat and stat.mtime and stat.mtime.sec or nil
end

local function backend_source_stamp(source_dir)
	source_dir = normalize(source_dir)
	if not source_dir then
		return nil
	end

	local stamp = 0
	for _, path in ipairs({
		source_dir .. "/Cargo.toml",
		source_dir .. "/Cargo.lock",
		source_dir .. "/.git/index",
		source_dir .. "/.git/HEAD",
	}) do
		stamp = math.max(stamp, mtime(path) or 0)
	end

	return stamp > 0 and stamp or nil
end

local function release_binaries_are_fresh(release_dir, source_dir)
	if not has_release_binaries_in(release_dir) then
		return false
	end

	local source_stamp = backend_source_stamp(source_dir)
	if not source_stamp then
		return true
	end

	local scanner_stamp = mtime(release_binary(release_dir, "u_scanner")) or 0
	local server_stamp = mtime(release_binary(release_dir, "u_core_server")) or 0
	return scanner_stamp >= source_stamp and server_stamp >= source_stamp
end

-- Build the development CLI command.
-- 构造开发模式 CLI 命令。
local function cargo_scanner_cmd(source_dir)
	return {
		"cargo",
		"run",
		"--quiet",
		"--manifest-path",
		source_dir .. "/Cargo.toml",
		"--bin",
		"u_scanner",
		"--",
	}
end

-- Build the development server command.
-- 构造开发模式 server 命令。
local function cargo_server_cmd(source_dir)
	return {
		"cargo",
		"run",
		"--manifest-path",
		source_dir .. "/Cargo.toml",
		"--bin",
		"u_core_server",
		"--",
	}
end

local function default_values()
	local cache_dir = vim.fn.stdpath("data") .. "/ucore"
	local managed_root = normalize(cache_dir .. "/backend")
	local managed_source_dir = normalize(managed_root .. "/UScanner")
	local xdg_data_home = normalize(vim.env.XDG_DATA_HOME)
	local managed_env_root = xdg_data_home and normalize(xdg_data_home .. "/ucore/backend") or nil
	local managed_env_source_dir = managed_env_root and normalize(managed_env_root .. "/UScanner") or nil

	return {
		port = 30110,

		-- Root directory for UCore registry, databases, and runtime logs.
		-- UCore 注册表、数据库和运行时日志的根目录。
		cache_dir = cache_dir,

		-- Prefer release binaries when they exist.
		-- release binary 存在时优先使用它们。
		use_release_binary = true,

		-- Backend source / binary resolution.
		-- 后端源码和二进制解析配置。
		backend = {
			repo = "vlicecream/UScanner",
			repo_url = "https://github.com/vlicecream/UScanner.git",
			source_dir = nil,
			bin_dir = nil,
			managed_root = managed_root,
			managed_source_dir = managed_source_dir,
			managed_env_root = managed_env_root,
			managed_env_source_dir = managed_env_source_dir,
		},

		-- Current backend mode: "missing", "cargo", or "release".
		-- 当前后端模式："missing"、"cargo" 或 "release"。
		backend_mode = "missing",

		-- Resolved backend paths after setup.
		-- setup 后解析出的后端路径。
		backend_source_dir = nil,
		backend_release_dir = nil,
		backend_bin_dir = nil,
		backend_cwd = repo_root,
		backend_manifest_path = nil,
		scanner_dir = nil,

		-- Automatically boot UCore when opening a file inside an Unreal project.
		-- 打开 Unreal 工程里的文件时自动启动 UCore。
		auto_boot = true,

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
			"BufEnter",
			"VimEnter",
			"DirChanged",
		},

		-- Auto-save interval in seconds. `0` disables auto-save.
		-- 自动保存间隔，单位秒。`0` 表示关闭。
		autosave = 0,

		-- Refresh progress notification options.
		-- refresh 进度通知配置。
		progress = {
			enable = true,
		},

		-- UI integration options.
		-- UI 集成配置。
		ui = {
			picker = "auto",
			output = {
				enable = true,
				auto_open = true,
				height = 12,
				max_tabs = 8,
			},
		},

		-- Navigation keymaps.
		-- 导航快捷键。
		navigation = {
			keymaps = {
				enable = true,
				definition = "gd",
				declaration = "gD",
				references = "gr",
				implementation = "gi",
				source_toggle = "gs",
				global_find = "gf",
				hover = "K",
				signature = "<C-k>",
				rename = "<leader>rn",
			},
		},

		-- Left-side project/source/config explorer.
		-- 左侧 Project/Source/Config 目录浏览器。
		explorer = {
			width = 36,
			min_width = 28,
			max_width = 56,
			tabs = { "Project", "Source", "Config" },
			default_tab = "Project",
			auto_open = false,
			auto_focus = false,
			auto_open_delay_ms = 120,
			close_other_explorers = false,
			show_hidden = false,
			search_case_sensitive = false,
			exclude_dirs = {
				".git",
				".svn",
				".p4",
			},
		},

		-- Completion integration options.
		-- 补全集成配置。
		completion = {
			min_chars = 2,
			debounce_ms = 180,
			debug = false,
		},

		-- UCore diagnostics rendered through vim.diagnostic.
		-- 通过 vim.diagnostic 渲染 UCore 诊断。
		diagnostics = {
			enable = true,
			action_keymap = "<leader>ca",
			underline = true,
			virtual_text = false,
			signs = true,
			float_on_cursor = true,
			float_in_insert = false,
			float_delay_ms = 200,
			update_in_insert = true,
			debounce_ms = 300,
		},

		-- Auto-pairs integration via nvim-autopairs.
		-- nvim-autopairs 自动配对集成。
		editing = {
			enable = true,
			disable_autoformat = true,
			indent = {
				enable = true,
				inherit_cpp = true,
				fallback_cindent = true,
			},
		},

		autopairs = {
			enable = true,
			map_cr = true,
			check_ts = true,
		},

		-- Semantic highlight overlay powered by the UCore index.
		-- 基于 UCore 索引的语义高亮覆盖层。
		semantic = {
			enable = true,
			debounce_ms = 120,
		},

		-- Backend executable commands resolved at setup time.
		-- setup 时解析出的后端可执行命令。
		scanner_cmd = {},
		server_cmd = {},
	}
end

M.values = default_values()

-- Return ordered backend source candidates.
-- 返回有序的后端源码候选目录。
function M.backend_source_candidates(values)
	values = values or M.values

	local backend = values.backend or {}
	local dirs = {}
	push_unique(dirs, backend.source_dir)
	push_unique(dirs, backend.managed_source_dir)
	push_unique(dirs, backend.managed_env_source_dir)
	return dirs
end

-- Return ordered backend release binary candidates.
-- 返回有序的后端 release 二进制候选目录。
function M.backend_bin_candidates(values)
	values = values or M.values

	local backend = values.backend or {}
	local dirs = {}
	push_unique(dirs, backend.bin_dir)

	local source_dir = values.backend_source_dir
	if source_dir then
		push_unique(dirs, source_dir .. "/target/release")
	end

	return dirs
end

-- Resolve the first usable backend source directory.
-- 解析第一个可用的后端源码目录。
function M.resolve_backend_source_dir(values)
	values = values or M.values

	for _, dir in ipairs(M.backend_source_candidates(values)) do
		if is_dir(dir) and readable(dir .. "/Cargo.toml") then
			return dir
		end
	end

	local explicit = values.backend and values.backend.source_dir
	return normalize(explicit)
end

-- Resolve the first usable release binary directory.
-- 解析第一个可用的 release 二进制目录。
function M.resolve_backend_release_dir(values)
	values = values or M.values

	for _, dir in ipairs(M.backend_bin_candidates(values)) do
		if is_dir(dir) and release_binaries_are_fresh(dir, values.backend_source_dir) then
			return dir
		end
	end

	return nil
end

-- Return the release binary path for one backend executable.
-- 返回某个后端可执行文件的 release binary 路径。
function M.release_binary(name, dir)
	local release_dir = dir or M.values.backend_release_dir or M.values.backend_bin_dir
	return release_binary(release_dir, name)
end

-- Return true when both backend release binaries exist.
-- 当两个后端 release binary 都存在时返回 true。
function M.has_release_binaries()
	return M.values.backend_release_dir ~= nil
end

-- Refresh backend commands from the current config.
-- 根据当前配置刷新后端命令。
function M.refresh_backend_commands(opts)
	opts = opts or {}

	local update_scanner = opts.scanner ~= false
	local update_server = opts.server ~= false

	local source_dir = M.resolve_backend_source_dir(M.values)
	M.values.backend_source_dir = source_dir
	M.values.backend_manifest_path = source_dir and (source_dir .. "/Cargo.toml") or nil
	M.values.scanner_dir = source_dir

	local preferred_bin_dir = normalize((M.values.backend or {}).bin_dir)
	M.values.backend_bin_dir = preferred_bin_dir

	local release_dir = M.resolve_backend_release_dir(M.values)
	M.values.backend_release_dir = release_dir

	if release_dir then
		M.values.backend_bin_dir = release_dir
	end

	M.values.backend_cwd = source_dir or release_dir or repo_root
	if not M.values.backend_bin_dir and source_dir then
		M.values.backend_bin_dir = normalize(source_dir .. "/target/release")
	end

	if M.values.use_release_binary and release_dir then
		if update_scanner then
			M.values.scanner_cmd = { M.release_binary("u_scanner", release_dir) }
		end
		if update_server then
			M.values.server_cmd = { M.release_binary("u_core_server", release_dir) }
		end
		M.values.backend_mode = "release"
		return
	end

	if source_dir and readable(source_dir .. "/Cargo.toml") then
		if update_scanner then
			M.values.scanner_cmd = cargo_scanner_cmd(source_dir)
		end
		if update_server then
			M.values.server_cmd = cargo_server_cmd(source_dir)
		end
		M.values.backend_mode = "cargo"
		return
	end

	if not update_scanner or not update_server then
		M.values.backend_mode = "custom"
		return
	end

	if update_scanner then
		M.values.scanner_cmd = { M.release_binary("u_scanner", M.values.backend_bin_dir) }
	end
	if update_server then
		M.values.server_cmd = { M.release_binary("u_core_server", M.values.backend_bin_dir) }
	end
	M.values.backend_mode = "missing"
end

-- Merge legacy top-level options into the new backend block.
-- 把旧的顶层配置兼容地合并到新的 backend 配置块。
local function normalize_user_opts(opts)
	if not opts then
		return {}
	end

	local normalized = vim.deepcopy(opts)
	if normalized.scanner_dir ~= nil then
		normalized.backend = normalized.backend or {}
		if normalized.backend.source_dir == nil then
			normalized.backend.source_dir = normalized.scanner_dir
		end
	end

	return normalized
end

-- Merge user options into the default config.
-- 将用户配置合并到默认配置里。
function M.setup(opts)
	local normalized_opts = normalize_user_opts(opts)
	local has_custom_scanner_cmd = normalized_opts.scanner_cmd ~= nil
	local has_custom_server_cmd = normalized_opts.server_cmd ~= nil

	M.values = vim.tbl_deep_extend("force", default_values(), normalized_opts)
	M.refresh_backend_commands({
		scanner = not has_custom_scanner_cmd,
		server = not has_custom_server_cmd,
	})
end

M.refresh_backend_commands()

return M
