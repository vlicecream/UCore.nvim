local project = require("ucore.project")
local remote = require("ucore.remote")
local ui = require("ucore.ui")

local M = {}

-- Read current buffer content as one string.
-- 读取当前 buffer 内容，并合并成一个字符串。
local function current_content()
	local lines = vim.api.nvim_buf_get_lines(0, 0, -1, false)
	return table.concat(lines, "\n") .. "\n"
end

-- Open one goto result returned by Rust.
-- 打开 Rust 返回的跳转结果。
local function open_result(result, opts)
	opts = opts or {}
	if not result or result == vim.NIL or vim.tbl_isempty(result) then
		if not opts.silent then
			vim.notify("UCore goto: definition not found", vim.log.levels.WARN)
		end
		return false
	end

	local path = result.file_path or result.path
	local raw_line = result.line_number or result.line or result.row
	local raw_col = result.col or result.column or result.character
	local line = tonumber(raw_line)
	local col = tonumber(raw_col) or 0
	local source = result.source and (" [" .. tostring(result.source) .. "]") or ""

	if not path or path == vim.NIL or path == "" then
		if not opts.silent then
			vim.notify("UCore goto: result has no file path" .. source, vim.log.levels.WARN)
			print(vim.inspect(result))
		end
		return false
	end

	if vim.fn.filereadable(path) ~= 1 then
		if not opts.silent then
			vim.notify("UCore goto: file is not readable: " .. tostring(path), vim.log.levels.WARN)
			print(vim.inspect(result))
		end
		return false
	end

	vim.cmd.edit(vim.fn.fnameescape(path))

	local last_line = vim.api.nvim_buf_line_count(0)
	line = line or 1

	-- Rust DB stores line numbers as 1-based in most tables.
	-- Rust DB 大多数表里的行号是 1-based。
	line = math.max(1, math.min(line, last_line))
	local line_text = vim.api.nvim_buf_get_lines(0, line - 1, line, false)[1] or ""
	col = math.max(0, math.min(col, #line_text))

	vim.api.nvim_win_set_cursor(0, { line, col })
	vim.cmd("normal! zz")

	if source ~= "" then
		vim.notify("UCore goto" .. source .. ": " .. tostring(path) .. ":" .. tostring(line), vim.log.levels.INFO)
	end

	return true
end

local function fallback_normal(keys)
	if not keys or keys == "" then
		return
	end

	vim.cmd("normal! " .. keys)
	vim.cmd("nohlsearch")
end

-- Go to definition with Vim-compatible fallback.
-- 跳转到定义；UCore 无结果时回退到 Vim 原生 gd。
function M.goto_definition()
	M._goto_definition_inner({ fallback = "gd" })
end

-- Go to declaration with Vim-compatible fallback.
-- 跳转到声明；UCore 无结果时回退到 Vim 原生 gD。
function M.goto_declaration()
	M._goto_definition_inner({ fallback = "gD" })
end

-- Go to implementation at the current cursor position.
-- 从当前光标位置跳转到实现（.h -> .cpp）。
function M.goto_implementation()
	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	local file_path = vim.api.nvim_buf_get_name(0)
	if file_path == "" then
		return vim.notify("UCore goto_implementation: current buffer has no file path", vim.log.levels.WARN)
	end

	local cursor = vim.api.nvim_win_get_cursor(0)
	remote.goto_implementation(root, {
		content = current_content(),
		line = cursor[1] - 1,
		character = cursor[2],
		file_path = file_path:gsub("\\", "/"),
	}, function(result, err)
		if err then
			return vim.notify("UCore goto implementation failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end
		vim.cmd("nohlsearch")
		open_result(result)
	end)
end

-- Internal dispatcher with fallback.
-- 内部调度，带回退机制。
function M._goto_definition_inner(opts)
	opts = opts or {}
	local root = project.find_project_root()
	if not root then
		if opts.fallback then
			fallback_normal(opts.fallback)
			return
		end
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	local file_path = vim.api.nvim_buf_get_name(0)
	if file_path == "" then
		if opts.fallback then
			fallback_normal(opts.fallback)
			return
		end
		return vim.notify("UCore goto: current buffer has no file path", vim.log.levels.WARN)
	end

	local cursor = vim.api.nvim_win_get_cursor(0)
	local line = cursor[1] - 1
	local character = cursor[2]

	remote.goto_definition(root, {
		content = current_content(),
		line = line,
		character = character,
		file_path = file_path:gsub("\\", "/"),
	}, function(result, err)
		if err then
			if opts.fallback then
				fallback_normal(opts.fallback)
				return
			end
			return vim.notify("UCore goto failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end

		if not open_result(result, { silent = opts.fallback ~= nil }) and opts.fallback then
			fallback_normal(opts.fallback)
		end
	end)
end

-- Toggle between source (.cpp) and header (.h) file.
-- 在 .cpp 和 .h 文件之间切换。
function M.toggle_source()
	local path = vim.api.nvim_buf_get_name(0)
	if path == "" then
		return vim.notify("UCore: buffer has no file path", vim.log.levels.WARN)
	end

	local alt = find_alternate_source(path)
	if alt and vim.fn.filereadable(alt) == 1 then
		vim.cmd.edit(vim.fn.fnameescape(alt))
	else
		vim.notify("UCore: no matching source/header file found", vim.log.levels.INFO)
	end
end

local function find_alternate_source(path)
	local normalized = path:gsub("\\", "/")
	local ext = normalized:match("%.([^.]*)$")
	if not ext then return nil end

	local lower = ext:lower()
	local is_header = lower == "h" or lower == "hpp" or lower == "hh" or lower == "hxx" or lower == "inl"
	local is_source = lower == "cpp" or lower == "cc" or lower == "cxx"

	if not is_header and not is_source then return nil end

	local target_ext = is_header and "cpp" or "h"
	local base = normalized:sub(1, -(#ext + 2))

	-- Exact match
	local candidate = base .. "." .. target_ext
	if vim.fn.filereadable(candidate) == 1 then return candidate end
	if is_source then
		local hpp = base .. ".hpp"
		if vim.fn.filereadable(hpp) == 1 then return hpp end
	end

	-- Classes/ → Private/ directory mapping
	local alt_base = normalized:gsub("Classes", "Private"):gsub("Public", "Private")
	alt_base = alt_base:sub(1, -(#ext + 2))
	candidate = alt_base .. "." .. target_ext
	if vim.fn.filereadable(candidate) == 1 then return candidate end
	if is_source then
		local hpp = alt_base .. ".hpp"
		if vim.fn.filereadable(hpp) == 1 then return hpp end
	end

	return nil
end

-- Find references for the symbol under the cursor.
-- 查找当前光标下符号的引用位置。
function M.references()
	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	local symbol = vim.fn.expand("<cword>")
	if symbol == "" then
		return vim.notify("UCore references: no symbol under cursor", vim.log.levels.WARN)
	end

	local file_path = vim.api.nvim_buf_get_name(0)
	local normalized_file_path = file_path ~= "" and file_path:gsub("\\", "/") or nil
	local cursor = vim.api.nvim_win_get_cursor(0)

	remote.find_references(root, {
		symbol_name = symbol,
		file_path = normalized_file_path,
		content = current_content(),
		line = cursor[1] - 1,
		character = cursor[2],
	}, function(result, err)
		if err then
			return vim.notify("UCore references failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end

		local references = result and (result.results or result) or {}
		if type(references) ~= "table" or vim.tbl_isempty(references) then
			return vim.notify("UCore references: no references found for " .. symbol, vim.log.levels.WARN)
		end

		ui.select.references(references)
	end)
end

-- Global find: fuzzy search indexed items.
-- 全局搜索：模糊查找已索引内容。
function M.global_find()
	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	require("ucore.commands.actions").global_find("")
end

return M
