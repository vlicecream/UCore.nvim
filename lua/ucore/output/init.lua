local config = require("ucore.config")

local M = {}

local ns = vim.api.nvim_create_namespace("ucore_output_panel")

local state = {
	active = nil,
	content = {
		buf = nil,
		win = nil,
	},
	order = {},
	placeholder = nil,
	seq = 0,
	tabbar = {
		buf = nil,
		win = nil,
	},
	tabs = {},
}

local function defer_if_fast(fn)
	if vim.in_fast_event() then
		vim.schedule(fn)
		return true
	end
	return false
end

local function output_config()
	local ui = (config.values or {}).ui or {}
	local output = ui.output or {}
	return {
		auto_open = output.auto_open ~= false,
		enable = output.enable ~= false,
		height = math.max(8, tonumber(output.height) or 12),
		max_tabs = math.max(4, tonumber(output.max_tabs) or 8),
	}
end

local function valid_buf(buf)
	return buf and vim.api.nvim_buf_is_valid(buf)
end

local function valid_win(win)
	return win and vim.api.nvim_win_is_valid(win)
end

local function setup_highlights()
	vim.api.nvim_set_hl(0, "UCoreOutputTabActive", { fg = "#E5EFFF", bg = "#1F2937", bold = true })
	vim.api.nvim_set_hl(0, "UCoreOutputTabInactive", { fg = "#94A3B8", bg = "#111827" })
	vim.api.nvim_set_hl(0, "UCoreOutputTabUnread", { fg = "#FBBF24", bg = "#111827", bold = true })
	vim.api.nvim_set_hl(0, "UCoreOutputTabFailed", { fg = "#F87171", bg = "#111827", bold = true })
	vim.api.nvim_set_hl(0, "UCoreOutputTabSuccess", { fg = "#86EFAC", bg = "#111827", bold = true })
	vim.api.nvim_set_hl(0, "UCoreOutputTabBar", { fg = "#CBD5E1", bg = "#111827" })
	vim.api.nvim_set_hl(0, "UCoreOutputMuted", { fg = "#64748B" })
	vim.api.nvim_set_hl(0, "UCoreOutputCommand", { fg = "#4FC1FF" })
	vim.api.nvim_set_hl(0, "UCoreOutputInfo", { fg = "#DBE7FF" })
	vim.api.nvim_set_hl(0, "UCoreOutputWarning", { fg = "#FFCC66" })
	vim.api.nvim_set_hl(0, "UCoreOutputError", { fg = "#F44747", bold = true })
	vim.api.nvim_set_hl(0, "UCoreOutputSuccess", { fg = "#89D185", bold = true })
end

local function buffer_options(buf, filetype)
	vim.bo[buf].buftype = "nofile"
	vim.bo[buf].bufhidden = "hide"
	vim.bo[buf].swapfile = false
	vim.bo[buf].buflisted = false
	vim.bo[buf].modifiable = false
	vim.bo[buf].readonly = true
	vim.bo[buf].filetype = filetype
end

local function window_options(win, opts)
	if not valid_win(win) then
		return
	end

	vim.wo[win].number = false
	vim.wo[win].relativenumber = false
	vim.wo[win].signcolumn = "no"
	vim.wo[win].foldcolumn = "0"
	vim.wo[win].spell = false
	vim.wo[win].wrap = opts.wrap == true
	vim.wo[win].cursorline = opts.cursorline == true
	vim.wo[win].winfixheight = true
	vim.wo[win].list = false
	vim.wo[win].conceallevel = 0
end

local function current_content_buf()
	local tab = state.active and state.tabs[state.active] or nil
	if tab and valid_buf(tab.buf) then
		return tab.buf
	end

	if valid_buf(state.placeholder) then
		return state.placeholder
	end

	local buf = vim.api.nvim_create_buf(false, true)
	buffer_options(buf, "ucore-output")
	vim.bo[buf].modifiable = true
	vim.bo[buf].readonly = false
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, {
		"No active output tab",
	})
	vim.bo[buf].modifiable = false
	vim.bo[buf].readonly = true
	pcall(vim.api.nvim_buf_set_name, buf, "UCoreOutputPlaceholder")
	state.placeholder = buf
	return buf
end

local function render_tabbar()
	if not valid_buf(state.tabbar.buf) then
		return
	end

	local line = ""
	local spans = {}
	local hint = "  H/L switch  q close tab  x close panel"

	if #state.order == 0 then
		line = " UCore Output "
		spans[#spans + 1] = {
			group = "UCoreOutputMuted",
			start_col = 0,
			end_col = #line,
		}
	else
		for index, key in ipairs(state.order) do
			local tab = state.tabs[key]
			if tab then
				local title = tostring(tab.title or key or "Output")
				if #title > 22 then
					title = title:sub(1, 19) .. "..."
				end

				local prefix = " "
				if tab.status == "failed" then
					prefix = "!"
				elseif tab.status == "success" then
					prefix = "+"
				elseif tab.unread and key ~= state.active then
					prefix = "*"
				end

				local piece = string.format("[%s %s]", prefix, title)
				if index > 1 then
					line = line .. " "
				end
				local start_col = #line
				line = line .. piece
				local group = "UCoreOutputTabInactive"
				if key == state.active then
					group = "UCoreOutputTabActive"
				elseif tab.status == "failed" then
					group = "UCoreOutputTabFailed"
				elseif tab.unread then
					group = "UCoreOutputTabUnread"
				elseif tab.status == "success" then
					group = "UCoreOutputTabSuccess"
				end
				spans[#spans + 1] = {
					group = group,
					start_col = start_col,
					end_col = #line,
				}
			end
		end
	end

	local hint_start = #line
	if line ~= "" then
		line = line .. hint
	else
		line = hint
		hint_start = 0
	end
	spans[#spans + 1] = {
		group = "UCoreOutputMuted",
		start_col = hint_start,
		end_col = #line,
	}

	vim.bo[state.tabbar.buf].modifiable = true
	vim.bo[state.tabbar.buf].readonly = false
	vim.api.nvim_buf_set_lines(state.tabbar.buf, 0, -1, false, { line })
	vim.api.nvim_buf_clear_namespace(state.tabbar.buf, ns, 0, -1)
	for _, span in ipairs(spans) do
		pcall(vim.api.nvim_buf_add_highlight, state.tabbar.buf, ns, span.group, 0, span.start_col, span.end_col)
	end
	vim.bo[state.tabbar.buf].modifiable = false
	vim.bo[state.tabbar.buf].readonly = true
end

local function sync_content_buffer()
	if not valid_win(state.content.win) then
		return
	end

	local buf = current_content_buf()
	if vim.api.nvim_win_get_buf(state.content.win) ~= buf then
		vim.api.nvim_win_set_buf(state.content.win, buf)
	end
end

local function close_workspace()
	if valid_win(state.tabbar.win) then
		pcall(vim.api.nvim_win_close, state.tabbar.win, true)
	end
	if valid_win(state.content.win) then
		pcall(vim.api.nvim_win_close, state.content.win, true)
	end
	state.tabbar.win = nil
	state.content.win = nil
end

local function close_active_tab()
	local key = state.active
	if not key then
		return
	end
	local tab = state.tabs[key]
	if not tab then
		return
	end

	state.tabs[key] = nil
	for index, value in ipairs(state.order) do
		if value == key then
			table.remove(state.order, index)
			break
		end
	end

	if valid_buf(tab.buf) then
		pcall(vim.api.nvim_buf_delete, tab.buf, { force = true })
	end

	state.active = state.order[1]
	if #state.order == 0 then
		close_workspace()
	end
	render_tabbar()
	sync_content_buffer()
end

local function select_relative(direction)
	if #state.order < 2 or not state.active then
		return
	end

	local current_index = 1
	for index, key in ipairs(state.order) do
		if key == state.active then
			current_index = index
			break
		end
	end

	local next_index = current_index + direction
	if next_index < 1 then
		next_index = #state.order
	elseif next_index > #state.order then
		next_index = 1
	end

	local next_key = state.order[next_index]
	if not next_key or not state.tabs[next_key] then
		return
	end

	state.active = next_key
	state.tabs[next_key].unread = false
	render_tabbar()
	sync_content_buffer()
end

local function install_buffer_keymaps(buf)
	local function map(lhs, rhs, desc)
		vim.keymap.set("n", lhs, rhs, {
			buffer = buf,
			silent = true,
			nowait = true,
			desc = desc,
		})
	end

	map("q", close_active_tab, "UCore output close tab")
	map("x", close_workspace, "UCore output close panel")
	map("<Tab>", function()
		select_relative(1)
	end, "UCore output next tab")
	map("<S-Tab>", function()
		select_relative(-1)
	end, "UCore output previous tab")
	map("L", function()
		select_relative(1)
	end, "UCore output next tab")
	map("H", function()
		select_relative(-1)
	end, "UCore output previous tab")
end

local function ensure_workspace()
	if not output_config().enable then
		return false
	end

	if valid_win(state.tabbar.win) and valid_win(state.content.win) then
		render_tabbar()
		sync_content_buffer()
		return true
	end

	setup_highlights()

	local previous = vim.api.nvim_get_current_win()

	if not valid_buf(state.tabbar.buf) then
		local buf = vim.api.nvim_create_buf(false, true)
		buffer_options(buf, "ucore-output-tabs")
		pcall(vim.api.nvim_buf_set_name, buf, "UCoreOutputTabs")
		install_buffer_keymaps(buf)
		state.tabbar.buf = buf
	end

	vim.cmd("botright " .. tostring(output_config().height) .. "new")
	state.content.win = vim.api.nvim_get_current_win()
	window_options(state.content.win, { wrap = false, cursorline = false })
	install_buffer_keymaps(current_content_buf())
	vim.api.nvim_win_set_buf(state.content.win, current_content_buf())

	vim.cmd("topleft 1split")
	state.tabbar.win = vim.api.nvim_get_current_win()
	window_options(state.tabbar.win, { wrap = false, cursorline = false })
	vim.api.nvim_win_set_buf(state.tabbar.win, state.tabbar.buf)
	vim.api.nvim_win_set_height(state.tabbar.win, 1)

	if valid_win(state.content.win) then
		vim.api.nvim_win_set_height(state.content.win, output_config().height)
	end

	if valid_win(previous) then
		pcall(vim.api.nvim_set_current_win, previous)
	end

	render_tabbar()
	sync_content_buffer()
	return true
end

local function ensure_tab_buffer(tab)
	if valid_buf(tab.buf) then
		return tab.buf
	end

	local buf = vim.api.nvim_create_buf(false, true)
	buffer_options(buf, "ucore-output")
	pcall(vim.api.nvim_buf_set_name, buf, "UCoreOutput:" .. tostring(tab.title or tab.key))
	install_buffer_keymaps(buf)
	tab.buf = buf
	return buf
end

local function set_modifiable(buf, value)
	if not valid_buf(buf) then
		return
	end

	vim.bo[buf].modifiable = value
	vim.bo[buf].readonly = not value
end

local function move_key_to_front(key)
	for index, value in ipairs(state.order) do
		if value == key then
			table.remove(state.order, index)
			break
		end
	end
	table.insert(state.order, 1, key)
end

local function trim_old_tabs()
	local max_tabs = output_config().max_tabs
	while #state.order > max_tabs do
		local key = table.remove(state.order)
		local tab = key and state.tabs[key] or nil
		if tab and valid_buf(tab.buf) then
			pcall(vim.api.nvim_buf_delete, tab.buf, { force = true })
		end
		state.tabs[key] = nil
		if state.active == key then
			state.active = state.order[1]
		end
	end
end

local function normalize_lines(tab, data)
	if type(data) == "table" then
		local items = {}
		for _, value in ipairs(data) do
			items[#items + 1] = tostring(value)
		end
		return items
	end

	local text = tostring(data or "")
	if text == "" then
		return {}
	end

	text = text:gsub("\r\n", "\n"):gsub("\r", "\n")
	text = (tab.partial or "") .. text

	local ends_with_newline = text:sub(-1) == "\n"
	local lines = vim.split(text, "\n", { plain = true })
	if ends_with_newline then
		tab.partial = ""
		if lines[#lines] == "" then
			table.remove(lines, #lines)
		end
	else
		tab.partial = table.remove(lines, #lines) or ""
	end

	return lines
end

local function apply_line_group(buf, line_index, text, group)
	if not group or not valid_buf(buf) then
		return
	end

	local end_col = math.max(0, vim.fn.strchars(text or ""))
	pcall(vim.api.nvim_buf_set_extmark, buf, ns, line_index, 0, {
		hl_group = group,
		end_row = line_index,
		end_col = end_col,
	})
end

local function append_to_tab(tab, lines, opts)
	if vim.tbl_isempty(lines) then
		return
	end

	local buf = ensure_tab_buffer(tab)
	local groups = opts and opts.line_groups or nil
	local group = opts and opts.highlight or nil
	set_modifiable(buf, true)
	local start_line = vim.api.nvim_buf_line_count(buf)
	vim.api.nvim_buf_set_lines(buf, -1, -1, false, lines)
	for index, text in ipairs(lines) do
		local line_group = groups and groups[index] or group
		apply_line_group(buf, start_line + index - 1, text, line_group)
	end
	set_modifiable(buf, false)

	if state.active == tab.key and valid_win(state.content.win) and vim.api.nvim_win_get_buf(state.content.win) == buf then
		pcall(vim.api.nvim_win_set_cursor, state.content.win, { vim.api.nvim_buf_line_count(buf), 0 })
	end
end

local function replace_tab_lines(tab, lines, opts)
	local buf = ensure_tab_buffer(tab)
	set_modifiable(buf, true)
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
	vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
	local groups = opts and opts.line_groups or nil
	local group = opts and opts.highlight or nil
	for index, text in ipairs(lines) do
		local line_group = groups and groups[index] or group
		apply_line_group(buf, index - 1, text, line_group)
	end
	set_modifiable(buf, false)
	if state.active == tab.key and valid_win(state.content.win) and vim.api.nvim_win_get_buf(state.content.win) == buf then
		pcall(vim.api.nvim_win_set_cursor, state.content.win, { math.max(1, vim.api.nvim_buf_line_count(buf)), 0 })
	end
end

local function get_or_create_tab(opts)
	opts = opts or {}
	local key = tostring(opts.key or ("output:" .. tostring(state.seq + 1)))
	local tab = state.tabs[key]
	if tab then
		if opts.title and opts.title ~= "" then
			tab.title = tostring(opts.title)
		end
		if opts.kind and opts.kind ~= "" then
			tab.kind = tostring(opts.kind)
		end
		return tab
	end

	state.seq = state.seq + 1
	tab = {
		buf = nil,
		created_at = vim.loop.hrtime(),
		key = key,
		kind = tostring(opts.kind or "output"),
		partial = "",
		status = tostring(opts.status or "running"),
		title = tostring(opts.title or key),
		unread = false,
		updated_at = vim.loop.hrtime(),
	}
	state.tabs[key] = tab
	move_key_to_front(key)
	state.active = key
	ensure_tab_buffer(tab)
	trim_old_tabs()
	return tab
end

local function touch_tab(tab, opts)
	tab.updated_at = vim.loop.hrtime()
	if opts and opts.focus then
		state.active = tab.key
		tab.unread = false
		move_key_to_front(tab.key)
	elseif tab.key ~= state.active then
		tab.unread = true
	end

	if (opts and opts.open ~= false) and output_config().auto_open then
		ensure_workspace()
	end

	render_tabbar()
	sync_content_buffer()
end

function M.open_tab(opts)
	if defer_if_fast(function()
		M.open_tab(opts)
	end) then
		return nil
	end

	if not output_config().enable then
		return nil
	end

	local tab = get_or_create_tab(opts)
	touch_tab(tab, {
		focus = opts == nil or opts.focus ~= false,
		open = true,
	})
	return tab.key
end

function M.append(key, data, opts)
	if defer_if_fast(function()
		M.append(key, data, opts)
	end) then
		return
	end

	if not output_config().enable then
		return
	end

	opts = opts or {}
	local tab = get_or_create_tab(vim.tbl_extend("force", opts, { key = key }))
	if opts.status and opts.status ~= "" then
		tab.status = tostring(opts.status)
	end
	local lines = normalize_lines(tab, data)
	append_to_tab(tab, lines, opts)
	touch_tab(tab, {
		focus = opts.focus == true,
		open = opts.open ~= false,
	})
end

function M.replace(key, data, opts)
	if defer_if_fast(function()
		M.replace(key, data, opts)
	end) then
		return
	end

	if not output_config().enable then
		return
	end

	opts = opts or {}
	local tab = get_or_create_tab(vim.tbl_extend("force", opts, { key = key }))
	tab.partial = ""
	if opts.status and opts.status ~= "" then
		tab.status = tostring(opts.status)
	end
	local lines = type(data) == "table" and data or vim.split(tostring(data or ""), "\n", { plain = true })
	if #lines > 0 and lines[#lines] == "" then
		table.remove(lines, #lines)
	end
	replace_tab_lines(tab, lines, opts)
	touch_tab(tab, {
		focus = opts.focus == true,
		open = opts.open ~= false,
	})
end

function M.flush(key)
	if defer_if_fast(function()
		M.flush(key)
	end) then
		return
	end

	local tab = key and state.tabs[key] or nil
	if not tab or not tab.partial or tab.partial == "" then
		return
	end

	local pending = tab.partial
	tab.partial = ""
	M.append(key, pending, { focus = false, open = false })
end

function M.finish(key, message, opts)
	if defer_if_fast(function()
		M.finish(key, message, opts)
	end) then
		return
	end

	opts = opts or {}
	M.flush(key)
	if message and message ~= "" then
		M.append(key, message, vim.tbl_extend("force", opts, {
			focus = opts.focus == true,
			status = opts.status or "success",
		}))
		return
	end

	local tab = state.tabs[key]
	if not tab then
		return
	end
	tab.status = tostring(opts.status or "success")
	touch_tab(tab, {
		focus = opts.focus == true,
		open = opts.open ~= false,
	})
end

function M.fail(key, message, opts)
	if defer_if_fast(function()
		M.fail(key, message, opts)
	end) then
		return
	end

	opts = opts or {}
	M.flush(key)
	if message and message ~= "" then
		M.append(key, message, vim.tbl_extend("force", opts, {
			focus = opts.focus ~= false,
			status = "failed",
		}))
		return
	end

	local tab = state.tabs[key]
	if not tab then
		return
	end
	tab.status = "failed"
	touch_tab(tab, {
		focus = opts.focus ~= false,
		open = opts.open ~= false,
	})
end

function M.select(key)
	if defer_if_fast(function()
		M.select(key)
	end) then
		return
	end

	local tab = key and state.tabs[key] or nil
	if not tab then
		return
	end

	state.active = key
	tab.unread = false
	move_key_to_front(key)
	ensure_workspace()
	render_tabbar()
	sync_content_buffer()
end

function M.toggle()
	if defer_if_fast(function()
		M.toggle()
	end) then
		return
	end

	if valid_win(state.tabbar.win) or valid_win(state.content.win) then
		close_workspace()
		return
	end
	ensure_workspace()
end

function M.is_open()
	return valid_win(state.tabbar.win) and valid_win(state.content.win)
end

function M.setup()
	setup_highlights()
	rawset(_G, "__ucore_output_panel_api", M)
end

function M.reset()
	if defer_if_fast(function()
		M.reset()
	end) then
		return
	end

	close_workspace()
	for key, tab in pairs(state.tabs) do
		if valid_buf(tab.buf) then
			pcall(vim.api.nvim_buf_delete, tab.buf, { force = true })
		end
		state.tabs[key] = nil
	end
	if valid_buf(state.tabbar.buf) then
		pcall(vim.api.nvim_buf_delete, state.tabbar.buf, { force = true })
	end
	if valid_buf(state.placeholder) then
		pcall(vim.api.nvim_buf_delete, state.placeholder, { force = true })
	end
	state.active = nil
	state.content.buf = nil
	state.content.win = nil
	state.order = {}
	state.placeholder = nil
	state.seq = 0
	state.tabbar.buf = nil
	state.tabbar.win = nil
	state.tabs = {}
	rawset(_G, "__ucore_output_panel_api", nil)
end

return M
