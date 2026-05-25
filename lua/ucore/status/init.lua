local M = {}

local spinner_frames = { "⣾", "⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽" }
local spinner_index = 1
local spinner_scheduled = false
local render_scheduled = false
local highlight_ns = vim.api.nvim_create_namespace("ucore.status.float")

local float_state = {
	buf = nil,
	win = nil,
}

local function uses_builtin_notify()
	local info = debug.getinfo(vim.notify, "S")
	local source = tostring(info and info.source or "")
	return source:find("vim/_core/editor.lua", 1, true) ~= nil
end

local ordered_keys = {
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
	"progress:UCore Engine Discovery",
	"progress:UCore Engine DB Prepare",
	"progress:UCore Engine Analysis",
	"progress:UCore Engine DB Write",
	"progress:UCore Engine Text DB Write",
	"progress:UCore Engine Asset Scan",
	"progress:UCore Engine Asset Persist",
	"progress:UCore Engine Finalize",
	"task:plugin",
	"task:asset_bridge",
}

local section_specs = {
	{
		title = "Workspace",
		keys = {
			"progress:UCore Server Start",
			"progress:UCore Server Ready",
			"progress:UCore Workspace Register",
		},
	},
	{
		title = "Project",
		keys = {
			"progress:UCore Project Discovery",
			"progress:UCore Project DB Prepare",
			"progress:UCore Project Analysis",
			"progress:UCore Project DB Write",
			"progress:UCore Project Text DB Write",
			"progress:UCore Project Asset Scan",
			"progress:UCore Project Asset Persist",
			"progress:UCore Project Finalize",
		},
	},
	{
		title = "Engine",
		keys = {
			"progress:UCore Engine Discovery",
			"progress:UCore Engine DB Prepare",
			"progress:UCore Engine Analysis",
			"progress:UCore Engine DB Write",
			"progress:UCore Engine Text DB Write",
			"progress:UCore Engine Asset Scan",
			"progress:UCore Engine Asset Persist",
			"progress:UCore Engine Finalize",
		},
	},
	{
		title = "Unreal Integration",
		keys = {
			"task:plugin",
			"task:asset_bridge",
		},
	},
}

local label_map = {
	["progress:UCore Server Start"] = "Server Start",
	["progress:UCore Server Ready"] = "Server Ready",
	["progress:UCore Workspace Register"] = "Workspace Register",
	["progress:UCore Project Discovery"] = "Project Discovery",
	["progress:UCore Project DB Prepare"] = "Project DB Prepare",
	["progress:UCore Project Analysis"] = "Project Analysis",
	["progress:UCore Project DB Write"] = "Project DB Write",
	["progress:UCore Project Text DB Write"] = "Project Text DB Write",
	["progress:UCore Project Asset Scan"] = "Project Asset Scan",
	["progress:UCore Project Asset Persist"] = "Project Asset Persist",
	["progress:UCore Project Finalize"] = "Project Finalize",
	["progress:UCore Engine Discovery"] = "Engine Discovery",
	["progress:UCore Engine DB Prepare"] = "Engine DB Prepare",
	["progress:UCore Engine Analysis"] = "Engine Analysis",
	["progress:UCore Engine DB Write"] = "Engine DB Write",
	["progress:UCore Engine Text DB Write"] = "Engine Text DB Write",
	["progress:UCore Engine Asset Scan"] = "Engine Asset Scan",
	["progress:UCore Engine Asset Persist"] = "Engine Asset Persist",
	["progress:UCore Engine Finalize"] = "Engine Finalize",
	["task:plugin"] = "NvimSourceCodeAccess",
	["task:asset_bridge"] = "NeovimLink",
}

local panel = {
	title = "UCore Init",
	notify_id = "ucore.status.init",
	ordered_keys = ordered_keys,
	items = {},
	spinner_active_keys = {},
	notify_handle = nil,
	boot_active = false,
	state = "running",
	pending_finish_message = nil,
	dismiss_version = 0,
	countdown_seconds = nil,
	manual_dismissed = false,
}

local function close_float()
	local win = float_state.win
	if win and vim.api.nvim_win_is_valid(win) then
		pcall(vim.api.nvim_win_close, win, true)
	end
	float_state.win = nil

	local buf = float_state.buf
	if buf and vim.api.nvim_buf_is_valid(buf) then
		pcall(vim.api.nvim_buf_delete, buf, { force = true })
	end
	float_state.buf = nil
end

local function ensure_float_buf()
	local buf = float_state.buf
	if buf and vim.api.nvim_buf_is_valid(buf) then
		return buf
	end

	buf = vim.api.nvim_create_buf(false, true)
	vim.bo[buf].bufhidden = "wipe"
	vim.bo[buf].buftype = "nofile"
	vim.bo[buf].swapfile = false
	float_state.buf = buf
	return buf
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

local function is_complete_message(text)
	text = tostring(text or "")
	if text == "" then
		return false
	end

	return text:find("100%%", 1, true) ~= nil
		or text:find("Skipped", 1, true) ~= nil
		or text:find("Ready", 1, true) ~= nil
		or text:find("Installed", 1, true) ~= nil
		or text:find("Missing", 1, true) ~= nil
		or text:find("Complete", 1, true) ~= nil
end

local function is_failed_message(text)
	text = tostring(text or "")
	if text == "" then
		return false
	end

	return text:lower():find("failed", 1, true) ~= nil
end

local function ordered_index(key)
	for index, item in ipairs(panel.ordered_keys) do
		if item == key then
			return index
		end
	end
	return math.huge
end

local function any_spinner_items()
	for key, active in pairs(panel.spinner_active_keys) do
		if active and panel.items[key] then
			return true
		end
	end
	return false
end

local function spinner_frame()
	return spinner_frames[spinner_index] or spinner_frames[1]
end

local function line_label(key)
	return label_map[key] or key:gsub("^progress:", "")
end

local function strip_repeated_prefix(message, prefix)
	message = tostring(message or "")
	prefix = tostring(prefix or "")
	if prefix == "" then
		return message
	end

	if message:sub(1, #prefix) == prefix then
		return vim.trim(message:sub(#prefix + 1))
	end

	return message
end

local function line_body(key, message)
	message = compact_message(message)
	if key == "boot" then
		return message
	end

	local raw_title = key:gsub("^progress:", "")
	local trimmed = strip_repeated_prefix(message, raw_title)
	if trimmed ~= message and trimmed ~= "" then
		return string.format("%s: %s", line_label(key), trimmed)
	end

	local label = line_label(key)
	trimmed = strip_repeated_prefix(message, label)
	if trimmed ~= message and trimmed ~= "" then
		return string.format("%s: %s", label, trimmed)
	end

	return message
end

local function line_prefix(key, message)
	if panel.spinner_active_keys[key] then
		return spinner_frame()
	end
	if is_failed_message(message) then
		return "x"
	end
	if is_complete_message(message) then
		return "="
	end
	return "-"
end

local function render_item_line(key, message)
	return string.format("%s %s", line_prefix(key, message), line_body(key, message))
end

local function visible_section_lines(section)
	local lines = {}
	for _, key in ipairs(section.keys) do
		local message = panel.items[key]
		if message and message ~= "" then
			table.insert(lines, render_item_line(key, message))
		end
	end
	return lines
end

local function content_lines()
	local lines = {}
	local boot = panel.items.boot
	if boot and boot ~= "" then
		local summary = render_item_line("boot", boot)
		if panel.state == "complete" and type(panel.countdown_seconds) == "number" then
			summary = string.format("%s  [closing in %ds]", summary, math.max(panel.countdown_seconds, 0))
		end
		table.insert(lines, summary)
	end

	for _, section in ipairs(section_specs) do
		local section_lines = visible_section_lines(section)
		if #section_lines > 0 then
			if #lines > 0 then
				table.insert(lines, "")
			end
			table.insert(lines, section.title)
			for _, line in ipairs(section_lines) do
				table.insert(lines, "  " .. line)
			end
		end
	end

	if #lines == 0 then
		local extra = {}
		for key, message in pairs(panel.items) do
			if key ~= "boot" and message and message ~= "" then
				table.insert(extra, { key = key, message = message })
			end
		end
		table.sort(extra, function(a, b)
			return ordered_index(a.key) < ordered_index(b.key)
		end)
		for _, item in ipairs(extra) do
			table.insert(lines, render_item_line(item.key, item.message))
		end
	end

	return lines
end

local function float_display_lines()
	local lines = content_lines()
	if #lines == 0 then
		return {}
	end

	local display = { panel.title, "" }
	vim.list_extend(display, lines)
	return display
end

local function float_text_width(lines)
	local width = 0
	for _, line in ipairs(lines) do
		width = math.max(width, vim.fn.strdisplaywidth(line))
	end
	return math.max(width, 1)
end

local function apply_float_highlights(buf, lines)
	if #lines >= 1 then
		pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, "Title", 0, 0, -1)
	end

	for index, line in ipairs(lines) do
		if index > 2 then
			local row = index - 1
			if line == "Workspace" or line == "Project" or line == "Engine" or line == "Unreal Integration" then
				pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, "Comment", row, 0, -1)
			elseif line:find("^  x ", 1, true) then
				pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, "DiagnosticError", row, 2, -1)
			elseif line:find("^  = ", 1, true) or line:find("^= ", 1, true) then
				pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, "String", row, 0, -1)
			end
		end
	end
end

local function render_float_panel()
	local lines = float_display_lines()
	if #lines == 0 then
		close_float()
		return
	end

	local width = math.min(math.max(float_text_width(lines) + 2, 44), math.max(vim.o.columns - 4, 44))
	local height = #lines
	local buf = ensure_float_buf()

	vim.bo[buf].modifiable = true
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
	vim.api.nvim_buf_clear_namespace(buf, highlight_ns, 0, -1)
	vim.bo[buf].modifiable = false

	local config = {
		relative = "editor",
		anchor = "NE",
		row = 1,
		col = vim.o.columns - 1,
		width = width,
		height = height,
		style = "minimal",
		focusable = false,
		noautocmd = true,
		border = "rounded",
		zindex = 250,
	}

	local win = float_state.win
	if win and vim.api.nvim_win_is_valid(win) then
		pcall(vim.api.nvim_win_set_config, win, config)
	else
		win = vim.api.nvim_open_win(buf, false, config)
		float_state.win = win
		vim.wo[win].winblend = 0
		vim.wo[win].wrap = false
		vim.wo[win].cursorline = false
	end

	apply_float_highlights(buf, lines)
end

local function render_notify_panel()
	local lines = content_lines()
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
		panel.notify_handle = nil
		render_float_panel()
	else
		close_float()
		render_notify_panel()
	end
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

local function dismiss_panel()
	if uses_builtin_notify() then
		close_float()
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

local function clear_panel_contents()
	panel.items = {}
	panel.spinner_active_keys = {}
	panel.boot_active = false
	panel.pending_finish_message = nil
	panel.countdown_seconds = nil
end

local function bump_dismiss_version()
	panel.dismiss_version = (panel.dismiss_version or 0) + 1
end

local function schedule_panel_dismiss(delay_ms)
	if not panel.notify_handle and not float_state.win then
		return
	end

	local version = panel.dismiss_version
	vim.defer_fn(function()
		if panel.dismiss_version ~= version then
			return
		end
		if panel.state == "complete" and not panel.boot_active and not any_spinner_items() then
			clear_panel_contents()
			dismiss_panel()
		end
	end, delay_ms or 5000)
end

local function schedule_panel_countdown(seconds)
	local version = panel.dismiss_version
	panel.countdown_seconds = seconds
	render()

	if seconds <= 0 then
		clear_panel_contents()
		dismiss_panel()
		render()
		return
	end

	vim.defer_fn(function()
		if panel.dismiss_version ~= version then
			return
		end
		if panel.state ~= "complete" or panel.boot_active or any_spinner_items() then
			return
		end
		schedule_panel_countdown(seconds - 1)
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

local function reset_panel()
	dismiss_panel()
	clear_panel_contents()
	panel.state = "running"
	panel.manual_dismissed = false
	bump_dismiss_version()
end

local function apply_pending_finish()
	if not panel.pending_finish_message or panel.boot_active or any_spinner_items() then
		return
	end

	local text = panel.pending_finish_message
	panel.pending_finish_message = nil
	panel.state = "complete"
	panel.items.boot = text
	bump_dismiss_version()
	schedule_panel_countdown(5)
end

function M.start(message)
	reset_panel()
	panel.boot_active = true
	panel.state = "running"
	panel.spinner_active_keys.boot = true
	panel.items.boot = message or "Initializing workspace..."
	render()
	schedule_spinner()
end

function M.finish(message)
	panel.boot_active = false
	panel.spinner_active_keys.boot = nil
	panel.pending_finish_message = message or "Workspace ready"
	bump_dismiss_version()
	apply_pending_finish()
end

function M.fail(message, detail)
	panel.boot_active = false
	panel.state = "failed"
	panel.pending_finish_message = nil
	panel.spinner_active_keys.boot = nil
	local text = message or "Initialization failed"
	if detail and detail ~= "" then
		text = text .. " | " .. tostring(detail)
	end
	panel.items.boot = text
	bump_dismiss_version()
	render()
end

function M.progress(title, message)
	local key = "progress:" .. title
	panel.spinner_active_keys[key] = true
	panel.items[key] = message
	panel.state = "running"
	bump_dismiss_version()
	render()
	schedule_spinner()
end

function M.progress_finish(title, message)
	local key = "progress:" .. title
	panel.spinner_active_keys[key] = nil
	panel.items[key] = compact_message(message or string.format("%s Complete", title))
	bump_dismiss_version()
	render()
	apply_pending_finish()
end

function M.unreal_start(message)
	panel.boot_active = true
	panel.state = "running"
	panel.spinner_active_keys.boot = true
	panel.items.boot = message or "Preparing Unreal integration..."
	bump_dismiss_version()
	render()
	schedule_spinner()
end

function M.unreal_finish(message)
	panel.boot_active = false
	panel.spinner_active_keys.boot = nil
	panel.pending_finish_message = message or "Unreal integration ready"
	bump_dismiss_version()
	apply_pending_finish()
end

function M.unreal_fail(message, detail)
	panel.boot_active = false
	panel.state = "failed"
	panel.pending_finish_message = nil
	panel.spinner_active_keys.boot = nil
	local text = message or "Unreal integration failed"
	if detail and detail ~= "" then
		text = text .. " | " .. tostring(detail)
	end
	panel.items.boot = text
	bump_dismiss_version()
	render()
end

function M.unreal_step(key, message)
	panel.spinner_active_keys[key] = true
	panel.items[key] = message
	panel.state = "running"
	bump_dismiss_version()
	render()
	schedule_spinner()
end

function M.unreal_step_finish(key, message)
	panel.spinner_active_keys[key] = nil
	panel.items[key] = message
	bump_dismiss_version()
	render()
	apply_pending_finish()
end

function M.unreal_clear(key)
	panel.spinner_active_keys[key] = nil
	panel.items[key] = nil
	bump_dismiss_version()
	render()
	apply_pending_finish()
end

function M.progress_fail(title, message)
	local key = "progress:" .. title
	panel.spinner_active_keys[key] = nil
	panel.state = "failed"
	panel.items[key] = message
	bump_dismiss_version()
	render()
end

function M.clear(key)
	panel.spinner_active_keys[key] = nil
	panel.items[key] = nil
	bump_dismiss_version()
	render()
	apply_pending_finish()
end

function M.clear_all()
	reset_panel()
	render()
end

return M
