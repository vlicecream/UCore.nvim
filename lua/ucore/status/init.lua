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

local clear_panel_contents
local dismiss_panel
local render
local bump_dismiss_version

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
		countdown_seconds = nil,
		manual_dismissed = false,
	}
end

local panels = {
	unreal_init = make_panel("UCore Unreal Init", "ucore.status.unreal_init", {
		"boot",
		"task:plugin",
		"task:asset_bridge",
	}),
	engine_init = make_panel("UCore Engine Index", "ucore.status.engine_init", {
		"boot",
		"progress:UCore Engine Discovery",
		"progress:UCore Engine DB Prepare",
		"progress:UCore Engine Analysis",
		"progress:UCore Engine DB Write",
		"progress:UCore Engine Text DB Write",
		"progress:UCore Engine Asset Scan",
		"progress:UCore Engine Asset Persist",
		"progress:UCore Engine Finalize",
	}),
	init = make_panel("UCore Workspace Init", "ucore.status.init", {
		"boot",
		"progress:UCore Server Start",
		"progress:UCore Server Ready",
		"progress:UCore Workspace Register",
		"progress:UCore Project Discovery",
		"progress:UCore Project DB Prepare",
		"progress:UCore Project Analysis",
		"progress:UCore Project DB Write",
		"progress:UCore Project Text DB Write",
		"progress:UCore Project Asset Scan",
		"progress:UCore Project Asset Persist",
		"progress:UCore Project Finalize",
	}),
}

local function panel_is_modal(panel)
	return false
end

local function panel_for_key(key)
	if type(key) == "string" and key:find("^progress:UCore Engine ", 1, true) == 1 then
		return panels.engine_init
	end

	return panels.init
end

local function panel_storage_key(panel)
	if panel == panels.unreal_init then
		return "unreal_init"
	end
	if panel == panels.engine_init then
		return "engine_init"
	end

	return "init"
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
	return panel_has_spinner_items(panels.unreal_init)
		or panel_has_spinner_items(panels.engine_init)
		or panel_has_spinner_items(panels.init)
end

local function spinner_frame()
	return spinner_frames[spinner_index] or spinner_frames[1]
end

local function split_message_lines(message)
	if message == nil then
		return {}
	end

	local lines = vim.split(tostring(message), "\n", { plain = true })
	if #lines == 0 then
		return { tostring(message) }
	end

	return lines
end

local function compact_message(message)
	local lines = split_message_lines(message)
	return lines[1] or ""
end

local function initialize_progress_placeholders(panel)
	for _, key in ipairs(panel.ordered_keys or {}) do
		if key:match("^progress:") and not panel.items[key] then
			local title = key:gsub("^progress:", "")
			panel.items[key] = string.format("%s 0%%", title)
		end
	end
end

local function render_message_lines(panel, key, message)
	local lines = split_message_lines(message)
	if #lines == 0 then
		return lines
	end

	if panel.spinner_active_keys[key] and lines[1] and lines[1] ~= "" then
		lines[1] = string.format("%s %s", lines[1], spinner_frame())
	end

	return lines
end

local function panel_lines(panel)
	local lines = {}
	local seen = {}

	for _, key in ipairs(panel.ordered_keys) do
		if panel.items[key] then
			vim.list_extend(lines, render_message_lines(panel, key, panel.items[key]))
			seen[key] = true
		end
	end

	for key, line in pairs(panel.items) do
		if not seen[key] then
			vim.list_extend(lines, render_message_lines(panel, key, line))
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

local function bind_modal_keys(panel_key, buf)
	if not vim.api.nvim_buf_is_valid(buf) then
		return
	end

	pcall(vim.keymap.del, "n", "<Esc>", { buffer = buf })
	pcall(vim.keymap.del, "n", "q", { buffer = buf })

	vim.keymap.set("n", "<Esc>", function()
		local panel = panels[panel_key]
		if not panel or panel.state == "running" or panel.boot_active then
			return
		end
		panel.manual_dismissed = true
		bump_dismiss_version(panel)
		clear_panel_contents(panel)
		dismiss_panel(panel)
		render()
	end, {
		buffer = buf,
		nowait = true,
		silent = true,
	})
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

local function init_modal_sections(panel)
	local items = panel_lines(panel)
	if #items == 0 then
		return {}
	end

	local status_line = "Please wait for init"
	if panel.state == "complete" and type(panel.countdown_seconds) == "number" then
		status_line = string.format("Closing in %ds", math.max(panel.countdown_seconds, 0))
	end

	local lines = {
		panel.title,
		"",
		status_line,
		"",
	}

	for _, line in ipairs(items) do
		if line ~= "" then
			table.insert(lines, line)
		end
	end

	local min_height = 18
	local body_target = math.max(min_height, #lines)
	while #lines < body_target do
		table.insert(lines, "")
	end

	return lines
end

local function init_modal_width(lines)
	local content_width = float_text_width(lines)
	local min_width = 98
	local max_width = math.max(vim.o.columns - 8, min_width)
	return math.min(math.max(content_width, min_width), max_width)
end

local function center_text(text, width)
	local display_width = vim.fn.strdisplaywidth(text)
	if display_width >= width then
		return text
	end

	local padding = math.floor((width - display_width) / 2)
	return string.rep(" ", padding) .. text
end

local function apply_init_modal_highlights(buf, lines, width)
	if #lines >= 1 then
		pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, "Title", 0, 0, -1)
	end

	if #lines >= 3 then
		pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, "Comment", 2, 0, -1)
	end

	for index = 5, #lines do
		local line = lines[index]
		if line:find("100%%", 1, true) then
			pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, "String", index - 1, 0, -1)
		elseif line:find("^Closing in ", 1, true) then
			pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, "Comment", index - 1, 0, -1)
		end
	end
end

local function render_float_panel(panel_key, panel, row)
	local lines
	local is_init_modal = panel_is_modal(panel)
	if is_init_modal then
		lines = init_modal_sections(panel)
	else
		lines = float_display_lines(panel)
	end
	if #lines == 0 then
		close_float(panel_key)
		close_float("init_footer")
		return 0
	end

	local width
	if is_init_modal then
		width = init_modal_width(lines)
	else
		width = math.min(float_text_width(lines), math.max(vim.o.columns - 4, 1))
	end
	local height = #lines
	local buf = ensure_float_buf(panel_key)
	local display_lines = lines
	if is_init_modal then
		display_lines = vim.deepcopy(lines)
		display_lines[1] = center_text(display_lines[1], width)
		display_lines[3] = center_text(display_lines[3], width)
	end
	vim.bo[buf].modifiable = true
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, display_lines)
	vim.api.nvim_buf_clear_namespace(buf, highlight_ns, 0, -1)
	vim.bo[buf].modifiable = false

	local config
	if is_init_modal then
		config = {
			relative = "editor",
			row = math.max(math.floor((vim.o.lines - height) / 2) - 1, 1),
			col = math.max(math.floor((vim.o.columns - width) / 2), 1),
			width = width,
			height = height,
			style = "minimal",
			focusable = false,
			noautocmd = true,
			border = "rounded",
			zindex = 260,
		}
	else
		config = {
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
	end

	local win = float_state.wins[panel_key]
	if win and vim.api.nvim_win_is_valid(win) then
		pcall(vim.api.nvim_win_set_config, win, config)
	else
		win = vim.api.nvim_open_win(buf, is_init_modal, config)
		float_state.wins[panel_key] = win
		vim.wo[win].winblend = 0
		vim.wo[win].wrap = false
		vim.wo[win].cursorline = false
	end

	if is_init_modal then
		bind_modal_keys(panel_key, buf)
		close_float("init_footer")
	end

	if is_init_modal then
		apply_init_modal_highlights(buf, display_lines, width)
	else
		local highlight = panel.state == "failed" and "DiagnosticError" or "Comment"
		for index, _ in ipairs(lines) do
			local prefix = panel.title .. ":"
			local prefix_len = #prefix
			pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, highlight, index - 1, 0, prefix_len)
		end
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
	local builtin_notify = uses_builtin_notify()
	if builtin_notify then
		local row = 1
		local init_height = render_float_panel("init", panels.init, row)
		if init_height > 0 then
			row = row + init_height + 1
		end
		local engine_height = render_float_panel("engine_init", panels.engine_init, row)
		if engine_height > 0 then
			row = row + engine_height + 1
		end
		local unreal_height = render_float_panel("unreal_init", panels.unreal_init, row)
		if unreal_height > 0 then
			row = row + unreal_height + 1
		end
		panels.init.notify_handle = nil
		panels.engine_init.notify_handle = nil
		panels.unreal_init.notify_handle = nil
	else
		close_float("init")
		close_float("engine_init")
		close_float("unreal_init")
		render_notify_panel(panels.init)
		render_notify_panel(panels.engine_init)
		render_notify_panel(panels.unreal_init)
	end
end

render = function()
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

dismiss_panel = function(panel)
	if uses_builtin_notify() then
		close_float(panel_storage_key(panel))
		if panel_is_modal(panel) then
			close_float("init_footer")
		end
		panel.notify_handle = nil
		return
	end

	if panel_is_modal(panel) then
		close_float(panel_storage_key(panel))
		close_float("init_footer")
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

clear_panel_contents = function(panel)
	panel.items = {}
	panel.spinner_active_keys = {}
	panel.boot_active = false
	panel.pending_finish_message = nil
	panel.countdown_seconds = nil
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

local function panel_updates_suppressed(panel)
	return panel == panels.init and panel.manual_dismissed == true
end

bump_dismiss_version = function(panel)
	panel.dismiss_version = (panel.dismiss_version or 0) + 1
end

local function is_complete_message(text)
	text = tostring(text or "")
	if text == "" then
		return false
	end

	return text:find("100%", 1, true) ~= nil or text:find("Skipped", 1, true) ~= nil
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

local function schedule_panel_dismiss(panel, delay_ms)
	if not panel.notify_handle and not float_state.wins[panel_storage_key(panel)] then
		return
	end

	local version = panel.dismiss_version
	vim.defer_fn(function()
		if panel.dismiss_version ~= version then
			return
		end

		if panel.state == "complete" and not panel.boot_active and not panel_has_spinner_items(panel) then
			clear_panel_contents(panel)
			dismiss_panel(panel)
		end
	end, delay_ms or 5000)
end

local function schedule_panel_countdown(panel, seconds)
	local version = panel.dismiss_version
	panel.countdown_seconds = seconds
	render()

	if seconds <= 0 then
		clear_panel_contents(panel)
		dismiss_panel(panel)
		render()
		return
	end

	vim.defer_fn(function()
		if panel.dismiss_version ~= version then
			return
		end
		if panel.state ~= "complete" or panel.boot_active or panel_has_spinner_items(panel) then
			return
		end
		schedule_panel_countdown(panel, seconds - 1)
	end, 1000)
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
	panel.manual_dismissed = false
	bump_dismiss_version(panel)
end

local function reset_all()
	reset_panel(panels.unreal_init)
	reset_panel(panels.engine_init)
	suppress_panel_keys(panels.engine_init)
	reset_panel(panels.init)
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
	schedule_panel_countdown(panel, 5)
end

function M.start(message)
	reset_panel(panels.engine_init)
	suppress_panel_keys(panels.engine_init)
	reset_panel(panels.init)
	panels.init.boot_active = true
	panels.init.state = "running"
	panels.init.spinner_active_keys.boot = true
	panels.init.items.boot = message or "UCore Initializing..."
	-- Do NOT pre-fill every phase as "X 0%". Phases should only appear
	-- once they actually emit progress, otherwise the panel shows a wall
	-- of 0% placeholders that aren't running yet (especially confusing
	-- with parallel refresh + background engine refresh, where many
	-- phases will never run sequentially anyway).
	-- 不再预填全部 phase 为 0%——避免显示一堆还没开始的占位行，
	-- 在并行 refresh + 后台 engine 场景尤其明显。
	render()
	schedule_spinner()
end

function M.finish(message)
	local panel = panels.init
	if panel_updates_suppressed(panel) then
		panel.boot_active = false
		panel.spinner_active_keys.boot = nil
		panel.pending_finish_message = nil
		return
	end
	panel.boot_active = false
	panel.spinner_active_keys.boot = nil
	panel.pending_finish_message = message or "UCore Ready - Initialization Complete"
	bump_dismiss_version(panel)
	apply_pending_init_finish()
end

function M.fail(message, detail)
	local panel = panels.init
	if panel_updates_suppressed(panel) then
		panel.boot_active = false
		panel.spinner_active_keys.boot = nil
		panel.pending_finish_message = nil
		return
	end
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
	if panel_updates_suppressed(panel) then
		return
	end
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
	if panel_updates_suppressed(panel) then
		return
	end
	local key = "progress:" .. title
	local text = compact_message(message or string.format("%s Complete", title))
	if should_ignore_suppressed_update(panel, key, text) then
		return
	end
	unsuppress_key(panel, key)
	panel.spinner_active_keys[key] = nil
	panel.items[key] = text
	bump_dismiss_version(panel)
	if panel == panels.engine_init and not panel_has_spinner_items(panel) then
		panel.state = "complete"
		schedule_panel_dismiss(panel, 5000)
	end
	render()
	if panel == panels.init then
		apply_pending_init_finish()
	end
end

function M.unreal_start(message)
	reset_panel(panels.unreal_init)
	panels.unreal_init.boot_active = true
	panels.unreal_init.state = "running"
	panels.unreal_init.spinner_active_keys.boot = true
	panels.unreal_init.items.boot = message or "Preparing Unreal editor integration..."
	render()
	schedule_spinner()
end

function M.unreal_finish(message)
	local panel = panels.unreal_init
	panel.boot_active = false
	panel.spinner_active_keys.boot = nil
	panel.pending_finish_message = message or "UCore Unreal Init Complete"
	bump_dismiss_version(panel)
	if not panel_has_spinner_items(panel) then
		local text = panel.pending_finish_message
		panel.pending_finish_message = nil
		panel.state = "complete"
		panel.items.boot = text
		render()
		schedule_panel_dismiss(panel, 5000)
	end
end

function M.unreal_fail(message, detail)
	local panel = panels.unreal_init
	panel.boot_active = false
	panel.state = "failed"
	panel.pending_finish_message = nil
	panel.spinner_active_keys.boot = nil
	local text = message or "UCore Unreal Init Failed"
	if detail and detail ~= "" then
		text = text .. " | " .. tostring(detail)
	end
	panel.items.boot = text
	bump_dismiss_version(panel)
	render()
end

function M.unreal_step(key, message)
	local panel = panels.unreal_init
	panel.spinner_active_keys[key] = true
	panel.items[key] = message
	panel.state = "running"
	bump_dismiss_version(panel)
	render()
	schedule_spinner()
end

function M.unreal_step_finish(key, message)
	local panel = panels.unreal_init
	panel.spinner_active_keys[key] = nil
	panel.items[key] = message
	bump_dismiss_version(panel)
	if panel.pending_finish_message and not panel.boot_active and not panel_has_spinner_items(panel) then
		local text = panel.pending_finish_message
		panel.pending_finish_message = nil
		panel.state = "complete"
		panel.items.boot = text
		render()
		schedule_panel_dismiss(panel, 5000)
		return
	end
	render()
end

function M.unreal_clear(key)
	local panel = panels.unreal_init
	panel.spinner_active_keys[key] = nil
	panel.items[key] = nil
	bump_dismiss_version(panel)
	render()
end

function M.progress_fail(title, message)
	local panel = panel_for_key("progress:" .. title)
	if panel_updates_suppressed(panel) then
		return
	end
	local key = "progress:" .. title
	panel.spinner_active_keys[key] = nil
	panel.state = "failed"
	panel.items[key] = message
	bump_dismiss_version(panel)
	render()
end

function M.clear(key)
	local panel = panel_for_key(key)
	if panel_updates_suppressed(panel) then
		return
	end
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
