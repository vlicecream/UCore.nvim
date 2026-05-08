local project = require("ucore.project")
local remote = require("ucore.remote")
local ui = require("ucore.ui")

local M = {}
local current_content
local open_result
local find_alternate_source

local function normalize_path(path)
	return path and path:gsub("\\", "/") or ""
end

local function is_header_file(path)
	local ext = normalize_path(path):match("%.([^.]*)$")
	if not ext then
		return false
	end

	ext = ext:lower()
	return ext == "h" or ext == "hpp" or ext == "hh" or ext == "hxx" or ext == "inl"
end

local function is_source_file(path)
	local ext = normalize_path(path):match("%.([^.]*)$")
	if not ext then
		return false
	end

	ext = ext:lower()
	return ext == "cpp" or ext == "cc" or ext == "cxx"
end

local function normalize_space(text)
	return tostring(text or ""):gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")
end

local function cursor_cword()
	return tostring(vim.fn.expand("<cword>") or "")
end

local function refresh_opened_buffer_filetype(path, bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	local ok_utreesitter, utreesitter_filetype = pcall(require, "utreesitter.filetype")
	if ok_utreesitter and utreesitter_filetype and type(utreesitter_filetype.apply_to_buffer) == "function" then
		pcall(utreesitter_filetype.apply_to_buffer, bufnr)
		return
	end

	local detected = vim.filetype.match({ buf = bufnr, filename = path }) or vim.filetype.match({ filename = path })
	if detected and detected ~= "" and vim.bo[bufnr].filetype ~= detected then
		vim.bo[bufnr].filetype = detected
	end
end

local function find_buffer_for_path(path)
	path = normalize_path(path)
	if path == "" then
		return nil
	end

	for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
		if vim.api.nvim_buf_is_valid(bufnr) and normalize_path(vim.api.nvim_buf_get_name(bufnr)) == path then
			return bufnr
		end
	end

	return nil
end

local function file_lines(path)
	if vim.fn.filereadable(path) ~= 1 then
		return {}
	end

	local ok, lines = pcall(vim.fn.readfile, path)
	return ok and lines or {}
end

local function lines_for_path(path)
	local bufnr = find_buffer_for_path(path)
	if bufnr and vim.api.nvim_buf_is_valid(bufnr) then
		if not vim.api.nvim_buf_is_loaded(bufnr) then
			pcall(vim.fn.bufload, bufnr)
		end
		if vim.api.nvim_buf_is_loaded(bufnr) then
			return vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
		end
	end

	return file_lines(path)
end

local function header_to_source_candidates(path)
	path = normalize_path(path)
	if path == "" then
		return {}
	end

	local ext = path:match("%.([^.]*)$")
	if not ext then
		return {}
	end

	local base = path:sub(1, -(#ext + 2))
	local candidates = {
		base .. ".cpp",
		base .. ".cc",
		base .. ".cxx",
	}

	local mapped = path:gsub("/Classes/", "/Private/"):gsub("/Public/", "/Private/")
	if mapped ~= path then
		local mapped_base = mapped:sub(1, -(#ext + 2))
		table.insert(candidates, 1, mapped_base .. ".cpp")
		table.insert(candidates, 2, mapped_base .. ".cc")
		table.insert(candidates, 3, mapped_base .. ".cxx")
	end

	local seen = {}
	local result = {}
	for _, candidate in ipairs(candidates) do
		if candidate ~= "" and not seen[candidate] then
			seen[candidate] = true
			table.insert(result, candidate)
		end
	end

	return result
end

local function source_to_header_candidates(path)
	path = normalize_path(path)
	if path == "" then
		return {}
	end

	local ext = path:match("%.([^.]*)$")
	if not ext then
		return {}
	end

	local base = path:sub(1, -(#ext + 2))
	local candidates = {
		base .. ".h",
		base .. ".hpp",
		base .. ".hh",
		base .. ".hxx",
	}

	local mapped = path:gsub("/Private/", "/Public/")
	if mapped ~= path then
		local mapped_base = mapped:sub(1, -(#ext + 2))
		table.insert(candidates, 1, mapped_base .. ".h")
		table.insert(candidates, 2, mapped_base .. ".hpp")
	end

	local legacy = path:gsub("/Private/", "/Classes/")
	if legacy ~= path then
		local legacy_base = legacy:sub(1, -(#ext + 2))
		table.insert(candidates, 1, legacy_base .. ".h")
		table.insert(candidates, 2, legacy_base .. ".hpp")
	end

	local seen = {}
	local result = {}
	for _, candidate in ipairs(candidates) do
		if candidate ~= "" and not seen[candidate] then
			seen[candidate] = true
			table.insert(result, candidate)
		end
	end

	return result
end

local function resolve_existing_path(candidates)
	for _, candidate in ipairs(candidates or {}) do
		if vim.fn.filereadable(candidate) == 1 then
			return candidate
		end
	end

	return nil
end

local function base_unreal_function_name(name)
	name = tostring(name or "")
	name = name:gsub("_Implementation$", "")
	name = name:gsub("_Validate$", "")
	return name
end

local function extract_declaring_class(cursor_info)
	local class_name = tostring(cursor_info.class_name or "")
	if class_name ~= "" then
		return class_name
	end

	local full_text = tostring(cursor_info.full_text or "")
	return full_text:match("([A-Za-z_][%w_]*)::[~A-Za-z_][%w_]*%s*%(") or ""
end

local function implementation_target_names(cursor_info)
	local names = {}
	local seen = {}

	for _, item in ipairs(cursor_info.generated_definitions or {}) do
		local name = tostring(type(item) == "table" and item.name or "")
		if name ~= "" and not seen[name] then
			seen[name] = true
			table.insert(names, name)
		end
	end

	local fallback_name = tostring(cursor_info.name or "")
	if fallback_name ~= "" and not seen[fallback_name] then
		table.insert(names, fallback_name)
	end

	return names
end

local function declaration_target_names(cursor_info)
	local names = {}
	local seen = {}
	local current_name = tostring(cursor_info.name or "")
	local base_name = base_unreal_function_name(current_name)

	for _, name in ipairs({ base_name, current_name }) do
		if name ~= "" and not seen[name] then
			seen[name] = true
			table.insert(names, name)
		end
	end

	return names
end

local function normalize_cursor_info(value)
	if type(value) == "table" then
		return value
	end

	return {}
end

local function cursor_matches_function_name(cursor_info)
	local symbol = cursor_cword()
	if symbol == "" then
		return false
	end

	for _, name in ipairs(declaration_target_names(cursor_info)) do
		if symbol == name then
			return true
		end
	end

	for _, item in ipairs(cursor_info.generated_definitions or {}) do
		local generated_name = tostring(type(item) == "table" and item.name or "")
		if generated_name ~= "" and symbol == generated_name then
			return true
		end
	end

	return false
end

local function find_function_signature(lines, opts)
	local signature = normalize_space(opts.signature)
	local locator = opts.locator or opts.signature
	local locator_col_offset = tonumber(opts.locator_col_offset or 0) or 0
	local max_span = opts.max_span or 6

	if signature == "" or vim.tbl_isempty(lines or {}) then
		return nil
	end

	for start_line = 1, #lines do
		local combined = ""
		local limit = math.min(#lines, start_line + max_span - 1)
		for finish_line = start_line, limit do
			local current = tostring(lines[finish_line] or "")
			combined = combined == "" and current or (combined .. "\n" .. current)
			if normalize_space(combined):find(signature, 1, true) then
				for target_line = start_line, finish_line do
					local line_text = tostring(lines[target_line] or "")
					local col = line_text:find(locator, 1, true)
					if col then
						return {
							line = target_line,
							col = math.max(0, col - 1 + locator_col_offset),
						}
					end
				end

				return {
					line = start_line,
					col = 0,
				}
			end
		end
	end

	return nil
end

local function open_path_at(path, line, col)
	if not path or path == "" or vim.fn.filereadable(path) ~= 1 then
		return false
	end

	return open_result({
		file_path = path,
		line_number = line,
		col = col,
	}, { silent = true })
end

local function query_parse_buffer(root, file_path, line, character, callback)
	remote.query(root, {
		kind = "ParseBuffer",
		content = current_content(),
		file_path = normalize_path(file_path),
		line = line,
		character = character,
	}, callback)
end

local function try_counterpart_from_header(root, file_path, line, character, callback)
	if not is_header_file(file_path) then
		return callback(false)
	end

	query_parse_buffer(root, file_path, line, character, function(result, err)
		if err then
			return callback(false)
		end

		local cursor_info = normalize_cursor_info(type(result) == "table" and result.cursor_info or {})
		if tostring(cursor_info.kind or "") == "function_definition" then
			if not cursor_matches_function_name(cursor_info) then
				return callback(false)
			end
		end

		local class_name = extract_declaring_class(cursor_info)
		local params = tostring(cursor_info.parameters or "")
		if class_name == "" or params == "" then
			return callback(false)
		end

		local source_path = resolve_existing_path(header_to_source_candidates(file_path))
		if not source_path then
			return callback(false)
		end

		local lines = lines_for_path(source_path)
		for _, name in ipairs(implementation_target_names(cursor_info)) do
			local qualified_name = string.format("%s::%s", class_name, name)
			local match = find_function_signature(lines, {
				signature = qualified_name .. params,
				locator = qualified_name,
				locator_col_offset = #class_name + 2,
			})
			if match then
				return callback(open_path_at(source_path, match.line, match.col))
			end
		end

		callback(false)
	end)
end

local function try_counterpart_from_source(root, file_path, line, character, callback)
	if not is_source_file(file_path) then
		return callback(false)
	end

	query_parse_buffer(root, file_path, line, character, function(result, err)
		if err then
			return callback(false)
		end

		local cursor_info = normalize_cursor_info(type(result) == "table" and result.cursor_info or {})
		local full_text = tostring(cursor_info.full_text or "")
		if tostring(cursor_info.kind or "") ~= "function_definition" and not full_text:find("::", 1, true) then
			return callback(false)
		end
		if not cursor_matches_function_name(cursor_info) then
			return callback(false)
		end

		local class_name = extract_declaring_class(cursor_info)
		local params = tostring(cursor_info.parameters or "")
		if class_name == "" or params == "" then
			return callback(false)
		end

		local header_path = resolve_existing_path(source_to_header_candidates(file_path))
		if not header_path then
			return callback(false)
		end

		local lines = lines_for_path(header_path)
		for _, name in ipairs(declaration_target_names(cursor_info)) do
			local match = find_function_signature(lines, {
				signature = name .. params,
				locator = name,
			})
			if match then
				return callback(open_path_at(header_path, match.line, match.col))
			end
		end

		callback(false)
	end)
end

-- Read current buffer content as one string.
-- 读取当前 buffer 内容，并合并成一个字符串。
current_content = function()
	local lines = vim.api.nvim_buf_get_lines(0, 0, -1, false)
	return table.concat(lines, "\n") .. "\n"
end

-- Open one goto result returned by Rust.
-- 打开 Rust 返回的跳转结果。
open_result = function(result, opts)
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
		end
		return false
	end

	if vim.fn.filereadable(path) ~= 1 then
		if not opts.silent then
			vim.notify("UCore goto: file is not readable: " .. tostring(path), vim.log.levels.WARN)
		end
		return false
	end

	vim.cmd.edit(vim.fn.fnameescape(path))
	refresh_opened_buffer_filetype(path)

	local last_line = math.max(1, vim.api.nvim_buf_line_count(0))
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
	local line = cursor[1] - 1
	local character = cursor[2]

	try_counterpart_from_header(root, file_path, line, character, function(found)
		if found then
			vim.cmd("nohlsearch")
			return
		end

		remote.goto_implementation(root, {
			content = current_content(),
			line = line,
			character = character,
			file_path = normalize_path(file_path),
		}, function(result, err)
			if err then
				return vim.notify("UCore goto implementation failed:\n" .. tostring(err), vim.log.levels.ERROR)
			end
			vim.cmd("nohlsearch")
			open_result(result, { silent = true })
		end)
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

	local counterpart = is_header_file(file_path) and try_counterpart_from_header or try_counterpart_from_source
	counterpart(root, file_path, line, character, function(found)
		if found then
			vim.cmd("nohlsearch")
			return
		end

		remote.goto_definition(root, {
			content = current_content(),
			line = line,
			character = character,
			file_path = normalize_path(file_path),
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
		refresh_opened_buffer_filetype(alt)
	else
		vim.notify("UCore: no matching source/header file found", vim.log.levels.INFO)
	end
end

find_alternate_source = function(path)
	local normalized = normalize_path(path)
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
	require("ucore.commands.actions").find("")
end

return M
