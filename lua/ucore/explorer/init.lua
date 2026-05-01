local config = require("ucore.config")
local project = require("ucore.project")
local render = require("ucore.explorer.render")
local search = require("ucore.explorer.search")
local state = require("ucore.explorer.state")
local tree = require("ucore.explorer.tree")

local M = {}

local providers = {
	Project = "ucore.explorer.providers.project",
	Source = "ucore.explorer.providers.source",
	Config = "ucore.explorer.providers.config",
}

local function explorer_config()
	return config.values.explorer or {}
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
	if previous_win and vim.api.nvim_win_is_valid(previous_win) and previous_win ~= state.win then
		vim.api.nvim_set_current_win(previous_win)
	else
		vim.cmd("wincmd p")
	end
	vim.cmd("edit " .. vim.fn.fnameescape(path))
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
	local ok, value = pcall(vim.fn.input, "UCore Explorer search: ", state.search or "")
	if ok and value ~= nil then
		state.search = value
		redraw()
	end
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
	map("q", function() switch_tab(-1) end, "UCore Explorer previous tab")
	map("e", function() switch_tab(1) end, "UCore Explorer next tab")
	map("Q", function() switch_tab(-1) end, "UCore Explorer previous tab")
	map("E", function() switch_tab(1) end, "UCore Explorer next tab")
	map("x", close_window, "UCore Explorer close")
	map("<CR>", activate, "UCore Explorer open")
	map("<Space>", activate, "UCore Explorer toggle")
	map("h", collapse, "UCore Explorer collapse")
	map("l", expand, "UCore Explorer expand")
	map("/", prompt_search, "UCore Explorer search")
	map("<Esc>", clear_search, "UCore Explorer clear search")
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
	if explorer_config().auto_focus ~= false then
		vim.api.nvim_set_current_win(state.win)
	end
end

function M.focus()
	if state.is_valid_win() then
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

return M
