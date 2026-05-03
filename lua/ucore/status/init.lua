local M = {}

local spinner_frames = { "⣾", "⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽" }
local spinner_index = 1
local spinner_scheduled = false

local function uses_builtin_notify()
	local info = debug.getinfo(vim.notify, "S")
	local source = tostring(info and info.source or "")
	return source:find("vim/_core/editor.lua", 1, true) ~= nil
end

local function parse_percent(text)
	local best = nil
	for token in tostring(text or ""):gmatch("(%d?%d?%d)%%") do
		local value = tonumber(token)
		if value and value >= 0 and value <= 100 then
			best = best and math.max(best, value) or value
		end
	end
	return best
end

local function make_panel(title, notify_id, ordered_keys)
	return {
		title = title,
		notify_id = notify_id,
		ordered_keys = ordered_keys,
		items = {},
		suppressed_keys = {},
		spinner_active_keys = {},
		notify_handle = nil,
		progress_id = nil,
		boot_active = false,
		state = "running",
		pending_finish_message = nil,
		dismiss_version = 0,
	}
end

local panels = {
	init = make_panel("UCore Workspace Init", "ucore.status.init", {
		"boot",
		"progress:UCore Other Initialization",
		"progress:UCore Syntax Highlight",
		"progress:UCore Project Index",
		"progress:UCore Engine Index",
	}),
	clang = make_panel("UCore Clang Init", "ucore.status.clang", {
		"progress:UCore Clangd Database",
		"progress:UCore Clangd Index",
	}),
}

local function panel_for_key(key)
	if key == "boot" then
		return panels.init
	end

	local lower = tostring(key or ""):lower()
	if lower:find("clangd", 1, true) then
		return panels.clang
	end

	return panels.init
end

local function panel_has_spinner_items(panel)
	for key, active in pairs(panel.spinner_active_keys) do
		if active and panel.items[key] then
			return true
		end
	end

	return false
end

local function any_spinner_items()
	return panel_has_spinner_items(panels.init)
		or panel_has_spinner_items(panels.clang)
end

local function spinner_frame()
	return spinner_frames[spinner_index] or spinner_frames[1]
end

local function render_line(panel, key, message)
	if panel.spinner_active_keys[key] and message and message ~= "" then
		return string.format("%s %s", message, spinner_frame())
	end

	return message
end

local function render_panel(panel)
	local lines = {}
	local seen = {}

	for _, key in ipairs(panel.ordered_keys) do
		if panel.items[key] then
			table.insert(lines, render_line(panel, key, panel.items[key]))
			seen[key] = true
		end
	end

	for key, line in pairs(panel.items) do
		if not seen[key] then
			table.insert(lines, render_line(panel, key, line))
		end
	end

	if #lines == 0 then
		panel.progress_id = nil
		if panel.notify_handle then
			pcall(vim.notify, "", vim.log.levels.INFO, {
				id = panel.notify_id,
				title = panel.title,
				replace = panel.notify_handle,
				timeout = 1,
			})
		end
		panel.notify_handle = nil
		return
	end

	if uses_builtin_notify() then
		local percent = panel.state == "complete" and 100 or nil
		if percent == nil then
			for _, line in ipairs(lines) do
				local value = parse_percent(line)
				if value then
					percent = percent and math.max(percent, value) or value
				end
			end
		end

		local status = "running"
		if panel.state == "failed" then
			status = "failed"
		elseif panel.state == "complete" then
			status = "success"
		end

		local ok, id = pcall(vim.api.nvim_echo, {
			{ table.concat(lines, " | "), "Normal" },
		}, false, {
			id = panel.progress_id,
			kind = "progress",
			title = panel.title,
			status = status,
			percent = percent,
			source = panel.notify_id,
		})

		if ok and id then
			panel.progress_id = id
		end
		return
	end

	local level = panel.state == "failed" and vim.log.levels.ERROR or vim.log.levels.INFO
	local ok, handle = pcall(vim.notify, table.concat(lines, "\n"), level, {
		id = panel.notify_id,
		title = panel.title,
		replace = panel.notify_handle,
		timeout = false,
	})

	if ok and handle then
		panel.notify_handle = handle
	end
end

local function render()
	render_panel(panels.init)
	render_panel(panels.clang)
end

local function dismiss_panel(panel)
	panel.progress_id = nil
	if uses_builtin_notify() then
		return
	end

	if not panel.notify_handle then
		return
	end

	pcall(vim.notify, "", vim.log.levels.INFO, {
		id = panel.notify_id,
		title = panel.title,
		replace = panel.notify_handle,
		timeout = 1,
	})
	panel.notify_handle = nil
end

local function clear_panel_contents(panel)
	panel.items = {}
	panel.spinner_active_keys = {}
	panel.boot_active = false
	panel.pending_finish_message = nil
end

local function suppress_panel_keys(panel)
	panel.suppressed_keys = {}
	for _, key in ipairs(panel.ordered_keys or {}) do
		panel.suppressed_keys[key] = true
	end
end

local function unsuppress_key(panel, key)
	panel.suppressed_keys[key] = nil
end

local function bump_dismiss_version(panel)
	panel.dismiss_version = (panel.dismiss_version or 0) + 1
end

local function is_complete_message(text)
	text = tostring(text or "")
	if text == "" then
		return false
	end

	return text:find("100%%", 1, false) ~= nil or text:find("Skipped", 1, true) ~= nil
end

local function is_terminal_message(text)
	text = tostring(text or "")
	if text == "" then
		return false
	end

	local lower = text:lower()
	return is_complete_message(text)
		or lower:find("ready", 1, true) ~= nil
		or lower:find("idle", 1, true) ~= nil
end

local function should_ignore_suppressed_update(panel, key, message)
	if not panel.suppressed_keys[key] then
		return false
	end

	return is_terminal_message(message)
end

local function clang_panel_ready(panel)
	for _, key in ipairs(panel.ordered_keys) do
		if not panel.items[key] or not is_complete_message(panel.items[key]) then
			return false
		end
	end

	return not panel_has_spinner_items(panel)
end

local function schedule_panel_dismiss(panel, delay_ms)
	if not panel.notify_handle then
		return
	end

	local version = panel.dismiss_version
	vim.defer_fn(function()
		if panel.dismiss_version ~= version then
			return
		end

		if panel == panels.init then
			if panel.state == "complete" and not panel.boot_active and not panel_has_spinner_items(panel) then
				clear_panel_contents(panel)
				dismiss_panel(panel)
			end
			return
		end

		if panel == panels.clang and clang_panel_ready(panel) then
			suppress_panel_keys(panel)
			clear_panel_contents(panel)
			dismiss_panel(panel)
		end
	end, delay_ms or 5000)
end

local function schedule_spinner()
	if spinner_scheduled or not any_spinner_items() then
		return
	end

	spinner_scheduled = true
	vim.defer_fn(function()
		spinner_scheduled = false
		if not any_spinner_items() then
			return
		end

		spinner_index = (spinner_index % #spinner_frames) + 1
		render()
		schedule_spinner()
	end, 120)
end

local function reset_panel(panel)
	dismiss_panel(panel)
	clear_panel_contents(panel)
	panel.suppressed_keys = {}
	panel.state = "running"
	panel.progress_id = nil
	bump_dismiss_version(panel)
end

local function reset_all()
	reset_panel(panels.init)
	reset_panel(panels.clang)
	render()
end

local function apply_pending_init_finish()
	local panel = panels.init
	if not panel.pending_finish_message or panel.boot_active or panel_has_spinner_items(panel) then
		return
	end

	local text = panel.pending_finish_message
	panel.pending_finish_message = nil
	panel.state = "complete"
	panel.items.boot = text
	bump_dismiss_version(panel)
	render()
	schedule_panel_dismiss(panel, 5000)
end

function M.start(message)
	reset_panel(panels.init)
	panels.init.boot_active = true
	panels.init.state = "running"
	panels.init.spinner_active_keys.boot = true
	panels.init.items.boot = message or "UCore Initializing..."
	render()
	schedule_spinner()
end

function M.finish(message)
	local panel = panels.init
	panel.boot_active = false
	panel.spinner_active_keys.boot = nil
	panel.pending_finish_message = message or "UCore Ready - Initialization Complete"
	bump_dismiss_version(panel)
	apply_pending_init_finish()
end

function M.fail(message, detail)
	local panel = panels.init
	panel.boot_active = false
	panel.state = "failed"
	panel.pending_finish_message = nil
	panel.spinner_active_keys.boot = nil
	local text = message or "UCore Initialization Failed"
	if detail and detail ~= "" then
		text = text .. " | " .. tostring(detail)
	end

	panel.items.boot = text
	bump_dismiss_version(panel)
	render()
end

function M.progress(title, message)
	local panel = panel_for_key("progress:" .. title)
	local key = "progress:" .. title
	if should_ignore_suppressed_update(panel, key, message) then
		return
	end
	unsuppress_key(panel, key)
	panel.spinner_active_keys[key] = true
	panel.items[key] = message
	panel.state = "running"
	bump_dismiss_version(panel)
	render()
	schedule_spinner()
end

function M.progress_finish(title, message)
	local panel = panel_for_key("progress:" .. title)
	local key = "progress:" .. title
	local text = message or string.format("%s Complete", title)
	if should_ignore_suppressed_update(panel, key, text) then
		return
	end
	unsuppress_key(panel, key)
	panel.spinner_active_keys[key] = nil
	panel.items[key] = text
	bump_dismiss_version(panel)
	render()
	if panel == panels.init then
		apply_pending_init_finish()
	elseif (panel == panels.clang or panel == panels.debug) and clang_panel_ready(panel) then
		panel.state = "complete"
		schedule_panel_dismiss(panel, 5000)
	end
end

function M.progress_fail(title, message)
	local panel = panel_for_key("progress:" .. title)
	local key = "progress:" .. title
	panel.spinner_active_keys[key] = nil
	panel.state = "failed"
	panel.items[key] = message
	bump_dismiss_version(panel)
	render()
end

function M.clear(key)
	local panel = panel_for_key(key)
	if panel.suppressed_keys[key] then
		return
	end
	panel.spinner_active_keys[key] = nil
	panel.items[key] = nil
	bump_dismiss_version(panel)
	render()
	if panel == panels.init then
		apply_pending_init_finish()
	end
end

function M.clear_all()
	reset_all()
end

return M
