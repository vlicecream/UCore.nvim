local M = {}

local items = {}
local boot_active = false
local state = "running"
local notify_handle = nil
local notify_id = "ucore.status"
local pending_finish_message = nil
local spinner_frames = { "", "", "", "", "", "", "", "", "", "", "", "" }
local spinner_index = 1
local spinner_active_keys = {}
local spinner_scheduled = false
local render

local ordered_keys = {
	"boot",
	"progress:UCore Other Initialization",
	"progress:UCore Clangd Database",
	"progress:UCore Syntax Highlight",
	"progress:UCore Project Index",
	"progress:UCore Engine Index",
	"progress:UCore Clangd Index",
}

local function has_spinner_items()
	for key, active in pairs(spinner_active_keys) do
		if active and items[key] then
			return true
		end
	end

	return false
end

local function spinner_frame()
	return spinner_frames[spinner_index] or spinner_frames[1]
end

local function render_line(key, message)
	if spinner_active_keys[key] and message and message ~= "" then
		return string.format("%s %s", message, spinner_frame())
	end

	return message
end

local function clear_progress_items()
	for key, _ in pairs(items) do
		if key:match("^progress:") then
			items[key] = nil
			spinner_active_keys[key] = nil
		end
	end
end

local function maybe_clear_completed_panel()
	if state ~= "complete" or boot_active or has_spinner_items() then
		return
	end

	local boot_text = items.boot
	if not boot_text then
		return
	end

	vim.defer_fn(function()
		if state == "complete" and not boot_active and not has_spinner_items() and items.boot == boot_text then
			M.clear_all()
		end
	end, 5000)
end

local function maybe_apply_pending_finish()
	if not pending_finish_message or boot_active or has_spinner_items() then
		return
	end

	state = "complete"
	clear_progress_items()
	items.boot = pending_finish_message
	pending_finish_message = nil
	render()
	maybe_clear_completed_panel()
end

-- Collect visible status lines in stable user-facing order.
-- 按稳定的用户可见顺序收集状态行。
local function collect_lines()
	local lines = {}
	local seen = {}

	for _, key in ipairs(ordered_keys) do
		if items[key] then
			table.insert(lines, render_line(key, items[key]))
			seen[key] = true
		end
	end

	for key, line in pairs(items) do
		if not seen[key] then
			table.insert(lines, render_line(key, line))
		end
	end

	return lines
end

local function notify_level()
	if state == "complete" then
		return vim.log.levels.INFO
	end

	if state == "failed" then
		return vim.log.levels.ERROR
	end

	return vim.log.levels.INFO
end

local function notify_title()
	if state == "complete" then
		return "UCore Ready"
	end

	return "UCore"
end

-- Render the current status lines as one replaceable notification.
-- 将当前状态渲染成一条可替换的通知，交给 noice/notify 管理位置。
function render()
	local lines = collect_lines()

	if #lines == 0 then
		if notify_handle then
			pcall(vim.notify, "", vim.log.levels.INFO, {
				id = notify_id,
				title = notify_title(),
				replace = notify_handle,
				timeout = 1,
			})
		end
		notify_handle = nil
		return
	end

	local ok, handle = pcall(vim.notify, table.concat(lines, "\n"), notify_level(), {
		id = notify_id,
		title = notify_title(),
		replace = notify_handle,
		timeout = false,
	})

	if ok and handle then
		notify_handle = handle
	end
end

local function schedule_spinner()
	if spinner_scheduled or not has_spinner_items() then
		return
	end

	spinner_scheduled = true
	vim.defer_fn(function()
		spinner_scheduled = false
		if not has_spinner_items() then
			return
		end

		spinner_index = (spinner_index % #spinner_frames) + 1
		render()
		schedule_spinner()
	end, 120)
end

local function reset_notification()
	items = {}
	boot_active = false
	state = "running"
	pending_finish_message = nil
	spinner_active_keys = {}
	render()
end

-- Set or replace one status line.
-- 设置或替换一条状态行。
function M.set(key, message)
	items[key] = message
	render()
	schedule_spinner()
end

-- Clear one status line.
-- 清理一条状态行。
function M.clear(key)
	spinner_active_keys[key] = nil
	items[key] = nil
	render()
	maybe_apply_pending_finish()
	maybe_clear_completed_panel()
end

-- Clear the whole status panel.
-- 清理整个状态面板。
function M.clear_all()
	reset_notification()
end

-- Start a persistent initialization status.
-- 开始一条不会自动消失的初始化状态。
function M.start(message)
	reset_notification()
	boot_active = true
	state = "running"
	spinner_active_keys.boot = true
	M.set("boot", message or "UCore Initializing...")
end

-- Mark initialization as complete and hide the whole panel shortly after.
-- 标记初始化完成，并在短暂显示后隐藏整个面板。
function M.finish(message)
	boot_active = false
	spinner_active_keys.boot = nil
	pending_finish_message = message or "UCore Ready - Initialization Complete"
	maybe_apply_pending_finish()
end

-- Mark initialization as failed and keep the error visible.
-- 标记初始化失败，并保留方便排查。
function M.fail(message, detail)
	boot_active = false
	state = "failed"
	pending_finish_message = nil
	spinner_active_keys.boot = nil
	local text = message or "UCore Initialization Failed"
	if detail and detail ~= "" then
		text = text .. " | " .. tostring(detail)
	end

	M.set("boot", text)
end

-- Update a progress line.
-- 更新一条进度状态。
function M.progress(title, message)
	local key = "progress:" .. title
	spinner_active_keys[key] = true
	M.set(key, message)
end

-- Complete a progress line and hide it shortly after.
-- 完成一条进度状态，并在短暂显示后隐藏。
function M.progress_finish(title, message)
	local key = "progress:" .. title
	spinner_active_keys[key] = nil
	local text = message or string.format("%s Complete", title)
	if boot_active then
		M.set(key, text)
		return
	end

	M.clear(key)
end

-- Mark a progress line as failed and keep it visible.
-- 标记一条进度失败，并保留方便排查。
function M.progress_fail(title, message)
	local key = "progress:" .. title
	spinner_active_keys[key] = nil
	M.set(key, message)
end

return M
