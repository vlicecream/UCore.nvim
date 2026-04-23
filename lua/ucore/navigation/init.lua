local project = require("ucore.project")
local remote = require("ucore.remote")

local M = {}

-- Read current buffer content as one string.
-- 读取当前 buffer 内容，并合并成一个字符串。
local function current_content()
	local lines = vim.api.nvim_buf_get_lines(0, 0, -1, false)
	return table.concat(lines, "\n") .. "\n"
end

-- Open one goto result returned by Rust.
-- 打开 Rust 返回的跳转结果。
local function open_result(result)
	if not result or result == vim.NIL or vim.tbl_isempty(result) then
		vim.notify("UCore goto: definition not found", vim.log.levels.WARN)
		return
	end

	local path = result.file_path or result.path
	local raw_line = result.line_number or result.line or result.row
	local line = tonumber(raw_line)
	local source = result.source and (" [" .. tostring(result.source) .. "]") or ""

	if not path or path == vim.NIL or path == "" then
		vim.notify("UCore goto: result has no file path" .. source, vim.log.levels.WARN)
		print(vim.inspect(result))
		return
	end

	if vim.fn.filereadable(path) ~= 1 then
		vim.notify("UCore goto: file is not readable: " .. tostring(path), vim.log.levels.WARN)
		print(vim.inspect(result))
		return
	end

	vim.cmd.edit(vim.fn.fnameescape(path))

	local last_line = vim.api.nvim_buf_line_count(0)
	line = line or 1

	-- Rust DB stores line numbers as 1-based in most tables.
	-- Rust DB 大多数表里的行号是 1-based。
	line = math.max(1, math.min(line, last_line))

	vim.api.nvim_win_set_cursor(0, { line, 0 })
	vim.cmd("normal! zz")

	if source ~= "" then
		vim.notify("UCore goto" .. source .. ": " .. tostring(path) .. ":" .. tostring(line), vim.log.levels.INFO)
	end
end

-- Go to definition at the current cursor position.
-- 跳转当前光标下符号的定义。
function M.goto_definition()
	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	local file_path = vim.api.nvim_buf_get_name(0)
	if file_path == "" then
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
			return vim.notify("UCore goto failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end

		open_result(result)
	end)
end

return M
