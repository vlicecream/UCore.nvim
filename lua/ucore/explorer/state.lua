local config = require("ucore.config")

local M = {}

local explorer_config = config.values.explorer or {}

M.win = nil
M.buf = nil
M.tab = explorer_config.default_tab or "Project"
M.search = ""
M.expanded = {}
M.tree = nil
M.visible = {}
M.line_items = {}
M.counts = {
	matched = 0,
	total = 0,
}

function M.tabs()
	return (config.values.explorer and config.values.explorer.tabs) or { "Project", "Source", "Config" }
end

function M.is_valid_win()
	return M.win and vim.api.nvim_win_is_valid(M.win)
end

function M.is_valid_buf()
	return M.buf and vim.api.nvim_buf_is_valid(M.buf)
end

function M.current_tab_index()
	for i, tab in ipairs(M.tabs()) do
		if tab == M.tab then
			return i
		end
	end
	return 1
end

function M.set_tab_by_delta(delta)
	local tabs = M.tabs()
	local index = M.current_tab_index() + delta
	if index < 1 then
		index = #tabs
	elseif index > #tabs then
		index = 1
	end
	M.tab = tabs[index]
	M.search = ""
end

function M.expanded_key(path)
	return M.tab .. "::" .. tostring(path or "")
end

function M.is_expanded(node)
	return M.expanded[M.expanded_key(node.id or node.path or node.label)] == true
end

function M.set_expanded(node, value)
	M.expanded[M.expanded_key(node.id or node.path or node.label)] = value == true
end

function M.toggle_expanded(node)
	M.set_expanded(node, not M.is_expanded(node))
end

return M
