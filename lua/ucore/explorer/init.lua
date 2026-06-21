local config = require("ucore.config")
local project = require("ucore.project")
local render = require("ucore.explorer.render")
local search = require("ucore.explorer.search")
local state = require("ucore.explorer.state")
local tree = require("ucore.explorer.tree")
local select_ui = require("ucore.ui.select")

local M = {}
local close_window
local normalize
local dirname
local create_relative_path

local providers = {
	Project = "ucore.explorer.providers.project",
	Source = "ucore.explorer.providers.source",
	Config = "ucore.explorer.providers.config",
}

-- Return the active explorer configuration block.
-- 返回当前生效的 explorer 配置块。
local function explorer_config()
	return config.values.explorer or {}
end

-- Define one small highlight group used by the explorer border.
-- 定义 explorer 边框使用的小型高亮组。
local function setup_highlights()
	vim.api.nvim_set_hl(0, "UCorePanelBorder", { fg = "#3F3F46" })
end

-- Compute the minimum width needed to render the tab header safely.
-- 计算安全渲染标签页标题所需的最小宽度。
local function minimum_width_for_tabs()
	local text = " q <  " .. table.concat(state.tabs(), " | ") .. "  > e "
	return vim.fn.strdisplaywidth(text) + 2
end

-- Create the shared scratch buffer used by the explorer window.
-- 创建 explorer 窗口复用的临时缓冲区。
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

-- Close other tree plugins when configured to keep only one explorer open.
-- 当配置要求只保留一个目录树时，关闭其他 tree 插件。
local function close_other_explorers()
	if explorer_config().close_other_explorers ~= true then
		return
	end
	pcall(vim.cmd, "NvimTreeClose")
	pcall(vim.cmd, "Neotree close")
end

-- Open and configure the floating explorer window when it is not already visible.
-- 在 explorer 尚未可见时打开并配置它的浮动窗口。
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
	width = math.min(width, math.max(vim.o.columns - 8, minimum_width_for_tabs()))

	local height = cfg.height or math.floor(vim.o.lines * 0.72)
	height = math.max(height, cfg.min_height or 0)
	if cfg.max_height then
		height = math.min(height, cfg.max_height)
	end
	height = math.min(height, math.max(vim.o.lines - 6, 8))

	local row = math.max(1, math.floor((vim.o.lines - height) / 2) - 1)
	local col = math.max(0, math.floor((vim.o.columns - width) / 2))

	state.anchor_win = previous_win
	state.win = vim.api.nvim_open_win(ensure_buffer(), true, {
		relative = "editor",
		row = row,
		col = col,
		width = width,
		height = height,
		border = "rounded",
		style = "minimal",
		title = " UCore Explorer ",
		title_pos = "center",
		noautocmd = true,
	})
	vim.wo[state.win].number = false
	vim.wo[state.win].relativenumber = false
	vim.wo[state.win].signcolumn = "no"
	vim.wo[state.win].foldcolumn = "0"
	vim.wo[state.win].wrap = false
	vim.wo[state.win].spell = false
	vim.wo[state.win].cursorline = true
	vim.wo[state.win].winfixwidth = true
	vim.wo[state.win].winfixheight = true
	vim.wo[state.win].winbar = ""
	vim.wo[state.win].winhl =
		"Normal:Normal,SignColumn:Normal,EndOfBuffer:Normal,FloatBorder:UCorePanelBorder"
	if explorer_config().auto_focus == false and vim.api.nvim_win_is_valid(previous_win) then
		vim.api.nvim_set_current_win(previous_win)
	end
	return state.win
end

-- Resolve the provider module for one explorer tab label.
-- 解析某个 explorer 标签对应的 provider 模块。
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

-- Load the current tab tree from its provider and seed root expansion state.
-- 从当前标签的 provider 加载树，并初始化根节点展开状态。
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

-- Rebuild the visible explorer rows from the current tree and search filter.
-- 根据当前树和搜索过滤条件重建可见的 explorer 行。
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

-- Re-render the explorer contents from current state.
-- 基于当前状态重新渲染 explorer 内容。
local function redraw()
	rebuild_visible()
	render.render()
end

-- Force one tree reload, optionally clearing expansion state first.
-- 强制重载当前树，并可选择先清空展开状态。
local function reload_tree(opts)
	opts = opts or {}
	if opts.reset_expanded then
		state.expanded = {}
	end
	state.tree = nil
	load_tree()
	redraw()
end

-- Return the tree item under the explorer cursor.
-- 返回 explorer 光标所在位置的树节点项。
local function current_item()
	if not state.is_valid_win() then
		return nil
	end
	local row = vim.api.nvim_win_get_cursor(state.win)[1]
	return state.line_items[row - 4]
end

-- Resolve the target directory represented by one tree item.
-- 解析一个树节点项所代表的目标目录。
local function target_directory_for_item(item)
	local node = item and item.node
	if not node then
		return nil
	end

	if node.type == "directory" and node.path then
		return normalize(node.path)
	end

	if node.path then
		return dirname(node.path)
	end

	return nil
end

-- Open one file in the most sensible non-explorer window and close the explorer.
-- 在最合理的非 explorer 窗口中打开文件并关闭 explorer。
local function open_file(path)
	local target_win = state.anchor_win
	if not (target_win and vim.api.nvim_win_is_valid(target_win) and target_win ~= state.win) then
		local previous_win = vim.fn.win_getid(vim.fn.winnr("#"))
		if previous_win and vim.api.nvim_win_is_valid(previous_win) and previous_win ~= state.win then
			target_win = previous_win
		end
	end

	close_window()

	if target_win and vim.api.nvim_win_is_valid(target_win) then
		vim.api.nvim_set_current_win(target_win)
	end
	vim.cmd("edit " .. vim.fn.fnameescape(path))
end

-- Normalize one path to slash-separated form.
-- 将一个路径规范为正斜杠形式。
-- Normalize one path to slash-separated form.
-- 将一个路径规范为正斜杠形式。
normalize = function(path)
	return path and path:gsub("\\", "/") or nil
end

-- Return whether one normalized path sits under the given prefix.
-- 返回一个规范路径是否位于给定前缀之下。
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

-- Return the normalized parent directory for one path.
-- 返回一个路径的规范化父目录。
-- Return the normalized parent directory for one path.
-- 返回一个路径的规范化父目录。
dirname = function(path)
	return normalize(vim.fn.fnamemodify(path, ":h"))
end

-- Join two path segments and normalize the result.
-- 连接两个路径片段并规范化结果。
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

-- Resolve the current context file path from the anchor window or current buffer.
-- 从锚点窗口或当前缓冲区解析当前上下文文件路径。
local function current_context_path()
	if state.anchor_win and vim.api.nvim_win_is_valid(state.anchor_win) then
		local bufnr = vim.api.nvim_win_get_buf(state.anchor_win)
		if bufnr and vim.api.nvim_buf_is_valid(bufnr) then
			return normalize(vim.api.nvim_buf_get_name(bufnr))
		end
	end

	return normalize(vim.api.nvim_buf_get_name(0))
end

-- Expand the tree path needed to reveal one target file path.
-- 展开树中需要的路径，以显示目标文件。
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

-- Move the explorer cursor onto one already revealed file path.
-- 将 explorer 光标移动到已经展开显示的文件路径上。
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

-- Reveal the current context file inside the explorer tree.
-- 在 explorer 树中展开并定位当前上下文文件。
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

-- Return whether Telescope is available for the picker-style explorer UI.
-- 返回 Telescope 是否可用，以启用选择器式 explorer UI。
local function telescope_available()
	return pcall(require, "telescope.pickers")
end

-- Build the tree prefix used by one picker entry.
-- 构造一个 picker 条目的树形前缀。
local function picker_tree_prefix(item)
	if item.depth == 0 then
		return ""
	end
	return (item.prefix or "") .. (item.is_last and "└─ " or "├─ ")
end

-- Build the display text for one picker entry.
-- 构造一个 picker 条目的显示文本。
local function picker_entry_text(item)
	local node = item.node or {}
	local prefix = picker_tree_prefix(item)

	if node.type == "directory" then
		local symbol = state.is_expanded(node) and "▾" or "▸"
		return string.format("%s%s %s", prefix, symbol, tostring(node.label or ""))
	end

	if node.type == "message" then
		return string.format("%s%s", prefix, tostring(node.message or node.label or ""))
	end

	return string.format("%s  %s", prefix, tostring(node.label or ""))
end

-- Build the fuzzy-search ordinal text for one picker entry.
-- 构造一个 picker 条目的模糊搜索排序文本。
local function picker_entry_ordinal(item)
	local node = item.node or {}
	return table.concat({
		tostring(node.label or ""),
		tostring(node.path or ""),
		tostring(node.type or ""),
		tostring(node.message or ""),
	}, " ")
end

-- Normalize preview lines into a newline-safe list for scratch buffers.
-- 将预览文本规范成适合临时缓冲区的安全行列表。
local function sanitize_preview_lines(lines)
	local result = {}
	for _, line in ipairs(lines or {}) do
		line = tostring(line or ""):gsub("\r\n", "\n"):gsub("\r", "\n")
		for _, part in ipairs(vim.split(line, "\n", { plain = true })) do
			table.insert(result, part)
		end
	end
	return #result > 0 and result or { "" }
end

-- Render a directory-style preview into one picker preview buffer.
-- 将目录预览渲染到一个 picker 预览缓冲区中。
local function preview_directory(node, bufnr)
	if node.type == "message" then
		vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, sanitize_preview_lines({
			"UCore Explorer",
			"",
			tostring(node.message or node.label or ""),
		}))
		vim.bo[bufnr].filetype = "text"
		return
	end

	tree.ensure_children(node)

	local directories = 0
	local files = 0
	local lines = {
		"UCore Explorer",
		"",
		"Directory: " .. tostring(node.path or node.label or ""),
		"",
	}

	for _, child in ipairs(node.children or {}) do
		if child.type == "directory" then
			directories = directories + 1
		elseif child.type == "file" then
			files = files + 1
		end
	end

	table.insert(lines, string.format("Children: %d directories, %d files", directories, files))
	table.insert(lines, "")
	table.insert(lines, "Entries:")

	for index, child in ipairs(node.children or {}) do
		if index > 180 then
			table.insert(lines, string.format("... and %d more", #node.children - index + 1))
			break
		end

		local marker = child.type == "directory" and "[D]" or "[F]"
		table.insert(lines, string.format("%s %s", marker, tostring(child.label or "")))
	end

	vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, sanitize_preview_lines(lines))
	vim.bo[bufnr].filetype = "text"
end

-- Render a file preview into one picker preview buffer when readable.
-- 当文件可读时，将其内容渲染到 picker 预览缓冲区。
local function preview_file(path, bufnr)
	if not path or path == "" or vim.fn.filereadable(path) ~= 1 then
		return false
	end

	local ok, lines = pcall(vim.fn.readfile, path, "", 250)
	if not ok then
		return false
	end

	vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, sanitize_preview_lines(lines))
	vim.bo[bufnr].filetype = vim.filetype.match({ filename = path }) or ""
	return true
end

-- Convert the visible explorer rows into Telescope picker entries.
-- 将当前可见 explorer 行转换为 Telescope picker 条目。
local function make_picker_entries()
	local entries = {}
	for _, item in ipairs(state.visible or {}) do
		local node = item.node or {}
		table.insert(entries, {
			value = item,
			display = picker_entry_text(item),
			ordinal = picker_entry_ordinal(item),
			filename = node.type == "file" and node.path or nil,
			path = node.path,
		})
	end
	return entries
end

-- Open the explorer using Telescope as a picker + preview workflow.
-- 使用 Telescope 的选择器和预览工作流打开 explorer。
local function pick_telescope_explorer()
	local pickers = require("telescope.pickers")
	local finders = require("telescope.finders")
	local previewers = require("telescope.previewers")
	local actions = require("telescope.actions")
	local action_state = require("telescope.actions.state")
	local conf = require("telescope.config").values

	close_window()
	state.anchor_win = vim.api.nvim_get_current_win()
	if not state.tree then
		load_tree()
	end
	reveal_current_file()
	rebuild_visible()

	local picker_ref

	-- Rebuild one Telescope finder from the current explorer rows.
	-- 基于当前 explorer 行重建一个 Telescope finder。
	local function make_finder()
		return finders.new_table({
			results = make_picker_entries(),
			entry_maker = function(entry)
				return entry
			end,
		})
	end

	-- Refresh picker results after state changes such as expand, search, or tab switch.
	-- 在展开、搜索或切换标签后刷新 picker 结果。
	local function refresh_picker(prompt_bufnr)
		rebuild_visible()
		if picker_ref then
			picker_ref.results_title = state.tab
			pcall(function()
				picker_ref:refresh(make_finder(), { reset_prompt = false })
			end)
		end
		if prompt_bufnr then
			vim.schedule(function()
				local ok = pcall(action_state.get_current_picker, prompt_bufnr)
				if ok then
					picker_ref = action_state.get_current_picker(prompt_bufnr)
				end
			end)
		end
	end

	-- Toggle the selected directory entry inside the picker result list.
	-- 在 picker 结果列表中切换当前选中目录的展开状态。
	local function toggle_selected_directory(prompt_bufnr)
		local selection = action_state.get_selected_entry()
		local item = selection and selection.value
		local node = item and item.node
		if not (node and node.type == "directory") then
			return false
		end

		if state.is_expanded(node) then
			state.set_expanded(node, false)
		else
			tree.expand_directory(node, state)
		end

		refresh_picker(prompt_bufnr)
		return true
	end

	-- Switch picker tabs and rebuild the visible tree for the new provider.
	-- 切换 picker 标签，并为新的 provider 重建可见树。
	local function switch_tab_in_picker(delta, prompt_bufnr)
		state.set_tab_by_delta(delta)
		state.tree = nil
		load_tree()
		reveal_current_file()
		refresh_picker(prompt_bufnr)
	end

	-- Create a file or directory relative to the currently selected picker entry.
	-- 基于当前选中的 picker 条目创建文件或目录。
	local function create_from_selection(prompt_bufnr, kind)
		local selection = action_state.get_selected_entry()
		local item = selection and selection.value
		local target_dir = target_directory_for_item(item)
		actions.close(prompt_bufnr)
		vim.schedule(function()
			create_relative_path(kind, nil, { target_dir = target_dir })
		end)
	end

	-- Register matching left/right tab-switch mappings for insert and normal mode.
	-- 同时为插入模式和普通模式注册左右标签切换映射。
	local function map_switch_tab(map, delta, prompt_bufnr)
		map("i", delta < 0 and "<Left>" or "<Right>", function()
			switch_tab_in_picker(delta, prompt_bufnr)
		end)
		map("n", delta < 0 and "<Left>" or "<Right>", function()
			switch_tab_in_picker(delta, prompt_bufnr)
		end)
	end

	picker_ref = pickers.new({}, {
		prompt_title = "UCore Explorer",
		results_title = state.tab,
		preview_title = "Preview",
		layout_strategy = "horizontal",
		layout_config = {
			width = 0.94,
			height = 0.88,
			prompt_position = "top",
			horizontal = {
				preview_width = 0.58,
			},
		},
		sorting_strategy = "ascending",
		finder = make_finder(),
		previewer = previewers.new_buffer_previewer({
			define_preview = function(self, entry)
				local item = entry and entry.value
				local node = item and item.node
				if not node then
					return
				end

				if node.type == "file" then
					if preview_file(node.path, self.state.bufnr) then
						return
					end
				end

				preview_directory(node, self.state.bufnr)
			end,
		}),
		sorter = conf.generic_sorter({}),
		attach_mappings = function(prompt_bufnr, map)
			local function close_picker()
				actions.close(prompt_bufnr)
			end

			map("i", "<Esc>", function()
				close_picker()
			end)
			map("n", "<Esc>", close_picker)

			actions.select_default:replace(function()
				if toggle_selected_directory(prompt_bufnr) then
					return
				end

				local selection = action_state.get_selected_entry()
				actions.close(prompt_bufnr)

				local item = selection and selection.value
				local node = item and item.node
				if node and node.type == "file" and node.path then
					open_file(node.path)
				end
			end)

			map_switch_tab(map, -1, prompt_bufnr)
			map_switch_tab(map, 1, prompt_bufnr)
			map("n", "a", function()
				create_from_selection(prompt_bufnr, "file")
			end)
			map("n", "A", function()
				create_from_selection(prompt_bufnr, "directory")
			end)

			return true
		end,
	})

	picker_ref:find()
end

local function current_target_directory()
	if state.is_valid_win() then
		local target_dir = target_directory_for_item(current_item())
		if target_dir then
			return target_dir
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

local function ensure_project_target_dir(target_dir)
	target_dir = normalize(target_dir) or current_target_directory()
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

-- Prompt for and create a file or directory relative to the current explorer target.
-- 基于当前 explorer 目标提示并创建文件或目录。
create_relative_path = function(kind, suffix, opts)
	opts = opts or {}
	local target_dir, _root = ensure_project_target_dir(opts.target_dir)
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
			if type(opts.on_created) == "function" then
				opts.on_created(path)
			end
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
		if type(opts.on_created) == "function" then
			opts.on_created(path)
			return
		end
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

local function switch_tab(delta)
	state.set_tab_by_delta(delta)
	reload_tree()
end

local function refresh_current()
	reload_tree()
end

local function refresh_all()
	reload_tree({
		reset_expanded = true,
	})
end

-- Close the explorer window and clear cached window anchors.
-- 关闭 explorer 窗口并清理缓存的窗口锚点。
close_window = function()
	if state.is_valid_win() then
		vim.api.nvim_win_close(state.win, true)
	end
	state.win = nil
	state.anchor_win = nil
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
	map("a", function()
		create_relative_path("file")
	end, "UCore Explorer new file")
	map("A", function()
		create_relative_path("directory")
	end, "UCore Explorer new directory")
	map("r", refresh_current, "UCore Explorer refresh tab")
	map("R", refresh_all, "UCore Explorer refresh all")
end

function M.open()
	if telescope_available() then
		pick_telescope_explorer()
		return
	end

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
	if telescope_available() then
		M.open()
		return
	end

	if state.is_valid_win() then
		reveal_current_file()
		vim.api.nvim_set_current_win(state.win)
	else
		M.open()
	end
end

function M.toggle()
	if telescope_available() then
		M.open()
		return
	end

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
