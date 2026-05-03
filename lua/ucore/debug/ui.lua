local M = {}

local state = {
	right = { win = nil, buf = nil },
	bottom = { win = nil, buf = nil, items = {} },
	session = nil,
	running = false,
}
local ns = vim.api.nvim_create_namespace("ucore_debug_ui")

local function valid_buf(buf)
	return buf and vim.api.nvim_buf_is_valid(buf)
end

local function valid_win(win)
	return win and vim.api.nvim_win_is_valid(win)
end

local function close_win(win)
	if valid_win(win) then
		pcall(vim.api.nvim_win_close, win, true)
	end
end

local function ensure_buf(slot, name, filetype)
	if valid_buf(slot.buf) then
		return slot.buf
	end

	local buf = vim.api.nvim_create_buf(false, true)
	vim.bo[buf].buftype = "nofile"
	vim.bo[buf].bufhidden = "wipe"
	vim.bo[buf].swapfile = false
	vim.bo[buf].modifiable = false
	vim.bo[buf].filetype = filetype
	pcall(vim.api.nvim_buf_set_name, buf, name)
	slot.buf = buf
	return buf
end

local function set_lines(buf, lines)
	if not valid_buf(buf) then
		return
	end

	vim.bo[buf].modifiable = true
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
	vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
	vim.bo[buf].modifiable = false
end

local function add_hl(buf, group, row, start_col, end_col)
	if valid_buf(buf) then
		pcall(vim.api.nvim_buf_add_highlight, buf, ns, group, row, start_col or 0, end_col or -1)
	end
end

local function setup_highlights()
	vim.api.nvim_set_hl(0, "UCoreDebugTitle", { fg = "#E5EFFF", bold = true })
	vim.api.nvim_set_hl(0, "UCoreDebugSection", { fg = "#93C5FD", bold = true })
	vim.api.nvim_set_hl(0, "UCoreDebugLabel", { fg = "#7C8FB8" })
	vim.api.nvim_set_hl(0, "UCoreDebugValue", { fg = "#DBE7FF" })
	vim.api.nvim_set_hl(0, "UCoreDebugAccent", { fg = "#86EFAC" })
	vim.api.nvim_set_hl(0, "UCoreDebugWarn", { fg = "#FDBA74" })
	vim.api.nvim_set_hl(0, "UCoreDebugMuted", { fg = "#64748B" })
	vim.api.nvim_set_hl(0, "UCoreDebugCurrent", { fg = "#38BDF8", bold = true })
end

local function width()
	local cols = vim.o.columns
	return math.max(36, math.min(50, math.floor(cols * 0.28)))
end

local function height()
	local lines = vim.o.lines - vim.o.cmdheight
	return math.max(10, math.min(16, math.floor(lines * 0.24)))
end

local function open_right()
	local buf = ensure_buf(state.right, "UCoreDebugInspect", "ucore-debug-inspect")
	if valid_win(state.right.win) then
		pcall(vim.api.nvim_win_set_width, state.right.win, width())
		return state.right.win, buf
	end

	local current = vim.api.nvim_get_current_win()
	vim.cmd("botright vsplit")
	local win = vim.api.nvim_get_current_win()
	vim.api.nvim_win_set_buf(win, buf)
	vim.api.nvim_set_current_win(current)
	state.right.win = win

	vim.wo[win].number = false
	vim.wo[win].relativenumber = false
	vim.wo[win].signcolumn = "no"
	vim.wo[win].foldcolumn = "0"
	vim.wo[win].spell = false
	vim.wo[win].wrap = false
	vim.wo[win].cursorline = false
	vim.wo[win].winfixwidth = true
	vim.api.nvim_win_set_width(win, width())

	return win, buf
end

local function open_bottom()
	local buf = ensure_buf(state.bottom, "UCoreDebugStack", "ucore-debug-stack")
	if valid_win(state.bottom.win) then
		pcall(vim.api.nvim_win_set_height, state.bottom.win, height())
		return state.bottom.win, buf
	end

	local current = vim.api.nvim_get_current_win()
	vim.cmd("botright split")
	local win = vim.api.nvim_get_current_win()
	vim.api.nvim_win_set_buf(win, buf)
	vim.api.nvim_set_current_win(current)
	state.bottom.win = win

	vim.wo[win].number = false
	vim.wo[win].relativenumber = false
	vim.wo[win].signcolumn = "no"
	vim.wo[win].foldcolumn = "0"
	vim.wo[win].spell = false
	vim.wo[win].wrap = false
	vim.wo[win].cursorline = true
	vim.wo[win].winfixheight = true
	vim.api.nvim_win_set_height(win, height())

	vim.keymap.set("n", "<CR>", function()
		M.activate_current_item()
	end, {
		buffer = buf,
		silent = true,
		desc = "UCore debug activate stack item",
	})

	return win, buf
end

local function clear_state()
	state.bottom.items = {}
	state.session = nil
	state.running = false
end

local function join_path(frame)
	local source = frame and frame.source or {}
	local path = source.path or source.name or "<unknown>"
	local line = tonumber(frame and frame.line or 0) or 0
	return string.format("%s:%d", path, line)
end

local function short_path(path)
	path = tostring(path or ""):gsub("\\", "/")
	local cwd = vim.loop.cwd()
	if cwd and cwd ~= "" then
		cwd = cwd:gsub("\\", "/")
		if path:sub(1, #cwd):lower() == cwd:lower() then
			return path:sub(#cwd + 2)
		end
	end
	return path
end

local function push(lines, text)
	table.insert(lines, text)
end

local function scopes_from_frame(frame)
	local scopes = {}
	for _, scope in ipairs(frame and frame.scopes or {}) do
		table.insert(scopes, scope)
	end
	return scopes
end

local function render_right(session)
	local _, buf = open_right()
	local lines = {}
	push(lines, "UCore Debug")
	push(lines, "")

	if not session then
		push(lines, "No Active Session")
		set_lines(buf, lines)
		add_hl(buf, "UCoreDebugTitle", 0, 0, -1)
		add_hl(buf, "UCoreDebugMuted", 2, 0, -1)
		return
	end

	if state.running or not session.current_frame then
		push(lines, "State")
		push(lines, "  Running")
		set_lines(buf, lines)
		add_hl(buf, "UCoreDebugTitle", 0, 0, -1)
		add_hl(buf, "UCoreDebugSection", 2, 0, -1)
		add_hl(buf, "UCoreDebugAccent", 3, 2, -1)
		return
	end

	local frame = session.current_frame
	push(lines, "Current Stop")
	push(lines, "  Frame   " .. tostring(frame.name or "<frame>"))
	push(lines, "  Source  " .. short_path(join_path(frame)))
	push(lines, "")

	local scopes = scopes_from_frame(frame)
	if vim.tbl_isempty(scopes) then
		push(lines, "Locals")
		push(lines, "  Loading...")
		set_lines(buf, lines)
		add_hl(buf, "UCoreDebugTitle", 0, 0, -1)
		add_hl(buf, "UCoreDebugSection", 2, 0, -1)
		add_hl(buf, "UCoreDebugSection", 6, 0, -1)
		add_hl(buf, "UCoreDebugMuted", 7, 2, -1)
		return
	end

	for _, scope in ipairs(scopes) do
		push(lines, tostring(scope.name or "Scope"))
		for _, variable in ipairs(scope.variables or {}) do
			local value = tostring(variable.value or "")
			if #value > 46 then
				value = value:sub(1, 43) .. "..."
			end
			push(lines, string.format("  %-18s %s", tostring(variable.name or "?"), value))
		end
		push(lines, "")
	end

	set_lines(buf, lines)
	add_hl(buf, "UCoreDebugTitle", 0, 0, -1)
	add_hl(buf, "UCoreDebugSection", 2, 0, -1)
	add_hl(buf, "UCoreDebugLabel", 3, 0, 9)
	add_hl(buf, "UCoreDebugValue", 3, 10, -1)
	add_hl(buf, "UCoreDebugLabel", 4, 0, 9)
	add_hl(buf, "UCoreDebugValue", 4, 10, -1)
	for row, text in ipairs(lines) do
		if row > 6 then
			if not text:match("^%s") and text ~= "" then
				add_hl(buf, "UCoreDebugSection", row - 1, 0, -1)
			elseif text:match("^  ") then
				local name_end = text:find("%s%s+", 3) or math.min(#text, 20)
				add_hl(buf, "UCoreDebugLabel", row - 1, 2, name_end)
				add_hl(buf, "UCoreDebugValue", row - 1, math.min(name_end + 1, #text), -1)
			end
		end
	end
end

local function add_item(kind, payload)
	state.bottom.items[#state.bottom.items + 1] = {
		kind = kind,
		payload = payload,
	}
end

local function render_bottom(session)
	local _, buf = open_bottom()
	local lines = {}
	state.bottom.items = {}
	local function push_with_item(text, kind, payload)
		table.insert(lines, text)
		state.bottom.items[#lines] = kind and {
			kind = kind,
			payload = payload,
		} or nil
	end

	push_with_item("UCore Debug Stack")
	push_with_item("")

	if not session then
		push_with_item("No active debug session.")
		set_lines(buf, lines)
		add_hl(buf, "UCoreDebugTitle", 0, 0, -1)
		add_hl(buf, "UCoreDebugMuted", 2, 0, -1)
		return
	end

	if state.running then
		push_with_item("Program is running.")
		set_lines(buf, lines)
		add_hl(buf, "UCoreDebugTitle", 0, 0, -1)
		add_hl(buf, "UCoreDebugAccent", 2, 0, -1)
		return
	end

	local current_frame_id = session.current_frame and session.current_frame.id or nil
	local thread_ids = {}
	for id, _ in pairs(session.threads or {}) do
		table.insert(thread_ids, id)
	end
	table.sort(thread_ids)

	if vim.tbl_isempty(thread_ids) then
		push_with_item("Threads are loading...")
		set_lines(buf, lines)
		add_hl(buf, "UCoreDebugTitle", 0, 0, -1)
		add_hl(buf, "UCoreDebugMuted", 2, 0, -1)
		return
	end

	for _, thread_id in ipairs(thread_ids) do
		local thread = session.threads[thread_id]
		local stopped = session.stopped_thread_id == thread_id
		local header = string.format("[%s] Thread %s  %s", stopped and "*" or " ", tostring(thread_id), tostring(thread and thread.name or ""))
		push_with_item(header, "thread", { thread_id = thread_id })

		local frames = thread and thread.frames or {}
		if vim.tbl_isempty(frames) then
			push_with_item("    frames loading...")
		else
			for index, frame in ipairs(frames) do
				local current = current_frame_id == frame.id
				local prefix = current and "  >" or "   "
				local location = short_path(join_path(frame))
				local text = string.format("%s %02d  %-36s  %s", prefix, index, tostring(frame.name or "<frame>"), location)
				push_with_item(text, "frame", {
					thread_id = thread_id,
					frame = frame,
				})
			end
		end

		push_with_item("")
	end

	push_with_item("Breakpoints")
	local breakpoints = require("dap.breakpoints").get()
	local count = 0
	for bufnr, buf_breakpoints in pairs(breakpoints) do
		local path = short_path(vim.api.nvim_buf_get_name(bufnr))
		for _, bp in ipairs(buf_breakpoints) do
			count = count + 1
			push_with_item(string.format("  %s:%d", path, tonumber(bp.line or 0) or 0))
		end
	end
	if count == 0 then
		push_with_item("  none")
	end

	set_lines(buf, lines)
	add_hl(buf, "UCoreDebugTitle", 0, 0, -1)
	for row, text in ipairs(lines) do
		local line = row - 1
		if text:match("^%[[%* ]%] Thread") then
			add_hl(buf, text:find("%[%*%]") and "UCoreDebugCurrent" or "UCoreDebugSection", line, 0, -1)
		elseif text:match("^  >") then
			add_hl(buf, "UCoreDebugCurrent", line, 0, -1)
		elseif text == "Breakpoints" then
			add_hl(buf, "UCoreDebugSection", line, 0, -1)
		elseif text:match("^  none") or text:match("^    frames loading") then
			add_hl(buf, "UCoreDebugMuted", line, 0, -1)
		elseif text:match("^   %d") then
			add_hl(buf, "UCoreDebugValue", line, 0, -1)
		end
	end
end

local function fetch_variables(session, frame, callback)
	if not frame then
		return callback()
	end

	session:request("scopes", { frameId = frame.id }, function(err, response)
		if err then
			return callback()
		end

		local scopes = response and response.scopes or {}
		if vim.tbl_isempty(scopes) then
			frame.scopes = {}
			return callback()
		end

		local remaining = #scopes
		for _, scope in ipairs(scopes) do
			if tonumber(scope.variablesReference or 0) > 0 then
				session:request("variables", { variablesReference = scope.variablesReference }, function(_, resp)
					scope.variables = resp and resp.variables or {}
					remaining = remaining - 1
					if remaining == 0 then
						frame.scopes = scopes
						callback()
					end
				end)
			else
				scope.variables = {}
				remaining = remaining - 1
				if remaining == 0 then
					frame.scopes = scopes
					callback()
				end
			end
		end
	end)
end

local function ensure_stack(session, callback)
	if not session or not session.stopped_thread_id then
		return callback()
	end

	session:update_threads(function()
		local thread = session.threads and session.threads[session.stopped_thread_id]
		if not thread then
			return callback()
		end

		if thread.frames and #thread.frames > 0 then
			return callback()
		end

		session:request("stackTrace", {
			threadId = session.stopped_thread_id,
			startFrame = 0,
			levels = 20,
		}, function(_, response)
			thread.frames = response and response.stackFrames or {}
			if not session.current_frame and thread.frames[1] then
				session.current_frame = thread.frames[1]
			end
			callback()
		end)
	end)
end

function M.open()
	setup_highlights()
	open_right()
	open_bottom()
end

function M.close()
	close_win(state.right.win)
	close_win(state.bottom.win)
	state.right.win = nil
	state.bottom.win = nil
	state.right.buf = nil
	state.bottom.buf = nil
	clear_state()
end

function M.is_open()
	return valid_win(state.right.win) or valid_win(state.bottom.win)
end

function M.mark_running(session)
	state.session = session
	state.running = true
	if valid_win(state.right.win) or valid_win(state.bottom.win) then
		render_right(session)
		render_bottom(session)
	end
end

function M.refresh(session)
	state.session = session
	state.running = false
	M.open()

	if not session then
		render_right(nil)
		render_bottom(nil)
		return
	end

	ensure_stack(session, function()
		fetch_variables(session, session.current_frame, function()
			vim.schedule(function()
				render_right(session)
				render_bottom(session)
			end)
		end)
	end)
end

function M.activate_current_item()
	if not valid_buf(state.bottom.buf) or not state.session then
		return
	end

	local row = vim.api.nvim_win_get_cursor(state.bottom.win)[1]
	local item = state.bottom.items[row]
	if not item then
		return
	end

	if item.kind == "frame" and item.payload and item.payload.frame then
		state.session.stopped_thread_id = item.payload.thread_id
		state.session:_frame_set(item.payload.frame)
		vim.defer_fn(function()
			M.refresh(state.session)
		end, 120)
	end
end

return M
