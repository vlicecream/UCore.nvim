local config = require("ucore.config")
local project = require("ucore.project")
local render = require("ucore.explorer.render")
local search = require("ucore.explorer.search")
local state = require("ucore.explorer.state")
local tree = require("ucore.explorer.tree")
local select_ui = require("ucore.ui.select")

local M = {}

local providers = {
	Project = "ucore.explorer.providers.project",
	Source = "ucore.explorer.providers.source",
	Config = "ucore.explorer.providers.config",
}

local function explorer_config()
	return config.values.explorer or {}
end

local function setup_highlights()
	vim.api.nvim_set_hl(0, "UCorePanelBorder", { fg = "#3F3F46" })
end

local function minimum_width_for_tabs()
	local text = " q <  " .. table.concat(state.tabs(), " | ") .. "  > e "
	return vim.fn.strdisplaywidth(text) + 2
end

local function ensure_buffer()
	if state.is_valid_buf() then
		return state.buf
	end

	state.buf = vim.api.nvim_create_buf(false, true)
	vim.bo[state.buf].buftype = "nofile"
	vim.bo[state.buf].bufhidden = "hide"
	vim.bo[state.buf].buflisted = false
	vim.bo[state.buf].swapfile = false
	vim.bo[state.buf].filetype = "ucore-explorer"
	vim.bo[state.buf].modifiable = false
	pcall(vim.api.nvim_buf_set_name, state.buf, "UCore Explorer")
	return state.buf
end

local function close_other_explorers()
	if explorer_config().close_other_explorers ~= true then
		return
	end
	pcall(vim.cmd, "NvimTreeClose")
	pcall(vim.cmd, "Neotree close")
end

local function ensure_window()
	if state.is_valid_win() then
		return state.win
	end

	setup_highlights()
	close_other_explorers()

	local previous_win = vim.api.nvim_get_current_win()
	local cfg = explorer_config()
	local width = math.max(cfg.width or 36, minimum_width_for_tabs())
	width = math.max(width, cfg.min_width or 0)
	if cfg.max_width then
		width = math.min(width, cfg.max_width)
	end

	vim.cmd("topleft vertical split")
	state.win = vim.api.nvim_get_current_win()
	vim.api.nvim_win_set_buf(state.win, ensure_buffer())
	vim.api.nvim_win_set_width(state.win, width)
	vim.wo[state.win].number = false
	vim.wo[state.win].relativenumber = false
	vim.wo[state.win].signcolumn = "no"
	vim.wo[state.win].foldcolumn = "0"
	vim.wo[state.win].wrap = false
	vim.wo[state.win].spell = false
	vim.wo[state.win].cursorline = true
	vim.wo[state.win].winfixwidth = true
	vim.wo[state.win].winbar = ""
	vim.wo[state.win].winhl = "Normal:Normal,SignColumn:Normal,EndOfBuffer:Normal,WinSeparator:UCorePanelBorder"
	if explorer_config().auto_focus == false and vim.api.nvim_win_is_valid(previous_win) then
		vim.api.nvim_set_current_win(previous_win)
	end
	return state.win
end

local function provider_for_tab(tab)
	local module_name = providers[tab]
	if not module_name then
		return nil
	end
	local ok, provider = pcall(require, module_name)
	if not ok then
		return nil
	end
	return provider
end

local function load_tree()
	local provider = provider_for_tab(state.tab)
	if not provider or type(provider.load) ~= "function" then
		state.tree = tree.message(state.tab, "Provider not available: " .. tostring(state.tab))
		return
	end

	local ok, result = pcall(provider.load)
	if ok and result then
		state.tree = result
	else
		state.tree = tree.message(state.tab, tostring(result or "Failed to load explorer tab"))
	end

	if state.tree and state.tree.type == "directory" then
		state.set_expanded(state.tree, true)
	end
end

local function rebuild_visible()
	if not state.tree then
		load_tree()
	end

	local root = state.tree
	local total = tree.total_nodes(root)
	local filtered = root
	local matched = total
	local forced_expanded = {}

	if state.search and state.search ~= "" then
		filtered, matched, forced_expanded = search.apply(root, state.search)
		for key, value in pairs(forced_expanded or {}) do
			state.expanded[state.expanded_key(key)] = value
		end
	end

	state.counts = {
		matched = matched,
		total = total,
	}
	state.visible = tree.flatten(filtered, state)
end

local function redraw()
	rebuild_visible()
	render.render()
end

local function current_item()
	if not state.is_valid_win() then
		return nil
	end
	local row = vim.api.nvim_win_get_cursor(state.win)[1]
	return state.line_items[row - 4]
end

local function open_file(path)
	local previous_win = vim.fn.win_getid(vim.fn.winnr("#"))
	if vim.api.nvim_get_current_win() ~= state.win then
		vim.cmd("edit " .. vim.fn.fnameescape(path))
		return
	end
	if previous_win and vim.api.nvim_win_is_valid(previous_win) and previous_win ~= state.win then
		vim.api.nvim_set_current_win(previous_win)
	else
		vim.cmd("wincmd p")
	end
	vim.cmd("edit " .. vim.fn.fnameescape(path))
end

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function path_starts_with(path, prefix)
	path = normalize(path or "")
	prefix = normalize(prefix or "")
	if path == "" or prefix == "" then
		return false
	end
	if path == prefix then
		return true
	end
	return path:sub(1, #prefix + 1) == prefix .. "/"
end

local function dirname(path)
	return normalize(vim.fn.fnamemodify(path, ":h"))
end

local function join_path(base, child)
	base = normalize(base or "")
	child = normalize(child or "")
	if base == "" then
		return child
	end
	if child == "" then
		return base
	end
	return normalize(base:gsub("/+$", "") .. "/" .. child:gsub("^/+", ""))
end

local function current_context_path()
	local win = vim.api.nvim_get_current_win()
	if state.is_valid_win() and win == state.win then
		local previous_win = vim.fn.win_getid(vim.fn.winnr("#"))
		if previous_win and vim.api.nvim_win_is_valid(previous_win) and previous_win ~= state.win then
			local bufnr = vim.api.nvim_win_get_buf(previous_win)
			return normalize(vim.api.nvim_buf_get_name(bufnr))
		end
	end

	return normalize(vim.api.nvim_buf_get_name(0))
end

local function reveal_path_in_tree(node, target_path)
	if not node or not target_path then
		return false
	end

	if node.type == "file" then
		return normalize(node.path) == target_path
	end

	if node.type ~= "directory" then
		return false
	end

	if node.path and not path_starts_with(target_path, node.path) then
		return false
	end

	tree.ensure_children(node)
	for _, child in ipairs(node.children or {}) do
		if reveal_path_in_tree(child, target_path) then
			state.set_expanded(node, true)
			return true
		end
	end

	return false
end

local function focus_revealed_path(target_path)
	if not state.is_valid_win() or not target_path then
		return
	end

	for index, item in ipairs(state.line_items or {}) do
		if normalize(item.node and item.node.path) == target_path then
			pcall(vim.api.nvim_win_set_cursor, state.win, { index + 4, 0 })
			pcall(vim.api.nvim_win_call, state.win, function()
				vim.cmd("normal! zz")
			end)
			return
		end
	end
end

local function reveal_current_file()
	local target_path = current_context_path()
	if not target_path or target_path == "" or not state.tree then
		return
	end

	if vim.fn.filereadable(target_path) ~= 1 then
		return
	end

	if reveal_path_in_tree(state.tree, target_path) then
		redraw()
		focus_revealed_path(target_path)
	end
end

local function current_target_directory()
	if state.is_valid_win() then
		local item = current_item()
		if item and item.node then
			if item.node.type == "directory" and item.node.path then
				return normalize(item.node.path)
			end
			if item.node.path then
				return dirname(item.node.path)
			end
		end
	end

	local path = vim.api.nvim_buf_get_name(0)
	if path and path ~= "" then
		path = normalize(path)
		if vim.fn.isdirectory(path) == 1 then
			return path
		end
		return dirname(path)
	end

	return project.find_project_root_from_context()
end

local function ensure_project_target_dir()
	local target_dir = current_target_directory()
	if not target_dir or target_dir == "" then
		vim.notify("UCore explorer: no target directory available", vim.log.levels.WARN)
		return nil, nil
	end

	local root = project.find_project_root(target_dir) or project.find_project_root_from_context()
	if not root then
		vim.notify("UCore explorer: not inside an Unreal project", vim.log.levels.WARN)
		return nil, nil
	end

	return normalize(target_dir), normalize(root)
end

local function create_relative_path(kind, suffix)
	local target_dir, _root = ensure_project_target_dir()
	if not target_dir then
		return
	end

	local prompt = kind == "file" and "UCore new file" or "UCore new directory"
	local title = string.format("%s  [%s]", prompt, target_dir)
	select_ui.input({
		title = title,
		default = suffix or "",
	}, function(value)
		if value == nil then
			return
		end

		value = vim.trim(tostring(value or "")):gsub("\\", "/")
		if value == "" then
			return
		end

		local is_absolute = value:match("^%a:/") or value:match("^/")
		local path = normalize(is_absolute and value or join_path(target_dir, value))
		if not path or path == "" then
			return
		end

		if kind == "directory" then
			vim.fn.mkdir(path, "p")
			if vim.fn.isdirectory(path) ~= 1 then
				vim.notify("UCore explorer: failed to create directory", vim.log.levels.ERROR)
				return
			end
			refresh_current()
			vim.notify("UCore new: created directory " .. path, vim.log.levels.INFO)
			return
		end

		if vim.fn.filereadable(path) == 1 then
			vim.notify("UCore new: path already exists: " .. path, vim.log.levels.WARN)
			open_file(path)
			return
		end
		if vim.fn.isdirectory(path) == 1 then
			vim.notify("UCore new: directory already exists: " .. path, vim.log.levels.WARN)
			refresh_current()
			return
		end

		vim.fn.mkdir(dirname(path), "p")
		local ok, err = pcall(vim.fn.writefile, {}, path)
		if not ok then
			vim.notify("UCore explorer: failed to create file: " .. tostring(err), vim.log.levels.ERROR)
			return
		end
		refresh_current()
		open_file(path)
	end)
end

local function activate()
	local item = current_item()
	if not item then
		return
	end
	local node = item.node
	local path = tree.openable_path(node)
	if path then
		open_file(path)
	elseif node.type == "directory" then
		if state.is_expanded(node) then
			state.set_expanded(node, false)
		else
			tree.expand_directory(node, state)
		end
		redraw()
	end
end

local function expand()
	local item = current_item()
	if item and item.node.type == "directory" then
		tree.expand_directory(item.node, state)
		redraw()
	elseif item then
		activate()
	end
end

local function collapse()
	local item = current_item()
	if item and item.node.type == "directory" then
		state.set_expanded(item.node, false)
		redraw()
	end
end

local function prompt_search()
	select_ui.input({
		title = "UCore Explorer search",
		default = state.search or "",
	}, function(value)
		if value == nil then
			return
		end
		state.search = value
		redraw()
	end)
end

local function clear_search()
	state.search = ""
	redraw()
end

local function switch_tab(delta)
	state.set_tab_by_delta(delta)
	state.tree = nil
	load_tree()
	redraw()
end

local function refresh_current()
	state.tree = nil
	load_tree()
	redraw()
end

local function refresh_all()
	state.expanded = {}
	state.tree = nil
	load_tree()
	redraw()
end

local function close_window()
	if state.is_valid_win() then
		vim.api.nvim_win_close(state.win, true)
	end
	state.win = nil
end

local function map(lhs, rhs, desc)
	vim.keymap.set("n", lhs, rhs, {
		buffer = state.buf,
		nowait = true,
		silent = true,
		desc = desc,
	})
end

local function setup_maps()
	map("q", close_window, "UCore Explorer close")
	map("e", function() switch_tab(1) end, "UCore Explorer next tab")
	map("Q", function() switch_tab(-1) end, "UCore Explorer previous tab")
	map("E", function() switch_tab(1) end, "UCore Explorer next tab")
	map("H", function() switch_tab(-1) end, "UCore Explorer previous tab")
	map("L", function() switch_tab(1) end, "UCore Explorer next tab")
	map("[", function() switch_tab(-1) end, "UCore Explorer previous tab")
	map("]", function() switch_tab(1) end, "UCore Explorer next tab")
	map("x", close_window, "UCore Explorer close")
	map("<Esc>", close_window, "UCore Explorer close")
	map("<CR>", activate, "UCore Explorer open")
	map("<Space>", activate, "UCore Explorer toggle")
	map("h", collapse, "UCore Explorer collapse")
	map("l", expand, "UCore Explorer expand")
	map("/", prompt_search, "UCore Explorer search")
	map("r", refresh_current, "UCore Explorer refresh tab")
	map("R", refresh_all, "UCore Explorer refresh all")
end

function M.open()
	ensure_buffer()
	ensure_window()
	setup_maps()
	if not state.tree then
		load_tree()
	end
	redraw()
	reveal_current_file()
	if explorer_config().auto_focus ~= false then
		vim.api.nvim_set_current_win(state.win)
	end
end

function M.focus()
	if state.is_valid_win() then
		reveal_current_file()
		vim.api.nvim_set_current_win(state.win)
	else
		M.open()
	end
end

function M.toggle()
	if state.is_valid_win() then
		close_window()
	else
		M.open()
	end
end

function M.close()
	close_window()
end

function M.smart_toggle(fallback)
	local root = project.find_project_root_from_context({
		registered_fallback = false,
	})

	if root then
		M.toggle()
		return true
	end

	if type(fallback) == "function" then
		fallback()
	elseif type(fallback) == "string" and fallback ~= "" then
		pcall(vim.cmd, fallback)
	end

	return false
end

function M.auto_open_for_project(project_root)
	if not project_root or explorer_config().auto_open == false then
		return
	end
	if state.is_valid_win() then
		return
	end

	vim.defer_fn(function()
		local current_root = project.find_project_root_from_context({
			registered_fallback = false,
		})
		if current_root == project_root then
			M.open()
		end
	end, explorer_config().auto_open_delay_ms or 120)
end

function M.refresh()
	refresh_current()
end

function M.new_file()
	create_relative_path("file")
end

function M.new_directory()
	create_relative_path("directory")
end

return M
