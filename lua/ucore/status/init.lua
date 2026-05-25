local M = {}

local spinner_frames = { "⣾", "⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽" }
local spinner_index = 1
local spinner_scheduled = false
local render_scheduled = false
local highlight_ns = vim.api.nvim_create_namespace("ucore.status.float")

local float_state = { buf = nil, win = nil }

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
	{ title = "Workspace", keys = { "progress:UCore Server Start", "progress:UCore Server Ready", "progress:UCore Workspace Register" } },
	{ title = "Project", keys = {
		"progress:UCore Project Discovery",
		"progress:UCore Project DB Prepare",
		"progress:UCore Project Analysis",
		"progress:UCore Project DB Write",
		"progress:UCore Project Text DB Write",
		"progress:UCore Project Asset Scan",
		"progress:UCore Project Asset Persist",
		"progress:UCore Project Finalize",
	} },
	{ title = "Engine", keys = {
		"progress:UCore Engine Discovery",
		"progress:UCore Engine DB Prepare",
		"progress:UCore Engine Analysis",
		"progress:UCore Engine DB Write",
		"progress:UCore Engine Text DB Write",
		"progress:UCore Engine Asset Scan",
		"progress:UCore Engine Asset Persist",
		"progress:UCore Engine Finalize",
	} },
	{ title = "Unreal Integration", keys = { "task:plugin", "task:asset_bridge" } },
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
	items = {},
	spinner_active_keys = {},
	notify_handle = nil,
	boot_active = false,
	state = "running",
	pending_finish_message = nil,
	dismiss_version = 0,
	countdown_seconds = nil,
}

local function uses_builtin_notify()
	local info = debug.getinfo(vim.notify, "S")
	local source = tostring(info and info.source or "")
	return source:find("vim/_core/editor.lua", 1, true) ~= nil
end

local function split_lines(message)
	if message == nil then
		return {}
	end
	return vim.split(tostring(message), "\n", { plain = true })
end

local function compact(message)
	local lines = split_lines(message)
	return lines[1] or ""
end

local function spinner_frame()
	return spinner_frames[spinner_index] or spinner_frames[1]
end

local function any_spinner_items()
	for key, active in pairs(panel.spinner_active_keys) do
		if active and panel.items[key] then
			return true
		end
	end
	return false
end

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

local function render_item_line(key, message)
	local prefix = "-"
	local text = compact(message)
	if panel.spinner_active_keys[key] then
		prefix = spinner_frame()
	elseif text:lower():find("failed", 1, true) then
		prefix = "x"
	elseif text:find("100%%", 1, true) or text:find("Ready", 1, true) or text:find("Complete", 1, true) then
		prefix = "="
	end

	if key ~= "boot" then
		local label = label_map[key] or key:gsub("^progress:", "")
		local raw_title = key:gsub("^progress:", "")
		if text:sub(1, #raw_title) == raw_title then
			text = vim.trim(text:sub(#raw_title + 1))
		elseif text:sub(1, #label) == label then
			text = vim.trim(text:sub(#label + 1))
		end
		if text == "100%" or text == "Complete" or text == "Ready" then
			text = ""
		end
		if text ~= "" then
			text = string.format("%s: %s", label, text)
		else
			text = label
		end
	end

	return string.format("%s %s", prefix, text)
end

local function content_lines()
	local lines = {}
	if panel.items.boot and panel.items.boot ~= "" then
		local boot_line = render_item_line("boot", panel.items.boot)
		if panel.state == "complete" and type(panel.countdown_seconds) == "number" then
			boot_line = string.format("%s  [closing in %ds]", boot_line, math.max(panel.countdown_seconds, 0))
		end
		table.insert(lines, boot_line)
	end

	for _, section in ipairs(section_specs) do
		local section_lines = {}
		for _, key in ipairs(section.keys) do
			local message = panel.items[key]
			if message and message ~= "" then
				table.insert(section_lines, "  " .. render_item_line(key, message))
			end
		end
		if #section_lines > 0 then
			if #lines > 0 then
				table.insert(lines, "")
			end
			table.insert(lines, section.title)
			vim.list_extend(lines, section_lines)
		end
	end

	return lines
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

local function render_float_panel()
	local lines = content_lines()
	if #lines == 0 then
		close_float()
		return
	end

	local display = { panel.title, "" }
	vim.list_extend(display, lines)

	local width = 44
	for _, line in ipairs(display) do
		width = math.max(width, vim.fn.strdisplaywidth(line) + 2)
	end
	width = math.min(width, math.max(vim.o.columns - 4, 44))

	local buf = ensure_float_buf()
	vim.bo[buf].modifiable = true
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, display)
	vim.api.nvim_buf_clear_namespace(buf, highlight_ns, 0, -1)
	vim.bo[buf].modifiable = false

	local config = {
		relative = "editor",
		anchor = "NE",
		row = 1,
		col = vim.o.columns - 1,
		width = width,
		height = #display,
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
		vim.wo[win].wrap = false
		vim.wo[win].cursorline = false
	end

	pcall(vim.api.nvim_buf_add_highlight, buf, highlight_ns, "Title", 0, 0, -1)
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

local function reset_panel()
	dismiss_panel()
	clear_panel_contents()
	panel.state = "running"
	bump_dismiss_version()
end

function M.start(message)
	reset_panel()
	panel.boot_active = true
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
	panel.items[key] = compact(message or string.format("%s Complete", title))
	bump_dismiss_version()
	render()
	apply_pending_finish()
end

function M.progress_fail(title, message)
	local key = "progress:" .. title
	panel.spinner_active_keys[key] = nil
	panel.items[key] = message
	panel.state = "failed"
	bump_dismiss_version()
	render()
end

function M.unreal_start(message)
	panel.boot_active = true
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
