local M = {}

local buf = nil
local win = nil
local items = {}
local boot_active = false
local state = "running"

local ordered_keys = {
	"boot",
	"progress:UCore other initialization",
	"progress:UCore project index",
	"progress:UCore engine index",
}

-- Return true when the floating status window is still valid.
-- 判断顶部状态浮窗是否仍然有效。
local function win_valid()
	return win and vim.api.nvim_win_is_valid(win)
end

-- Return true when the backing buffer is still valid.
-- 判断状态浮窗使用的 buffer 是否仍然有效。
local function buf_valid()
	return buf and vim.api.nvim_buf_is_valid(buf)
end

-- Collect visible status lines in stable user-facing order.
-- 按稳定的用户可见顺序收集状态行。
local function collect_lines()
	local lines = {}
	local seen = {}

	for _, key in ipairs(ordered_keys) do
		if items[key] then
			table.insert(lines, items[key])
			seen[key] = true
		end
	end

	for key, line in pairs(items) do
		if not seen[key] then
			table.insert(lines, line)
		end
	end

	return lines
end

-- Create the scratch buffer used by the floating status window.
-- 创建状态浮窗使用的临时 buffer。
local function ensure_buf()
	if buf_valid() then
		return
	end

	buf = vim.api.nvim_create_buf(false, true)
	vim.bo[buf].bufhidden = "wipe"
	vim.bo[buf].buftype = "nofile"
	vim.bo[buf].swapfile = false
end

-- Compute a compact top-right window size.
-- 计算紧凑的右上角窗口尺寸。
local function window_config(lines)
	local max_width = 0
	for _, line in ipairs(lines) do
		max_width = math.max(max_width, vim.fn.strdisplaywidth(line))
	end

	local columns = vim.o.columns
	local width = math.min(math.max(max_width + 4, 42), math.max(columns - 8, 20))
	local height = math.max(#lines, 1)

	return {
		relative = "editor",
		anchor = "NW",
		row = 1,
		col = math.max(columns - width - 2, 0),
		width = width,
		height = height,
		style = "minimal",
		border = "rounded",
		title = state == "complete" and " UCore Ready " or " UCore ",
		title_pos = "center",
		focusable = false,
		zindex = 250,
	}
end

-- Apply visual styling for the current state.
-- 根据当前状态应用浮窗样式。
local function apply_window_style()
	if not win_valid() then
		return
	end

	if state == "complete" then
		vim.wo[win].winhl = "Normal:NormalFloat,FloatBorder:DiagnosticOk,FloatTitle:DiagnosticOk"
	elseif state == "failed" then
		vim.wo[win].winhl = "Normal:NormalFloat,FloatBorder:DiagnosticError,FloatTitle:DiagnosticError"
	else
		vim.wo[win].winhl = "Normal:NormalFloat,FloatBorder:FloatBorder,FloatTitle:FloatTitle"
	end
end

-- Close the floating status window if there is nothing to show.
-- 没有任何状态需要展示时关闭浮窗。
local function close_if_empty(lines)
	if #lines > 0 then
		return false
	end

	if win_valid() then
		vim.api.nvim_win_close(win, true)
	end

	win = nil
	return true
end

-- Render the current status lines into one reusable top floating window.
-- 将当前状态渲染到一个可复用的顶部浮窗里。
local function render()
	local lines = collect_lines()
	if close_if_empty(lines) then
		return
	end

	ensure_buf()
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)

	local config = window_config(lines)
	if win_valid() then
		vim.api.nvim_win_set_config(win, config)
	else
		win = vim.api.nvim_open_win(buf, false, config)
		vim.wo[win].winblend = 0
		vim.wo[win].wrap = false
	end

	apply_window_style()
end

-- Set or replace one status line.
-- 设置或替换一条状态行。
function M.set(key, message)
	items[key] = message
	render()
end

-- Clear one status line.
-- 清理一条状态行。
function M.clear(key)
	items[key] = nil
	render()
end

-- Clear the whole status panel.
-- 清理整个状态面板。
function M.clear_all()
	items = {}
	boot_active = false
	state = "running"
	render()
end

-- Start a persistent initialization status.
-- 开始一条不会自动消失的初始化状态。
function M.start(message)
	boot_active = true
	state = "running"
	M.set("boot", message or "UCore initializing...")
end

-- Mark initialization as complete and hide the whole panel shortly after.
-- 标记初始化完成，并在短暂显示后隐藏整个面板。
function M.finish(message)
	state = "complete"
	local text = message or "UCore READY - initialization complete"
	M.set("boot", text)

	vim.defer_fn(function()
		if items.boot == text then
			M.clear_all()
		end
	end, 5000)
end

-- Mark initialization as failed and keep the error visible.
-- 标记初始化失败，并保留错误方便排查。
function M.fail(message, detail)
	boot_active = false
	state = "failed"
	local text = message or "UCore initialization failed"
	if detail and detail ~= "" then
		text = text .. " | " .. tostring(detail)
	end

	M.set("boot", text)
end

-- Update a progress line.
-- 更新一条进度状态。
function M.progress(title, message)
	M.set("progress:" .. title, message)
end

-- Complete a progress line and hide it shortly after.
-- 完成一条进度状态，并在短暂显示后隐藏。
function M.progress_finish(title, message)
	local key = "progress:" .. title
	local text = message or string.format("%s complete", title)
	M.set(key, text)

	if boot_active then
		return
	end

	vim.defer_fn(function()
		if items[key] == text then
			M.clear(key)
		end
	end, 5000)
end

-- Mark a progress line as failed and keep it visible.
-- 标记一条进度失败，并保留方便排查。
function M.progress_fail(title, message)
	M.set("progress:" .. title, message)
end

return M
