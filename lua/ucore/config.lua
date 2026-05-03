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

	-- Root directory for UCore registry, databases, and runtime logs.
	-- UCore 注册表、数据库和运行时日志的根目录。
	cache_dir = vim.fn.stdpath("data") .. "/ucore",


	-- Prefer release binaries when they exist.
	-- release binary 存在时优先使用它们。
	use_release_binary = true,

	-- Current backend mode: "cargo" or "release".
	-- 当前后端模式："cargo" 或 "release"。
	backend_mode = "cargo",

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
		-- Show refresh progress notifications from the Rust server.
		-- 显示 Rust server 返回的 refresh 进度通知。
		enable = true,
	},

	-- UI integration options.
	-- UI 集成配置。
	ui = {
		-- Picker backend: "auto", "vim", "fzf-lua", or "telescope".
		-- 选择器后端："auto"、"vim"、"fzf-lua" 或 "telescope"。
		picker = "auto",
	},

	-- Navigation keymaps.
	-- 导航快捷键。
	navigation = {
		keymaps = {
			-- Register buffer-local default navigation mappings for Unreal C++ files.
			-- 为 Unreal C++ buffer 注册默认导航快捷键。
			enable = true,

			-- Go to definition. Core navigation.
			-- 跳转到定义。核心代码跳转。
			definition = "gd",

			-- Go to declaration. Specifically jumps to .h declaration.
			-- 跳转到声明。专门跳转到 .h 的声明。
			declaration = "gD",

			-- Find references. Searches entire project.
			-- 查找引用。全工程搜索。
			references = "gr",

			-- Go to implementation (.h -> .cpp).
			-- 跳转到实现（.h -> .cpp）。
			implementation = "gi",

			-- Toggle between source (.cpp) and header (.h) file.
			-- 在 .cpp 和 .h 文件之间切换。
			source_toggle = "gs",

			-- Global find: fuzzy search indexed symbols, modules, assets, config.
			-- 全局搜索：模糊查找已索引的符号、模块、资产、配置。
			global_find = "gf",
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
		auto_open = true,
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
		-- Minimum identifier prefix length before global completion starts.
		-- 触发全局补全所需的最短标识符前缀长度。
		min_chars = 2,

		-- Debounce delay for automatic completion requests.
		-- 自动补全请求的防抖延迟。
		debounce_ms = 180,
	},

	-- Build diagnostics and quickfix options.
	-- 构建诊断与 quickfix 配置。
	build = {
		-- Auto-open quickfix when the build has errors.
		-- 构建有 error 时自动打开 quickfix。
		open_quickfix_on_error = true,

		-- Include warnings in the quickfix list.
		-- warning 是否也加入 quickfix 列表。
		include_warnings = true,

		-- Colorize the build log buffer with extmarks.
		-- 是否给 build log buffer 加颜色高亮。
		color_log = true,
	},

	-- UCore diagnostics rendered through vim.diagnostic.
	-- 通过 vim.diagnostic 渲染 UCore 诊断。
	diagnostics = {
		-- Enable UCore diagnostics.
		-- 是否启用 UCore 诊断。
		enable = true,

		-- Smart quick-fix keymap. Set to false or "" to disable the default mapping.
		-- 智能修复快捷键。设为 false 或 "" 可关闭默认映射。
		action_keymap = "<leader>ca",

		-- Show underline for diagnostics.
		-- 是否显示红线/黄线下划线。
		underline = true,

		-- Show inline virtual text. Disabled by default to avoid noisy C++ buffers.
		-- 是否显示行内虚拟文本，默认关闭以减少 C++ buffer 噪音。
		virtual_text = false,

		-- Show sign column markers.
		-- 是否显示 sign column 标记。
		signs = true,

		-- Show a diagnostic float when the cursor stays on a red/yellow line.
		-- 光标停留在红线/黄线上时是否自动弹出诊断浮窗。
		float_on_cursor = true,

		-- Also show the diagnostic float while typing in Insert mode.
		-- 插入模式下是否也自动弹出诊断浮窗。
		float_in_insert = false,

		-- Delay before showing the cursor diagnostic float.
		-- 光标诊断浮窗的延迟时间。
		float_delay_ms = 200,

		-- Update diagnostics while typing in Insert mode.
		-- 插入模式输入时是否更新诊断。
		update_in_insert = true,

		-- Debounce delay for diagnostics refresh.
		-- 诊断刷新的防抖延迟。
		debounce_ms = 300,
	},

	-- Recommended LSP integration for semantic diagnostics and code actions.
	-- 推荐的 LSP 集成，用于语义红线黄线和 code action。
	lsp = {
		auto_setup = true,
		clangd = {
			command = "clangd",
			args = {
				"--header-insertion=never",
				"--completion-style=detailed",
				"--function-arg-placeholders",
				"--pch-storage=disk",
				"--fallback-style=llvm",
			},
			prefer_blink_capabilities = true,
			single_file_support = false,
			compile_commands_dir = nil,
			require_compile_commands = true,
			auto_generate_compile_commands = true,
			auto_detect_windows = true,

			-- Suppress clangd IncludeCleaner "unused include" diagnostics in
			-- Unreal projects when needed. Disabled by default so users can
			-- inspect the raw clangd warnings first.
			-- 需要时可屏蔽 clangd IncludeCleaner 的“unused include”诊断。
			-- 默认关闭，先保留 clangd 原始 warning 方便观察。
			suppress_unused_include_warnings = false,
		},
	},

	-- Auto-pairs integration via nvim-autopairs.
	-- nvim-autopairs 自动配对集成。
	editing = {
		enable = true,
		indent = {
			enable = true,
			inherit_cpp = true,
			fallback_cindent = true,
		},
	},

	autopairs = {
		-- Enable nvim-autopairs integration. Disable if using your own config.
		-- 启用 nvim-autopairs 集成。若自行配置，可关闭。
		enable = true,

		-- Let nvim-autopairs handle <CR> inside pairs such as {|}.
		-- 允许 nvim-autopairs 处理 {|} 中的回车展开。
		map_cr = true,

		-- Use treesitter context checks when available.
		-- parser 可用时使用 treesitter 上下文检查。
		check_ts = true,
	},

	-- Semantic highlight overlay powered by the UCore index.
	-- 基于 UCore 索引的语义高亮覆盖层。
	semantic = {
		-- Enable semantic extmark highlights for indexed declarations.
		-- 是否启用已索引声明的语义 extmark 高亮。
		enable = true,

		-- Debounce delay for buffer semantic refresh.
		-- buffer 语义高亮刷新的防抖延迟。
		debounce_ms = 120,
	},

	-- Debugging integration powered by nvim-dap + cppvsdbg.
	-- 基于 nvim-dap + cppvsdbg 的调试集成。
	debug = {
		-- Enable UCore debug integration.
		-- 是否启用 UCore 调试集成。
		enable = true,

		-- Save modified project files before launching a debug session.
		-- 启动调试前是否先保存项目中的已修改文件。
		autosave_before_launch = true,

		-- Redirect breakpoints placed on header declarations to the matching
		-- source definition when possible.
		-- 尽量把头文件声明上的断点重定向到对应 .cpp 定义。
		redirect_header_breakpoints = true,

		-- Adapter resolution. `command = nil` means auto-detect OpenDebugAD7.exe.
		-- 调试适配器解析。`command = nil` 表示自动查找 OpenDebugAD7.exe。
		adapter = {
			command = nil,
			args = {},

			-- Auto-install the debug adapter via mason.nvim when possible.
			-- 缺少调试适配器时，尽量通过 mason.nvim 自动安装。
			auto_install = true,

			-- Mason package name to install for cppvsdbg support.
			-- 用于提供 cppvsdbg 支持的 Mason 包名。
			package = "cpptools",
		},

		ui = {
			-- Auto-open the built-in minimal debug UI on session start.
			-- 会话开始时自动打开内置的最轻调试 UI。
			auto_open = true,

			-- Auto-close the UI when the session ends.
			-- 会话结束时自动关闭 UI。
			auto_close = true,
		},

		keymaps = {
			enable = true,
			toggle_breakpoint = "<leader>db",
			continue = "<leader>dc",
			attach = "<leader>da",
			launch_editor = "<leader>de",
			restart = "<leader>dr",
			stop = "<leader>ds",
			step_over = "<leader>do",
			step_into = "<leader>di",
			step_out = "<leader>du",
			hover = "<leader>dh",
			processes = "<leader>dp",
			list_breakpoints = "<leader>dl",
			ui = "<leader>dt",
		},
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
