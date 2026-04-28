local config = require("ucore.config")

local M = {}

local parser_name = "unreal_cpp"
local parser_repo = "https://github.com/vlicecream/UTreeSitter"
local installing = false
local install_attempted = false
local pending_buffers = {}

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

-- Retry pending buffers after install finishes.
-- 安装完成后重试所有等待中的 buffer。
local function retry_pending()
	local pending = pending_buffers
	pending_buffers = {}
	for bufnr, _ in pairs(pending) do
		if vim.api.nvim_buf_is_valid(bufnr) then
			M.activate_buffer(bufnr)
		end
	end
end

-- Auto-install the unreal_cpp parser via nvim-treesitter.
-- 通过 nvim-treesitter 自动安装 unreal_cpp parser。
local function ensure_parser_installed()
	if installing or install_attempted then
		return
	end

	local ok_ts, _ = pcall(require, "nvim-treesitter")
	if not ok_ts then
		return
	end

	local parser_dir = vim.fn.stdpath("data") .. "/site/parser"
	local suffix = vim.fn.has("win32") == 1 and ".dll" or ".so"
	local parser_file = parser_dir .. "/" .. parser_name .. suffix

	if vim.fn.filereadable(parser_file) == 1 then
		install_attempted = true
		return
	end

	installing = true
	status_progress("installing unreal_cpp parser")

	pcall(vim.cmd, "TSInstallSync " .. parser_name)

	installing = false
	install_attempted = true

	if vim.fn.filereadable(parser_file) == 1 then
		status_progress_finish()
		retry_pending()
	else
		status_progress_finish()
		vim.notify(
			"UCore: unreal_cpp parser installation failed.\nRun :TSInstallSync unreal_cpp manually.",
			vim.log.levels.WARN
		)
	end
end

-- Activate treesitter highlighting on a buffer using the unreal_cpp parser.
-- Sets filetype, ensures parser is installed, then starts highlighting.
-- 在 buffer 上激活 unreal_cpp treesitter 高亮。
-- 设置 filetype，确保 parser 已安装，然后启动高亮。
function M.activate_buffer(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()

	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	if vim.bo[bufnr].filetype ~= parser_name then
		return
	end

	local ok, parser = pcall(vim.treesitter.get_parser, bufnr, parser_name)
	if ok and parser then
		pcall(vim.treesitter.start, bufnr, parser_name)
		return
	end

	-- Parser not yet available — queue this buffer for retry after install.
	-- parser 还不可用 — 把 buffer 加入等待队列。
	pending_buffers[bufnr] = true

	if not installing and not install_attempted then
		ensure_parser_installed()
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

	-- When an unreal_cpp buffer appears, auto-install parser and activate highlighting.
	-- 当 unreal_cpp buffer 出现时，自动安装 parser 并激活高亮。
	vim.api.nvim_create_autocmd("FileType", {
		pattern = parser_name,
		group = vim.api.nvim_create_augroup("UCoreUnrealCppActivate", { clear = true }),
		callback = function(ev)
			if config.values.treesitter.auto_install then
				ensure_parser_installed()
			end
			if config.values.treesitter.auto_start then
				vim.schedule(function()
					M.activate_buffer(ev.buf)
				end)
			end
		end,
	})
end

return M
