local config = require("ucore.config")

local M = {}

-- Check whether a Lua module can be required.
-- 检查某个 Lua 模块是否可用。
local function has_module(name)
	local ok = pcall(require, name)
	return ok
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
function M.symbols(symbols)
	pick("UCore symbols", symbols, function(item)
		local name = tostring(item.name or "<unknown>")
		local kind = tostring(item.symbol_type or item.type or "")
		local source = item.source and (" [" .. tostring(item.source) .. "]") or ""
		local path = tostring(item.path or "")

		if path ~= "" then
			return string.format("%s%s [%s] - %s", name, source, kind, path)
		end

		return string.format("%s%s [%s]", name, source, kind)
	end, function(item)
		local path = item.path
		local line = tonumber(item.line or item.line_number or 1) or 1

		if path and path ~= vim.NIL and vim.fn.filereadable(path) == 1 then
			vim.cmd.edit(vim.fn.fnameescape(path))
			vim.api.nvim_win_set_cursor(0, { line, 0 })
		else
			print(vim.inspect(item))
		end
	end)
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
