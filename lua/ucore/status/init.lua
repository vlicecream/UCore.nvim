local M = {}

local spinner_frames = { "⣾", "⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽" }
local spinner_index = 1
local spinner_scheduled = false
local render_scheduled = false
local highlight_ns = vim.api.nvim_create_namespace("ucore.status.float")

local float_state = {
	bufs = {},
	wins = {},
}

local function uses_builtin_notify()
	local info = debug.getinfo(vim.notify, "S")
	local source = tostring(info and info.source or "")
	return source:find("vim/_core/editor.lua", 1, true) ~= nil
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
	clang = make_panel("Clangd Init", "ucore.status.clang", {
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

local function panel_lines(panel)
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

	return lines
end

local function close_float(panel_key)
	local win = float_state.wins[panel_key]
	if win and vim.api.nvim_win_is_valid(win) then
		pcall(vim.api.nvim_win_close, win, true)
	end
	float_state.wins[panel_key] = nil

	local buf = float_state.bufs[panel_key]
	if buf and vim.api.nvim_buf_is_valid(buf) then
		pcall(vim.api.nvim_buf_delete, buf, { force = true })
	end
	float_state.bufs[panel_key] = nil
end

local function ensure_float_buf(panel_key)
	local buf = float_state.bufs[panel_key]
	if buf and vim.api.nvim_buf_is_valid(buf) then
		return buf
	end

	buf = vim.api.nvim_create_buf(false, true)
	vim.bo[buf].bufhidden = "wipe"
	vim.bo[buf].buftype = "nofile"
	vim.bo[buf].swapfile = false
	float_state.bufs[panel_key] = buf
	return buf
end

local function float_text_width(lines)
	local width = 0
	for _, line in ipairs(lines) do
		width = math.max(width, vim.fn.strdisplaywidth(line))
	end
	return math.max(width, 1)
end

local function float_display_lines(panel)
	local lines = panel_lines(panel)
	if #lines == 0 then
		return lines
	end

	lines[1] = string.format("%s: %s", panel.title, lines[1])
	for index = 2, #lines do
		lines[index] = string.format("%s  %s", panel.title, lines[index])
	end
	return lines
end

local function render_float_panel(panel_key, panel, row)
	local lines = float_display_lines(panel)
	if #lines == 0 then
		close_float(panel_key)
		return 0
	end

	local width = math.min(float_text_width(lines), math.max(vim.o.columns - 4, 1))
	local height = #lines
	local buf = ensure_float_buf(panel_key)
	vim.bo[buf].modifiable = true
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
	vim.api.nvim_buf_clear_namespace(buf, highlight_ns, 0, -1)
	vim.bo[buf].modifiable = false

	local config = {
		relative = "editor",
		anchor = "NE",
		row = row,
		col = vim.o.columns - 1,
		width = width,
		height = height,
		style = "minimal",
		focusable = false,
		noautocmd = true,
		zindex = 250,
	}

	local win = float_state.wins[panel_key]
	if win and vim.api.nvim_win_is_valid(win) then
		pcall(vim.api.nvim_win_set_config, win, config)
	else
		win = vim.api.nvim_open_win(buf, false, config)
		float_state.wins[panel_key] = win
		vim.wo[win].winblend = 0
		vim.wo[win].wrap = false
		vim.wo[win].cursorline = false
	end

	local highlight = panel.state == "failed" and "DiagnosticError" or "Comment"
	for index, _ in ipairs(lines) do
		local prefix = panel.title .. ":"
		local prefix_len = #prefix
		pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, highlight, index - 1, 0, prefix_len)
	end

	return height
end

local function render_notify_panel(panel)
	local lines = panel_lines(panel)
	if #lines == 0 then
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

local function render_now()
	if uses_builtin_notify() then
		local row = 1
		row = row + render_float_panel("init", panels.init, row)
		if panels.init and #panel_lines(panels.init) > 0 and #panel_lines(panels.clang) > 0 then
			row = row + 1
		end
		render_float_panel("clang", panels.clang, row)
		panels.init.notify_handle = nil
		panels.clang.notify_handle = nil
		return
	end

	close_float("init")
	close_float("clang")
	render_notify_panel(panels.init)
	render_notify_panel(panels.clang)
end

local function render()
	if vim.in_fast_event() then
		if render_scheduled then
			return
		end
		render_scheduled = true
		vim.schedule(function()
			render_scheduled = false
			render_now()
		end)
		return
	end

	render_now()
end

local function dismiss_panel(panel)
	if uses_builtin_notify() then
		if panel == panels.init then
			close_float("init")
		elseif panel == panels.clang then
			close_float("clang")
		end
		panel.notify_handle = nil
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
