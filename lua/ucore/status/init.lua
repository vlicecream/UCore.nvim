local M = {}

local items = {}
local boot_active = false
local state = "running"
local notify_handle = nil
local notify_id = "ucore.status"

local ordered_keys = {
	"boot",
	"progress:UCore other initialization",
	"progress:UCore project index",
	"progress:UCore engine index",
}

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
local function render()
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
-- 标记初始化失败，并保留方便排查。
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
