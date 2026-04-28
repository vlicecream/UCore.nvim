local config = require("ucore.config")

local M = {}

local parser_name = "unreal_cpp"
local parser_repo = "https://github.com/vlicecream/UTreeSitter"
local installing = false
local install_attempted = false
local install_notified = false
local pending_buffers = {}
local retry_timer = nil

local MAX_INSTALL_RETRIES = 120
local INSTALL_RETRY_DELAY_MS = 500

local function is_unreal_project(path)
	local markers = vim.fs.find(function(name)
		return name:match("%.uproject$") or name:match("%.uplugin$")
	end, {
		path = vim.fs.dirname(path),
		upward = true,
		type = "file",
		limit = 1,
	})
	return #markers > 0
end

local function augment_parsers(parsers)
	parsers[parser_name] = vim.tbl_deep_extend("force", parsers[parser_name] or {}, {
		install_info = {
			url = parser_repo,
			files = { "src/parser.c", "src/scanner.c" },
			queries = "queries/unreal_cpp",
			generate_requires_npm = false,
			requires_generate_from_grammar = false,
		},
		filetype = parser_name,
		maintainers = { "@vlicecream" },
		tier = 2,
	})

	return parsers
end

local function stock_parsers_path()
	return vim.fn.stdpath("data") .. "/lazy/nvim-treesitter/lua/nvim-treesitter/parsers.lua"
end

local function build_parsers()
	local module_path = stock_parsers_path()
	if not vim.uv.fs_stat(module_path) then
		return nil
	end

	return augment_parsers(dofile(module_path))
end

local function register_parser()
	local ok, parsers = pcall(require, "nvim-treesitter.parsers")
	if ok and type(parsers) == "table" then
		augment_parsers(parsers)
		package.loaded["nvim-treesitter.parsers"] = parsers
	end
end

local function apply_highlight_links()
	local hl = vim.api.nvim_set_hl

	local styles = {
		["@keyword.unreal_cpp"] = { fg = "#6EA6FF" },
		["@keyword.directive.unreal_cpp"] = { fg = "#5AA8FF", bold = true },
		["@keyword.function.unreal_cpp"] = { fg = "#C792EA", bold = true },

		["@type.unreal_cpp"] = { fg = "#D18CFF" },
		["@type.enum.unreal_cpp"] = { fg = "#D18CFF", bold = true },
		["@type.builtin.unreal_cpp"] = { fg = "#4FC1FF" },
		["@type.qualifier.unreal_cpp"] = { fg = "#4FC1FF", italic = true },

		["@function.unreal_cpp"] = { fg = "#4EC9B0" },
		["@function.method.unreal_cpp"] = { fg = "#4EC9B0", italic = true },
		["@function.macro.unreal_cpp"] = { fg = "#D19A66", bold = true },
		["@function.macro.delegate.unreal_cpp"] = { fg = "#D19A66", bold = true, italic = true },

		["@property.unreal_cpp"] = { fg = "#61AFEF" },
		["@variable.unreal_cpp"] = { fg = "#E5C07B" },
		["@variable.builtin.unreal_cpp"] = { fg = "#56B6C2" },
		["@parameter.unreal_cpp"] = { fg = "#FFD866" },

		["@string.unreal_cpp"] = { fg = "#CE9178" },
		["@string.special.unreal_cpp"] = { fg = "#D7BA7D" },
		["@number.unreal_cpp"] = { fg = "#F78C6C" },
		["@comment.unreal_cpp"] = { fg = "#6A9955", italic = true },

		["@constant.unreal_cpp"] = { fg = "#C678DD" },
		["@constant.enum.unreal_cpp"] = { fg = "#F78C6C", bold = true },
		["@constant.builtin.unreal_cpp"] = { fg = "#56B6C2", bold = true },
		["@macro.unreal_cpp"] = { fg = "#D19A66", bold = true },
	}

	for target, style in pairs(styles) do
		hl(0, target, style)
	end
end

local function unreal_filetype(path)
	if is_unreal_project(path) then
		return parser_name
	end
end

-- ---------------------------------------------------------------------------
-- Readiness checks
-- ---------------------------------------------------------------------------

-- Check whether the parser binary file exists on disk.
-- 检查 parser 文件是否存在于磁盘。
local function parser_installed()
	local parser_dir = vim.fn.stdpath("data") .. "/site/parser"
	local candidates = {
		parser_dir .. "/" .. parser_name .. ".so",
		parser_dir .. "/" .. parser_name .. ".dll",
		parser_dir .. "/" .. parser_name .. ".dylib",
	}

	for _, path in ipairs(candidates) do
		if vim.fn.filereadable(path) == 1 then
			return true
		end
	end

	return false
end

-- Check whether the parser can attach to a buffer via get_parser.
-- 检查 parser 是否能通过 get_parser 附加到 buffer。
local function parser_can_attach(bufnr)
	return pcall(vim.treesitter.get_parser, bufnr, parser_name)
end

-- Check whether nvim-treesitter and the parser config are fully ready.
-- 检查 nvim-treesitter 和 parser config 是否完全就绪。
local function treesitter_ready()
	if vim.fn.exists(":TSInstallSync") ~= 2 then
		return false
	end

	local ok, parsers = pcall(require, "nvim-treesitter.parsers")
	if not ok or type(parsers) ~= "table" then
		return false
	end

	local configs = parsers
	if type(parsers.get_parser_configs) == "function" then
		configs = parsers.get_parser_configs()
	end

	return configs and configs.unreal_cpp ~= nil
end

-- ---------------------------------------------------------------------------
-- Status panel helpers
-- ---------------------------------------------------------------------------

local function status_progress(msg)
	pcall(function()
		require("ucore.status").progress("UCore treesitter", msg)
	end)
end

local function status_progress_finish()
	pcall(function()
		require("ucore.status").progress_finish("UCore treesitter", "UCore treesitter 100%")
	end)
end

-- ---------------------------------------------------------------------------
-- Retry queue
-- ---------------------------------------------------------------------------

-- Retry pending buffers after parser becomes available.
-- parser 就绪后重试所有等待中的 buffer。
local function retry_pending()
	local pending = pending_buffers
	pending_buffers = {}
	for bufnr, _ in pairs(pending) do
		if vim.api.nvim_buf_is_valid(bufnr) then
			M.activate_buffer(bufnr)
		end
	end
end

local function any_pending_can_attach()
	for bufnr, _ in pairs(pending_buffers) do
		if parser_can_attach(bufnr) then
			return true
		end
	end

	return false
end

-- Attempt to install the parser. Retries if nvim-treesitter is not ready yet.
-- 尝试安装 parser。如果 nvim-treesitter 未就绪则重试。
local function install_with_retry(remaining)
	if remaining <= 0 then
		installing = false
		if not install_notified then
			install_notified = true
			status_progress_finish()
		end
		return
	end

	if any_pending_can_attach() then
		installing = false
		install_attempted = true
		status_progress_finish()
		retry_pending()
		return
	end

	if not treesitter_ready() then
		register_parser()
		retry_timer = vim.defer_fn(function()
			install_with_retry(remaining - 1)
		end, INSTALL_RETRY_DELAY_MS)
		return
	end

	status_progress("installing unreal_cpp parser")

	pcall(vim.cmd, "TSInstallSync " .. parser_name)

	if parser_installed() or any_pending_can_attach() then
		installing = false
		install_attempted = true
		status_progress_finish()
		retry_pending()
		return
	end

	retry_timer = vim.defer_fn(function()
		install_with_retry(remaining - 1)
	end, INSTALL_RETRY_DELAY_MS)
end

-- Activate treesitter highlighting on a buffer using the unreal_cpp parser.
-- 在 buffer 上激活 unreal_cpp treesitter 高亮。
function M.activate_buffer(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()

	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	if vim.bo[bufnr].filetype ~= parser_name then
		return
	end

	if parser_can_attach(bufnr) then
		pcall(vim.treesitter.start, bufnr, parser_name)
		return
	end

	pending_buffers[bufnr] = true

	if installing then
		return
	end

	installing = true
	install_with_retry(MAX_INSTALL_RETRIES)
end

local function activate_existing_buffers()
	register_parser()

	for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
		if vim.api.nvim_buf_is_valid(bufnr) and vim.bo[bufnr].filetype == parser_name then
			M.activate_buffer(bufnr)
		end
	end
end

function M.setup()
	package.preload["nvim-treesitter.parsers"] = function()
		if type(package.loaded["nvim-treesitter.parsers"]) == "table" then
			return package.loaded["nvim-treesitter.parsers"]
		end

		local parsers = build_parsers()
		if not parsers then
			error("nvim-treesitter.parsers not available")
		end

		package.loaded["nvim-treesitter.parsers"] = parsers
		return parsers
	end

	register_parser()
	vim.schedule(register_parser)
	vim.defer_fn(register_parser, 100)
	vim.defer_fn(register_parser, 500)

	apply_highlight_links()
	vim.api.nvim_create_autocmd("ColorScheme", {
		group = vim.api.nvim_create_augroup("UCoreUnrealHighlights", { clear = true }),
		callback = apply_highlight_links,
	})

	vim.filetype.add({
		extension = {
			cpp = unreal_filetype,
			h = unreal_filetype,
			hpp = unreal_filetype,
			hh = unreal_filetype,
			cc = unreal_filetype,
			cxx = unreal_filetype,
			inl = unreal_filetype,
		},
	})

	-- When an unreal_cpp buffer appears, queue it for activation.
	-- 当 unreal_cpp buffer 出现时，加入激活队列。
	vim.api.nvim_create_autocmd("FileType", {
		pattern = parser_name,
		group = vim.api.nvim_create_augroup("UCoreUnrealCppActivate", { clear = true }),
		callback = function(ev)
			if config.values.treesitter.auto_start then
				vim.schedule(function()
					M.activate_buffer(ev.buf)
				end)
			end
		end,
	})

	vim.api.nvim_create_autocmd("User", {
		pattern = { "LazyDone", "VeryLazy" },
		group = vim.api.nvim_create_augroup("UCoreUnrealCppLazyRetry", { clear = true }),
		callback = function()
			vim.defer_fn(activate_existing_buffers, 100)
			vim.defer_fn(activate_existing_buffers, 500)
		end,
	})
end

return M
