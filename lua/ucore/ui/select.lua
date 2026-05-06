local config = require("ucore.config")

local M = {}
local FIND_PREVIEW_MAX_LINES = 200
local FIND_PAGE_SIZE = 50
local FIND_DEBOUNCE_MS = 400
local FIND_MIN_QUERY_LENGTH = 2

-- Check whether a Lua module can be required.
-- 检查某个 Lua 模块是否可用。
local function has_module(name)
	local ok = pcall(require, name)
	return ok
end

local function sanitize_buffer_lines(lines)
	local result = {}
	for _, line in ipairs(lines or {}) do
		line = tostring(line or "")
		line = line:gsub("\r\n", "\n"):gsub("\r", "\n")
		for _, part in ipairs(vim.split(line, "\n", { plain = true })) do
			table.insert(result, part)
		end
	end

	if #result == 0 then
		return { "" }
	end

	return result
end

-- Pick the best available picker backend.
-- 选择当前可用的最佳 picker 后端。
local function picker_backend()
	local ui_config = config.values.ui or {}
	local requested = ui_config.picker or "auto"

	if requested == "vim" then
		return "vim"
	end

	if requested == "fzf-lua" and has_module("fzf-lua") then
		return "fzf-lua"
	end

	if requested == "telescope" and has_module("telescope.pickers") then
		return "telescope"
	end

	if requested == "auto" then
		if has_module("fzf-lua") then
			return "fzf-lua"
		end

		if has_module("telescope.pickers") then
			return "telescope"
		end
	end

	return "vim"
end

-- Build stable display entries for picker backends.
-- 为 picker 后端构造稳定展示项。
local function build_entries(items, format_item)
	local entries = {}

	for index, item in ipairs(items or {}) do
		local display = format_item and format_item(item) or tostring(item)
		display = tostring(display):gsub("\n", "  ")

		table.insert(entries, {
			index = index,
			item = item,
			display = string.format("%04d  %s", index, display),
			ordinal = display,
		})
	end

	return entries
end

-- Trim a string to a display width while keeping the useful tail.
-- 按显示宽度裁剪字符串，并保留更有用的尾部路径信息。
local function truncate_left(text, max_width)
	text = tostring(text or "")

	if vim.fn.strdisplaywidth(text) <= max_width then
		return text
	end

	local marker = "..."
	local target = math.max(1, max_width - vim.fn.strdisplaywidth(marker))
	local result = ""

	for index = #text, 1, -1 do
		local candidate = text:sub(index)
		if vim.fn.strdisplaywidth(candidate) >= target then
			result = candidate
			break
		end
	end

	while vim.fn.strdisplaywidth(result) > target do
		result = result:sub(2)
	end

	return marker .. result
end

-- Pad a string on the right using display width instead of byte length.
-- 按显示宽度右侧补空格，避免中文或宽字符导致列错位。
local function pad_right(text, width)
	text = tostring(text or "")
	local padding = math.max(0, width - vim.fn.strdisplaywidth(text))
	return text .. string.rep(" ", padding)
end

local function normalize_path(path)
	return tostring(path or ""):gsub("\\", "/"):lower()
end

-- Prefer a cwd-relative path in picker displays.
-- picker 里优先显示相对当前工作目录的路径。
local function display_path(path)
	path = tostring(path or ""):gsub("\\", "/")
	local cwd = vim.loop.cwd()

	if cwd and cwd ~= "" then
		cwd = cwd:gsub("\\", "/")
		if path:lower():sub(1, #cwd) == cwd:lower() then
			path = path:sub(#cwd + 2)
		end
	end

	return path
end

local function compact_path(path, width)
	return truncate_left(display_path(path), width)
end

local function current_buffer_path()
	return normalize_path(vim.api.nvim_buf_get_name(0))
end

local function normalize_source(source)
	source = tostring(source or "")

	if source == "" then
		return ""
	end

	return source:lower()
end

local function normalize_kind(kind)
	kind = tostring(kind or "")

	local lowered = kind:lower()
	if lowered == "uclass" then
		return "class"
	end
	if lowered == "ustruct" then
		return "struct"
	end
	if lowered == "uenum" then
		return "enum"
	end

	return kind
end

local function find_category(item)
	local kind = normalize_kind(item.symbol_type or item.type):lower()

	if item.asset_path or kind == "asset" then
		return "asset"
	end
	if kind == "file" then
		return "file"
	end
	if kind == "text" then
		return "text"
	end
	if kind == "config" then
		return "config"
	end
	if kind == "module" then
		return "module"
	end
	if kind == "class" or kind == "struct" or kind == "enum" then
		return kind
	end
	if kind:find("function", 1, true) or kind:find("method", 1, true) then
		return "function"
	end
	if kind:find("property", 1, true) or kind:find("member", 1, true) then
		return "member"
	end

	return "symbol"
end

local function find_category_label(item)
	return ({
		asset = "[asset]",
		class = "[class]",
		config = "[config]",
		enum = "[enum]",
		file = "[file]",
		["function"] = "[func]",
		member = "[member]",
		module = "[module]",
		struct = "[struct]",
		symbol = "[symbol]",
		text = "[text]",
	})[find_category(item)] or "[symbol]"
end

local function find_group(item)
	local category = find_category(item)

	if category == "module" then
		return "Modules"
	end
	if category == "asset" then
		return "Assets"
	end
	if category == "file" then
		return "Files"
	end
	if category == "text" then
		return "Text"
	end
	if category == "config" then
		return "Config"
	end

	return "Code"
end

local function find_group_order(item)
	return ({
		Text = 1,
		Code = 2,
		Files = 3,
		Modules = 4,
		Assets = 5,
		Config = 6,
	})[find_group(item)] or 9
end

local function find_display_location(item, path, line)
	path = tostring(path or "")

	if path == "" then
		return ""
	end

	if item.asset_path then
		return vim.fn.fnamemodify(path, ":t")
	end

	local filename = vim.fn.fnamemodify(path:gsub("\\", "/"), ":t")
	if filename == "" then
		filename = path
	end

	if item.type == "config" and item.config_section then
		return string.format("%s [%s]", filename, tostring(item.config_section))
	end

	if item.type == "text" then
		return string.format("%s:%d", filename, line)
	end

	return string.format("%s:%d", filename, line)
end

local function find_search_text(item, name, kind, label, path)
	path = tostring(path or "")
	local normalized_path = path:gsub("\\", "/")

	return table.concat({
		name,
		kind,
		label,
		tostring(item.text or ""),
		find_group(item),
		tostring(item.class_name or ""),
		tostring(item.module_name or ""),
		tostring(item.config_section or ""),
		tostring(item.config_value or ""),
		tostring(item.config_file or ""),
		tostring(item.asset_path or ""),
		vim.fn.fnamemodify(normalized_path, ":t"),
		display_path(normalized_path),
		normalized_path,
		path,
	}, " ")
end

local function find_item_key(item)
	local path = normalize_path(item.path or item.file_path or item.asset_path or "")
	local line = tonumber(item.line or item.line_number or 1) or 1
	local name = tostring(item.name or item.symbol_name or "")
	local kind = tostring(item.symbol_type or item.type or "")

	return table.concat({ path, line, name, kind }, "\t")
end

local function find_item_score(item, current)
	local source = normalize_source(item.source)
	local kind = normalize_kind(item.symbol_type or item.type):lower()
	local path = normalize_path(item.path or item.file_path or item.asset_path or "")
	local score = 0

	if source == "project" then
		score = score - 300
	elseif source == "engine" then
		score = score + 300
	end

	score = score + (find_group_order(item) * 100)

	if current ~= "" and path == current then
		score = score - 120
	end

	if kind == "class" or kind == "struct" or kind == "enum" then
		score = score - 80
	elseif kind == "module" then
		score = score - 60
	elseif kind:find("function", 1, true) or kind:find("method", 1, true) then
		score = score - 40
	elseif kind == "config" then
		score = score + 40
	elseif kind == "asset" then
		score = score + 60
	elseif kind:find("property", 1, true) or kind:find("member", 1, true) then
		score = score + 20
	end

	return score
end

local function prepare_find_items(items)
	local current = current_buffer_path()
	if type(items) == "table" and items.__ucore_prepared == true and items.__ucore_prepared_for == current then
		return items
	end

	local seen = {}
	local result = {}

	for _, item in ipairs(items or {}) do
		local key = find_item_key(item)
		if not seen[key] then
			seen[key] = true
			table.insert(result, item)
		end
	end

	for _, item in ipairs(result) do
		item._ucore_find_sort_name = tostring(item.name or item.symbol_name or "")
		item._ucore_find_sort_path = display_path(item.path or item.file_path or item.asset_path or "")
		item._ucore_find_score = find_item_score(item, current)
	end

	table.sort(result, function(left, right)
		local left_score = left._ucore_find_score or 0
		local right_score = right._ucore_find_score or 0

		if left_score ~= right_score then
			return left_score < right_score
		end

		local left_group = find_group(left)
		local right_group = find_group(right)
		if left_group ~= right_group then
			return left_group < right_group
		end

		local left_name = left._ucore_find_sort_name or tostring(left.name or left.symbol_name or "")
		local right_name = right._ucore_find_sort_name or tostring(right.name or right.symbol_name or "")
		if left_name ~= right_name then
			return left_name < right_name
		end

		return (left._ucore_find_sort_path or display_path(left.path or left.file_path or left.asset_path or ""))
			< (right._ucore_find_sort_path or display_path(right.path or right.file_path or right.asset_path or ""))
	end)

	for index, item in ipairs(result) do
		local name = tostring(item.name or item.symbol_name or "<unknown>")
		local kind = normalize_kind(item.symbol_type or item.type)
		local source = normalize_source(item.source)
		local path = tostring(item.path or item.file_path or item.asset_path or "")
		local line = tonumber(item.line or item.line_number or 1) or 1
		local label = find_category_label(item)
		local group = find_group(item)
		local source_label = source ~= "" and source or "index"
		local location = find_display_location(item, path, line)

		item._ucore_find_index = index
		item._ucore_find_path = path
		item._ucore_find_line = line
		item._ucore_find_text = name
		item._ucore_find_display = string.format(
			"%s  %s  %s  %s  %s",
			pad_right(group, 7),
			pad_right(label, 9),
			pad_right(truncate_left(name, 34), 34),
			pad_right(source_label, 7),
			location
		)
		item._ucore_find_ordinal = find_search_text(item, name, kind, label, path)
	end

	result.__ucore_prepared = true
	result.__ucore_prepared_for = current

	return result
end

local function apply_find_item_metadata(item, index, current)
	current = current or current_buffer_path()
	local name = tostring(item.name or item.symbol_name or "<unknown>")
	if item.type == "text" and tostring(item.text or "") ~= "" then
		name = tostring(item.text)
	end
	local kind = normalize_kind(item.symbol_type or item.type)
	local source = normalize_source(item.source)
	local path = tostring(item.path or item.file_path or item.asset_path or "")
	local line = tonumber(item.line or item.line_number or 1) or 1
	local label = find_category_label(item)
	local group = find_group(item)
	local source_label = source ~= "" and source or "index"
	local location = find_display_location(item, path, line)

	item._ucore_find_index = index
	item._ucore_find_path = path
	item._ucore_find_line = line
	item._ucore_find_text = name
	item._ucore_find_score = find_item_score(item, current)
	item._ucore_find_display = string.format(
		"%s  %s  %s  %s  %s",
		pad_right(group, 7),
		pad_right(label, 9),
		pad_right(truncate_left(name, 34), 34),
		pad_right(source_label, 7),
		location
	)
	item._ucore_find_ordinal = find_search_text(item, name, kind, label, path)
end

-- Prepare live search results in backend order, then let the live filter add
-- group/source priority when ranking the visible rows.
local function prepare_find_items_in_order(items)
	local current = current_buffer_path()
	local seen = {}
	local result = {}

	for _, item in ipairs(items or {}) do
		local key = find_item_key(item)
		if not seen[key] then
			seen[key] = true
			table.insert(result, item)
		end
	end

	for index, item in ipairs(result) do
		apply_find_item_metadata(item, index, current)
	end

	return result
end

local function make_find_entry(item)
	return {
		value = item,
		display = item._ucore_find_display or tostring(item.name or item.symbol_name or "<unknown>"),
		ordinal = item._ucore_find_ordinal or tostring(item.name or item.symbol_name or "<unknown>"),
		filename = item._ucore_find_path or tostring(item.path or item.file_path or item.asset_path or ""),
		path = item._ucore_find_path or tostring(item.path or item.file_path or item.asset_path or ""),
		lnum = item._ucore_find_line or tonumber(item.line or item.line_number or 1) or 1,
		col = 1,
		text = item._ucore_find_text or tostring(item.name or item.symbol_name or "<unknown>"),
	}
end

local function filter_static_find_items(items, query, limit)
	local prepared = prepare_find_items(items or {})
	query = vim.trim(tostring(query or "")):lower()
	limit = limit or FIND_PAGE_SIZE

	if query == "" then
		local result = {}
		for index = 1, math.min(#prepared, limit) do
			table.insert(result, prepared[index])
		end
		return result
	end

	local tokens = vim.split(query, "%s+", { trimempty = true })
	local result = {}

	for _, item in ipairs(prepared) do
		local ordinal = tostring(item._ucore_find_ordinal or ""):lower()
		local matched = true
		for _, token in ipairs(tokens) do
			if not ordinal:find(token, 1, true) then
				matched = false
				break
			end
		end

		if matched then
			table.insert(result, item)
			if #result >= limit then
				break
			end
		end
	end

	return result
end

local function fuzzy_token_score(text, token)
	text = tostring(text or ""):lower()
	token = tostring(token or ""):lower()

	if token == "" then
		return 0
	end

	local exact_start = text:find(token, 1, true)
	if exact_start then
		return exact_start == 1 and 0 or exact_start
	end

	local cursor = 1
	local score = 1000
	for index = 1, #token do
		local char = token:sub(index, index)
		local found = text:find(char, cursor, true)
		if not found then
			return nil
		end

		score = score + (found - cursor)
		cursor = found + 1
	end

	return score
end

local function find_query_tokens(query)
	query = vim.trim(tostring(query or "")):lower()
	return vim.split(query, "%s+", { trimempty = true })
end

local function filter_live_find_items(items, query, limit)
	local prepared = prepare_find_items_in_order(items or {})
	query = vim.trim(tostring(query or ""))
	limit = limit or FIND_PAGE_SIZE

	if query == "" then
		local result = {}
		for index = 1, math.min(#prepared, limit) do
			table.insert(result, prepared[index])
		end
		return result
	end

	local tokens = find_query_tokens(query)
	if vim.tbl_isempty(tokens) then
		return prepared
	end

	local ranked = {}
	for _, item in ipairs(prepared) do
		local ordinal = tostring(item._ucore_find_ordinal or "")
		local score = tonumber(item._ucore_find_score or 0) or 0
		local matched = true

		for _, token in ipairs(tokens) do
			local token_score = fuzzy_token_score(ordinal, token)
			if not token_score then
				matched = false
				break
			end
			score = score + token_score
		end

		if matched then
			table.insert(ranked, {
				item = item,
				score = score,
				index = item._ucore_find_index or #ranked + 1,
			})
		end
	end

	table.sort(ranked, function(left, right)
		if left.score ~= right.score then
			return left.score < right.score
		end
		return left.index < right.index
	end)

	local result = {}
	for index = 1, math.min(#ranked, limit) do
		table.insert(result, ranked[index].item)
	end

	return result
end

local function reference_label(kind)
	kind = tostring(kind or "unknown")

	return ({
		declaration = "Declaration",
		definition = "Definition",
		read = "Reference",
		write = "Assignment",
		call = "Call",
		unknown = "Reference",
	})[kind] or "Reference"
end

local function open_reference(item)
	local path = item.path or item.file_path
	local line = tonumber(item.line or item.line_number or 1) or 1
	local col = tonumber(item.col or item.column or 0) or 0

	if path and path ~= vim.NIL and vim.fn.filereadable(path) == 1 then
		vim.cmd.edit(vim.fn.fnameescape(path))
		local last_line = vim.api.nvim_buf_line_count(0)
		line = math.max(1, math.min(line, last_line))
		local line_text = vim.api.nvim_buf_get_lines(0, line - 1, line, false)[1] or ""
		col = math.max(0, math.min(col, #line_text))
		vim.api.nvim_win_set_cursor(0, { line, col })
		vim.cmd("normal! zz")
	else
		print(vim.inspect(item))
	end
end

local function open_source_item(item)
	if item.type == "asset" or item.asset_path then
		local asset_path = tostring(item.asset_path or item.path or "")
		vim.fn.setreg("+", asset_path)
		vim.notify("Copied asset path: " .. asset_path)
		return
	end

	local path = item.path or item.file_path
	local line = tonumber(item.line or item.line_number or 1) or 1
	local col = tonumber(item.col or item.column or 0) or 0

	if path and path ~= vim.NIL and vim.fn.filereadable(path) == 1 then
		vim.cmd.edit(vim.fn.fnameescape(path))
		local last_line = vim.api.nvim_buf_line_count(0)
		line = math.max(1, math.min(line, last_line))
		local line_text = vim.api.nvim_buf_get_lines(0, line - 1, line, false)[1] or ""
		col = math.max(0, math.min(col, #line_text))
		vim.api.nvim_win_set_cursor(0, { line, col })
		vim.cmd("normal! zz")
	else
		print(vim.inspect(item))
	end
end

local function preview_find_item(entry, bufnr)
	local item = entry.value or {}
	local path = item.path or item.file_path

	if path and path ~= vim.NIL and vim.fn.filereadable(path) == 1 then
		local ok, lines = pcall(vim.fn.readfile, path, "", FIND_PREVIEW_MAX_LINES)
		if ok then
			vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)
			vim.bo[bufnr].filetype = vim.filetype.match({ filename = path }) or ""
			vim.b[bufnr].ucore_preview_path = path
			return
		end
	end

	local lines = {}
	if item.asset_path then
		lines = {
			"UCore asset",
			"",
			tostring(item.asset_path),
			"",
			"Press <CR> to copy the asset path.",
		}
	elseif item.type == "config" then
		lines = {
			"UCore config",
			"",
			"Section: " .. tostring(item.config_section or ""),
			"Key:     " .. tostring(item.name or ""),
			"Value:   " .. tostring(item.config_value or ""),
			"Source:  " .. tostring(item.config_file or item.source or ""),
		}
	else
		lines = vim.split(vim.inspect(item), "\n", { plain = true })
	end

	vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, sanitize_buffer_lines(lines))
	vim.bo[bufnr].filetype = "text"
	vim.b[bufnr].ucore_preview_path = nil
end

local function preview_find_file(previewer, entry)
	local path = entry.filename
	local bufnr = previewer.state.bufnr

	if not path or path == "" or vim.fn.filereadable(path) ~= 1 then
		return false
	end

	if vim.b[bufnr].ucore_preview_path == path then
		return true
	end

	local ok, lines = pcall(vim.fn.readfile, path, "", FIND_PREVIEW_MAX_LINES)
	if not ok then
		return false
	end

	vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, sanitize_buffer_lines(lines))
	vim.bo[bufnr].filetype = vim.filetype.match({ filename = path }) or ""
	vim.b[bufnr].ucore_preview_path = path
	return true
end

-- Open the built-in vim.ui.select picker.
-- 打开内置 vim.ui.select 选择器。
local function pick_vim(title, items, format_item, on_choice)
	vim.ui.select(items, {
		prompt = title,
		format_item = format_item,
	}, function(choice)
		if choice then
			on_choice(choice)
		end
	end)
end

-- Open fzf-lua picker.
-- 打开 fzf-lua 选择器。
local function pick_fzf(title, items, format_item, on_choice)
	local fzf = require("fzf-lua")
	local entries = build_entries(items, format_item)
	local lines = vim.tbl_map(function(entry)
		return entry.display
	end, entries)

	fzf.fzf_exec(lines, {
		prompt = title .. "> ",
		actions = {
			["default"] = function(selected)
				local line = selected and selected[1]
				local index = line and tonumber(line:match("^(%d+)"))
				local entry = index and entries[index]

				if entry then
					on_choice(entry.item)
				end
			end,
		},
	})
end

-- Open telescope picker.
-- 打开 telescope 选择器。
local function pick_telescope(title, items, format_item, on_choice)
	local pickers = require("telescope.pickers")
	local finders = require("telescope.finders")
	local conf = require("telescope.config").values
	local actions = require("telescope.actions")
	local action_state = require("telescope.actions.state")

	local entries = build_entries(items, format_item)

	pickers
		.new({}, {
			prompt_title = title,
			finder = finders.new_table({
				results = entries,
				entry_maker = function(entry)
					return {
						value = entry,
						display = entry.display,
						ordinal = entry.ordinal,
					}
				end,
			}),
			sorter = conf.generic_sorter({}),
			attach_mappings = function(prompt_bufnr)
				actions.select_default:replace(function()
					local selection = action_state.get_selected_entry()
					actions.close(prompt_bufnr)

					if selection and selection.value then
						on_choice(selection.value.item)
					end
				end)

				return true
			end,
		})
		:find()
end

-- Open references using a grep-like Telescope layout:
-- left side lists locations, right side previews the whole file.
-- 使用类似全局搜索的 Telescope 布局：
-- 左侧列出定位信息，右侧预览整个文件内容。
local function pick_telescope_references(references)
	local pickers = require("telescope.pickers")
	local finders = require("telescope.finders")
	local conf = require("telescope.config").values
	local actions = require("telescope.actions")
	local action_state = require("telescope.actions.state")

	pickers
		.new({}, {
			prompt_title = "UCore references",
			finder = finders.new_table({
				results = references,
				entry_maker = function(item)
					local path = tostring(item.path or item.file_path or "")
					local line = tonumber(item.line or item.line_number or 1) or 1
					local col = tonumber(item.col or item.column or 0) or 0
					local context = tostring(item.context or item.text or ""):gsub("^%s+", "")
					local label = reference_label(item.kind)
					local location = string.format("[%s] %s:%d:%d", label, display_path(path), line, col + 1)

					return {
						value = item,
						display = location,
						ordinal = location .. " " .. context,
						filename = path,
						path = path,
						lnum = line,
						col = col + 1,
						text = context,
					}
				end,
			}),
			previewer = conf.grep_previewer({}),
			sorter = conf.generic_sorter({}),
			attach_mappings = function(prompt_bufnr)
				actions.select_default:replace(function()
					local selection = action_state.get_selected_entry()
					actions.close(prompt_bufnr)

					if selection and selection.value then
						open_reference(selection.value)
					end
				end)

				return true
			end,
		})
		:find()
end

-- Open project-wide find using a Telescope grep-style file preview.
-- 使用 Telescope grep 风格预览打开项目全局查找。
local function pick_telescope_find(items, default_text)
	local pickers = require("telescope.pickers")
	local finders = require("telescope.finders")
	local previewers = require("telescope.previewers")
	local actions = require("telescope.actions")
	local action_state = require("telescope.actions.state")
	local conf = require("telescope.config").values

	items = prepare_find_items(items)

	if vim.tbl_isempty(items) then
		vim.notify("UCore find: no results", vim.log.levels.WARN)
		return
	end

	pickers
		.new({}, {
			prompt_title = "UCore find",
			default_text = default_text,
			sorting_strategy = "ascending",
			-- Telescope's default reset strategy can try to restore row 1 while
			-- the filtered results buffer is temporarily empty, which trips
			-- `Invalid cursor line: out of range` on first prompt edits.
			selection_strategy = "row",
			finder = finders.new_table({
				results = items,
				entry_maker = make_find_entry,
			}),
			previewer = previewers.new_buffer_previewer({
				get_buffer_by_name = function(_, entry)
					if entry.filename and entry.filename ~= "" then
						return entry.filename
					end
				end,
				define_preview = function(self, entry)
					if not preview_find_file(self, entry) then
						preview_find_item(entry, self.state.bufnr)
					end
				end,
			}),
			sorter = conf.generic_sorter({}),
			attach_mappings = function(prompt_bufnr)
				actions.select_default:replace(function()
					local selection = action_state.get_selected_entry()
					actions.close(prompt_bufnr)

					if selection and selection.value then
						open_source_item(selection.value)
					end
				end)

				return true
			end,
		})
		:find()
end

-- Open project-wide find with backend-driven live search and pagination.
-- 使用后端实时搜索和分页打开项目全局查找。
local function pick_telescope_find_live(initial_symbols, opts)
	local pickers = require("telescope.pickers")
	local finders = require("telescope.finders")
	local previewers = require("telescope.previewers")
	local actions = require("telescope.actions")
	local action_state = require("telescope.actions.state")
	local sorters = require("telescope.sorters")

	opts = opts or {}

	local state = {
		query = tostring(opts.default_text or ""),
		symbols = initial_symbols or {},
		cached_initial_symbols = initial_symbols or {},
		static_items = opts.static_items or {},
		offset = #(initial_symbols or {}),
		limit = opts.page_size or FIND_PAGE_SIZE,
		has_more = #(initial_symbols or {}) >= (opts.page_size or FIND_PAGE_SIZE),
		loading = false,
		request_id = 0,
		input_seq = 0,
		pending_reset_query = nil,
	}

	local picker_ref

	local function combined_items()
		local results = filter_live_find_items(state.symbols or {}, state.query, state.limit)
		if #results >= state.limit then
			return results
		end

		local static = filter_static_find_items(state.static_items or {}, state.query, state.limit - #results)
		for _, item in ipairs(static) do
			table.insert(results, item)
		end
		return results
	end

	local function make_finder()
		return finders.new_table({
			results = combined_items(),
			entry_maker = make_find_entry,
		})
	end

	local function refresh_picker()
		if picker_ref then
			pcall(function()
				picker_ref:refresh(make_finder(), { reset_prompt = false })
			end)
		end
	end

	local function should_fetch_query(query)
		query = vim.trim(tostring(query or ""))
		return #query >= FIND_MIN_QUERY_LENGTH
	end

	local function backend_find_query(query)
		return tostring(query or "")
	end

	if not should_fetch_query(state.query) then
		state.has_more = false
	end

	local function request_symbols(query, reset)
		if type(opts.fetch_symbols) ~= "function" then
			return
		end
		query = tostring(query or "")
		if reset and not should_fetch_query(query) then
			state.pending_reset_query = nil
			state.request_id = state.request_id + 1
			state.loading = false
			state.symbols = vim.trim(query) == "" and state.cached_initial_symbols or {}
			state.offset = 0
			state.has_more = false
			refresh_picker()
			return
		end
		if not reset and not should_fetch_query(state.query) then
			return
		end
		if state.loading and not reset then
			return
		end
		if not reset and not state.has_more then
			return
		end

		state.loading = true
		state.request_id = state.request_id + 1
		local request_id = state.request_id
		local offset = reset and 0 or state.offset
		state.pending_reset_query = reset and nil or state.pending_reset_query
		if reset then
			state.symbols = {}
			state.offset = 0
			state.has_more = false
			refresh_picker()
		end

		opts.fetch_symbols(backend_find_query(query), {
			limit = state.limit,
			offset = offset,
		}, function(result, err, meta)
			vim.schedule(function()
				if request_id ~= state.request_id then
					return
				end

				meta = type(meta) == "table" and meta or {}
				local done = meta.done ~= false
				state.loading = false
				local pending_reset_query = state.pending_reset_query
				state.pending_reset_query = nil
				if pending_reset_query and pending_reset_query ~= query then
					request_symbols(pending_reset_query, true)
					return
				end

				if err then
					if done then
						vim.notify("UCore find failed:\n" .. tostring(err), vim.log.levels.ERROR)
					end
					return
				end

				local values = result or {}
				if reset and meta.append ~= true then
					local appended = state.symbols or {}
					state.symbols = values
					for _, item in ipairs(appended) do
						table.insert(state.symbols, item)
					end
					state.offset = #values
				else
					for _, item in ipairs(values) do
						table.insert(state.symbols, item)
					end
					state.offset = state.offset + #values
				end

				state.has_more = #values >= state.limit
				refresh_picker()
			end)
		end)
	end

	local function maybe_load_more(prompt_bufnr)
		if not picker_ref then
			picker_ref = action_state.get_current_picker(prompt_bufnr)
		end
		if not picker_ref or not picker_ref.manager then
			return
		end

		local total = picker_ref.manager:num_results()
		local row = picker_ref:get_selection_row() or 0
		if total > 0 and row >= total - 10 then
			request_symbols(state.query, false)
		end
	end

	if type(opts.subscribe_updates) == "function" then
		opts.subscribe_updates(function(snapshot)
			vim.schedule(function()
				if type(snapshot) ~= "table" then
					return
				end

				if type(snapshot.static_items) == "table" then
					state.static_items = snapshot.static_items
				end

				if state.query == "" and type(snapshot.initial_symbols) == "table" then
					state.cached_initial_symbols = snapshot.initial_symbols
					state.symbols = snapshot.initial_symbols
					state.offset = #snapshot.initial_symbols
					state.has_more = false
				end

				refresh_picker()
			end)
		end)
	end

	picker_ref = pickers.new({}, {
		prompt_title = "UCore find",
		default_text = state.query ~= "" and state.query or nil,
		sorting_strategy = "ascending",
		selection_strategy = "row",
		finder = make_finder(),
		previewer = previewers.new_buffer_previewer({
			get_buffer_by_name = function(_, entry)
				if entry.filename and entry.filename ~= "" then
					return entry.filename
				end
			end,
			define_preview = function(self, entry)
				if not preview_find_file(self, entry) then
					preview_find_item(entry, self.state.bufnr)
				end
			end,
		}),
		sorter = sorters.empty(),
		on_input_filter_cb = function(prompt)
			prompt = tostring(prompt or "")
			if prompt == state.query then
				return
			end

			state.query = prompt
			state.input_seq = state.input_seq + 1
			local input_seq = state.input_seq

			vim.defer_fn(function()
				if input_seq == state.input_seq then
					state.has_more = true
					request_symbols(state.query, true)
				end
			end, FIND_DEBOUNCE_MS)
		end,
		attach_mappings = function(prompt_bufnr, map)
			actions.select_default:replace(function()
				local selection = action_state.get_selected_entry()
				actions.close(prompt_bufnr)

				if selection and selection.value then
					open_source_item(selection.value)
				end
			end)

			local function move_next_and_load()
				actions.move_selection_next(prompt_bufnr)
				maybe_load_more(prompt_bufnr)
			end

			local function page_down_and_load()
				actions.results_scrolling_down(prompt_bufnr)
				maybe_load_more(prompt_bufnr)
			end

			map("i", "<C-n>", move_next_and_load)
			map("i", "<Down>", move_next_and_load)
			map("i", "<C-d>", page_down_and_load)
			map("i", "<PageDown>", page_down_and_load)
			map("n", "j", move_next_and_load)
			map("n", "<Down>", move_next_and_load)
			map("n", "<C-d>", page_down_and_load)
			map("n", "<PageDown>", page_down_and_load)

			return true
		end,
	})

	if vim.tbl_isempty(initial_symbols or {})
		and vim.tbl_isempty(opts.static_items or {})
		and not (opts.initial_loading and state.query == "")
	then
		request_symbols(state.query, true)
	end

	picker_ref:find()
end

-- Open a generic selection UI with a label formatter.
-- 打开一个通用选择 UI，并支持自定义显示文本。
local function pick(title, items, format_item, on_choice)
	if type(items) ~= "table" or vim.tbl_isempty(items) then
		vim.notify(title .. ": no results", vim.log.levels.WARN)
		return
	end

	local backend = picker_backend()

	if backend == "fzf-lua" then
		return pick_fzf(title, items, format_item, on_choice)
	end

	if backend == "telescope" then
		return pick_telescope(title, items, format_item, on_choice)
	end

	return pick_vim(title, items, format_item, on_choice)
end

-- Pick generic action items.
-- 选择通用动作项。
function M.items(title, items, opts)
	opts = opts or {}

	pick(title, items, opts.format_item or function(item)
		return tostring(item.label or item.name or item.title or item)
	end, opts.on_choice or function() end)
end

-- Pick a registered Unreal project.
-- 选择一个已注册 Unreal 项目。
function M.projects(items, on_choice)
	pick("UCore projects", items, function(item)
		local engine = item.engine_association and (" [" .. item.engine_association .. "]") or ""
		return string.format("%s%s - %s", item.name or item.root, engine, item.root)
	end, on_choice)
end

-- Pick a module and open its Build.cs or module root.
-- 选择一个模块，并打开它的 Build.cs 或模块目录。
function M.modules(modules)
	pick("UCore modules", modules, function(item)
		local name = tostring(item.name or "<unknown>")
		local typ = tostring(item.type or "")
		local owner = tostring(item.owner_name or item.component_name or "")

		if owner ~= "" then
			return string.format("%s [%s] - %s", name, typ, owner)
		end

		return string.format("%s [%s]", name, typ)
	end, function(item)
		local target = item.build_cs_path or item.path or item.module_root

		if not target or target == vim.NIL or target == "" then
			vim.notify("Selected module has no path", vim.log.levels.WARN)
			return
		end

		if vim.fn.filereadable(target) == 1 then
			vim.cmd.edit(vim.fn.fnameescape(target))
		else
			print(target)
			vim.fn.setreg("+", target)
			vim.notify("Copied module path to clipboard")
		end
	end)
end

-- Pick an asset path and copy it to the clipboard.
-- 选择一个资产路径，并复制到剪贴板。
function M.assets(assets)
	pick("UCore assets", assets, function(item)
		return tostring(item)
	end, function(item)
		local asset_path = tostring(item)
		vim.fn.setreg("+", asset_path)
		vim.notify("Copied asset path: " .. asset_path)
	end)
end

-- Pick a symbol and open its source file when possible.
-- 选择一个符号，并尽量打开它所在的源码文件。
function M.find(items, opts)
	opts = opts or {}

	if picker_backend() == "telescope" then
		return pick_telescope_find(items, opts.default_text)
	end

	pick("UCore find", items, function(item)
		local name = tostring(item.name or "<unknown>")
		local kind = tostring(item.symbol_type or item.type or "")
		local source = item.source and (" [" .. tostring(item.source) .. "]") or ""
		local path = tostring(item.path or "")

		if path ~= "" then
			return string.format("%s%s [%s] - %s", name, source, kind, path)
		end

		return string.format("%s%s [%s]", name, source, kind)
	end, open_source_item)
end

-- Pick symbols using live backend search when Telescope is available.
-- Telescope 可用时使用后端实时搜索选择 symbol。
function M.find_live(initial_symbols, opts)
	opts = opts or {}

	if picker_backend() == "telescope" then
		return pick_telescope_find_live(initial_symbols or {}, opts)
	end

	local items = {}
	for _, item in ipairs(initial_symbols or {}) do
		table.insert(items, item)
	end
	for _, item in ipairs(filter_static_find_items(opts.static_items or {}, opts.default_text or "", opts.page_size or FIND_PAGE_SIZE)) do
		table.insert(items, item)
	end

	return M.find(items, opts)
end

function M.prepare_find_items(items)
	return prepare_find_items(items)
end

-- Backward-compatible alias for older callers.
-- 兼容旧调用方。
function M.symbols(symbols)
	M.find(symbols)
end

-- Pick a reference result and open its source location.
-- 选择一个引用结果，并打开对应源码位置。
function M.references(references)
	if type(references) ~= "table" or vim.tbl_isempty(references) then
		vim.notify("UCore references: no results", vim.log.levels.WARN)
		return
	end

	if picker_backend() == "telescope" then
		return pick_telescope_references(references)
	end

	pick("UCore references", references, function(item)
		local path = tostring(item.path or item.file_path or "")
		local line = tonumber(item.line or item.line_number or 1) or 1
		local col = tonumber(item.col or item.column or 0) or 0
		local context = tostring(item.context or item.text or ""):gsub("^%s+", "")
		local label = reference_label(item.kind)

		local location = string.format("[%s] %s:%d:%d", label, display_path(path), line, col + 1)
		location = pad_right(truncate_left(location, 72), 72)

		if context ~= "" then
			return string.format("%s │ %s", location, context)
		end

		return location
	end, open_reference)
end

return M
