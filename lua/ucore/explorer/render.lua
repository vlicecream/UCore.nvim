local state = require("ucore.explorer.state")

local M = {}

local ns = vim.api.nvim_create_namespace("ucore_explorer")
local devicons_ok, devicons = pcall(require, "nvim-web-devicons")

local function setup_highlights()
	local highlights = {
		UCoreExplorerBorder = { fg = "#ff8a4c" },
		UCoreExplorerHeader = { fg = "#ffb86c", bold = true },
		UCoreExplorerTab = { link = "Comment" },
		UCoreExplorerTabActive = { fg = "#7aa2f7", bold = true },
		UCoreExplorerSearch = { fg = "#7dcfff" },
		UCoreExplorerFolder = { link = "Directory" },
		UCoreExplorerFile = { link = "Normal" },
		UCoreExplorerMuted = { link = "Comment" },
		UCoreExplorerCount = { link = "Number" },
		UCoreExplorerIcon = { link = "Special" },
	}

	for group, opts in pairs(highlights) do
		pcall(vim.api.nvim_set_hl, 0, group, opts)
	end
end

local function str_width(text)
	return vim.fn.strdisplaywidth(text or "")
end

local function pad_right(text, width)
	local padding = math.max(0, width - str_width(text))
	return text .. string.rep(" ", padding)
end

local function file_icon(node)
	local label = node.label or ""
	if devicons_ok and devicons then
		local icon = devicons.get_icon(label, vim.fn.fnamemodify(label, ":e"), { default = true })
		if icon and icon ~= "" then
			return icon
		end
	end
	local lower = label:lower()
	if lower:match("%.cs$") then
		return "󰌛"
	elseif lower:match("%.h$") or lower:match("%.hpp$") then
		return "󰙱"
	elseif lower:match("%.cpp$") or lower:match("%.cc$") or lower:match("%.cxx$") then
		return "󰙲"
	elseif lower:match("%.ini$") then
		return ""
	elseif lower:match("%.uproject$") or lower:match("%.uplugin$") then
		return "󰚯"
	end
	return ""
end

local function folder_icon(node)
	if state.is_expanded(node) then
		return ""
	end
	return ""
end

local function tree_prefix(item)
	if item.depth == 0 then
		return ""
	end
	return (item.prefix or "") .. (item.is_last and "└─ " or "├─ ")
end

local function node_line(item)
	local node = item.node
	local prefix = tree_prefix(item)

	if node.type == "message" then
		return prefix .. "  " .. (node.message or node.label or "")
	end

	if node.type == "directory" then
		local symbol = state.is_expanded(node) and "▾" or "▸"
		return prefix .. symbol .. " " .. folder_icon(node) .. " " .. (node.label or "")
	end

	return prefix .. file_icon(node) .. " " .. (node.label or "")
end

local function header_line()
	local parts = { " q <", "  " }
	for index, tab in ipairs(state.tabs()) do
		if index > 1 then
			table.insert(parts, " | ")
		end
		table.insert(parts, tab)
	end
	table.insert(parts, "  > e ")
	return table.concat(parts, "")
end

local function search_line(width)
	local text = " > " .. (state.search or "")
	local count = string.format("%d/%d", state.counts.matched or 0, state.counts.total or 0)
	local inner_width = math.max(12, width - 2)
	local content = pad_right(text, inner_width - str_width(count)) .. count
	return "╭" .. string.rep("─", inner_width) .. "╮\n" .. "│" .. content .. "│" .. "\n" .. "╰" .. string.rep("─", inner_width) .. "╯"
end

local function apply_header_highlights(buf)
	local line = header_line()
	vim.api.nvim_buf_add_highlight(buf, ns, "UCoreExplorerHeader", 0, 0, #line)
	for _, tab in ipairs(state.tabs()) do
		local start = line:find(tab, 1, true)
		if start then
			local group = tab == state.tab and "UCoreExplorerTabActive" or "UCoreExplorerTab"
			vim.api.nvim_buf_add_highlight(buf, ns, group, 0, start - 1, start - 1 + #tab)
		end
	end
end

local function apply_tree_highlights(buf)
	for line_nr, item in ipairs(state.line_items or {}) do
		local row = line_nr + 3
		local node = item.node
		if node.type == "directory" then
			vim.api.nvim_buf_add_highlight(buf, ns, "UCoreExplorerFolder", row, 0, -1)
		elseif node.type == "file" then
			vim.api.nvim_buf_add_highlight(buf, ns, "UCoreExplorerFile", row, 0, -1)
		else
			vim.api.nvim_buf_add_highlight(buf, ns, "UCoreExplorerMuted", row, 0, -1)
		end

		local query = state.search or ""
		if query ~= "" then
			local text = node_line(item)
			local start = text:lower():find(query:lower(), 1, true)
			if start then
				vim.api.nvim_buf_add_highlight(buf, ns, "UCoreExplorerSearch", row, start - 1, start - 1 + #query)
			end
		end
	end
end

function M.render()
	if not state.is_valid_buf() then
		return
	end

	setup_highlights()

	local width = 44
	if state.is_valid_win() then
		width = vim.api.nvim_win_get_width(state.win)
	end

	local lines = {
		header_line(),
	}
	for _, line in ipairs(vim.split(search_line(width), "\n", { plain = true })) do
		table.insert(lines, line)
	end

	state.line_items = {}
	for _, item in ipairs(state.visible or {}) do
		table.insert(lines, node_line(item))
		table.insert(state.line_items, item)
	end

	vim.bo[state.buf].modifiable = true
	vim.api.nvim_buf_clear_namespace(state.buf, ns, 0, -1)
	vim.api.nvim_buf_set_lines(state.buf, 0, -1, false, lines)
	vim.bo[state.buf].modifiable = false
	vim.bo[state.buf].modified = false

	apply_header_highlights(state.buf)
	vim.api.nvim_buf_add_highlight(state.buf, ns, "UCoreExplorerBorder", 1, 0, -1)
	vim.api.nvim_buf_add_highlight(state.buf, ns, "UCoreExplorerSearch", 2, 0, -1)
	vim.api.nvim_buf_add_highlight(state.buf, ns, "UCoreExplorerBorder", 3, 0, -1)
	local count = string.format("%d/%d", state.counts.matched or 0, state.counts.total or 0)
	local count_start = math.max(0, #lines[3] - #count - 1)
	vim.api.nvim_buf_add_highlight(state.buf, ns, "UCoreExplorerCount", 2, count_start, count_start + #count)
	apply_tree_highlights(state.buf)
end

return M
