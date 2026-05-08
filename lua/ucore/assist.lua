local project = require("ucore.project")
local remote = require("ucore.remote")
local select_ui = require("ucore.ui.select")
local write_access = require("ucore.write_access")

local M = {}

local float_state = {
	buf = nil,
	win = nil,
	group = nil,
	kind = nil,
	auto = false,
}

local auto_state = {
	group = nil,
	hover_seq = 0,
	signature_seq = 0,
}

local function normalize_path(path)
	return tostring(path or ""):gsub("\\", "/")
end

local function is_json_null(value)
	return value == nil or value == vim.NIL
end

local function list_value(value)
	if type(value) == "table" then
		return value
	end
	return {}
end

local function text_value(value)
	if is_json_null(value) then
		return ""
	end
	return tostring(value or "")
end

local function current_content(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	local lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
	return table.concat(lines, "\n") .. "\n"
end

local function current_context()
	local bufnr = vim.api.nvim_get_current_buf()
	local file_path = vim.api.nvim_buf_get_name(bufnr)
	if file_path == "" then
		return nil, "Current buffer has no file path"
	end

	local root = project.find_project_root(file_path)
	if not root then
		return nil, "Current buffer is not inside an Unreal project"
	end

	local cursor = vim.api.nvim_win_get_cursor(0)
	return {
		bufnr = bufnr,
		root = root,
		file_path = normalize_path(file_path),
		content = current_content(bufnr),
		line = cursor[1] - 1,
		character = cursor[2],
	}, nil
end

local function close_float()
	if float_state.group then
		pcall(vim.api.nvim_del_augroup_by_id, float_state.group)
		float_state.group = nil
	end

	if float_state.win and vim.api.nvim_win_is_valid(float_state.win) then
		pcall(vim.api.nvim_win_close, float_state.win, true)
	end

	float_state.win = nil
	float_state.buf = nil
	float_state.kind = nil
	float_state.auto = false
end

local function display_path(path)
	return select_ui.relative_unreal_path(normalize_path(path))
end

local function float_width(lines, min_width, max_width)
	local width = min_width or 24
	for _, line in ipairs(lines or {}) do
		width = math.max(width, vim.fn.strdisplaywidth(tostring(line or "")))
	end
	width = width + 2
	return math.max(12, math.min(width, max_width or math.max(vim.o.columns - 4, 12)))
end

local function split_params(params)
	params = tostring(params or "")
	if params == "" or params == "()" then
		return {}
	end

	params = params:gsub("^%s*%(", ""):gsub("%)%s*$", "")
	if params == "" then
		return {}
	end

	local items = {}
	local current = {}
	local paren = 0
	local angle = 0
	local bracket = 0
	local brace = 0

	for i = 1, #params do
		local ch = params:sub(i, i)
		if ch == "," and paren == 0 and angle == 0 and bracket == 0 and brace == 0 then
			table.insert(items, vim.trim(table.concat(current)))
			current = {}
		else
			if ch == "(" then
				paren = paren + 1
			elseif ch == ")" then
				paren = math.max(paren - 1, 0)
			elseif ch == "<" then
				angle = angle + 1
			elseif ch == ">" then
				angle = math.max(angle - 1, 0)
			elseif ch == "[" then
				bracket = bracket + 1
			elseif ch == "]" then
				bracket = math.max(bracket - 1, 0)
			elseif ch == "{" then
				brace = brace + 1
			elseif ch == "}" then
				brace = math.max(brace - 1, 0)
			end

			table.insert(current, ch)
		end
	end

	local tail = vim.trim(table.concat(current))
	if tail ~= "" then
		table.insert(items, tail)
	end

	return items
end

local function signature_label(entry, active_param, active)
	local owner = tostring(entry.owner_class or entry.class_name or "")
	local name = tostring(entry.name or "")
	local return_type = vim.trim(tostring(entry.return_type or ""))
	local params = split_params(entry.parameters or entry.detail or "()")
	local parts = {}

	for index, param in ipairs(params) do
		param = vim.trim(param)
		if active and index - 1 == active_param then
			param = "[ " .. param .. " ]"
		end
		table.insert(parts, param)
	end

	local prefix = return_type ~= "" and (return_type .. " ") or ""
	local scope = owner ~= "" and (owner .. "::") or ""
	return string.format("%s%s%s(%s)", prefix, scope, name, table.concat(parts, ", "))
end

local function push_labeled_line(lines, highlights, label, value, group)
	value = vim.trim(tostring(value or ""))
	if value == "" then
		return
	end

	local line = string.format("%s %s", label, value)
	local index = #lines
	table.insert(lines, line)
	table.insert(highlights, {
		line = index,
		start_col = 0,
		end_col = #label,
		group = "Keyword",
	})
	table.insert(highlights, {
		line = index,
		start_col = #label + 1,
		end_col = #line,
		group = group or "Normal",
	})
end

local function hover_content(item)
	local lines = {}
	local highlights = {}
	local hover_kind = text_value(not is_json_null(item.hover_kind) and item.hover_kind or item.kind)
	local name = text_value(item.name ~= vim.NIL and item.name or item.symbol_name)
	local kind = text_value(item.kind)
	local class_name = text_value(item.class_name ~= vim.NIL and item.class_name or item.owner_class)
	local params = text_value(not is_json_null(item.parameters) and item.parameters or item.detail)
	local line_number = tonumber(item.line_number or item.line or 0) or 0
	local file_path = text_value(item.file_path)

	if hover_kind == "local" then
		table.insert(lines, name)
		push_labeled_line(
			lines,
			highlights,
			"type:",
			text_value(item.type_name) ~= "" and text_value(item.type_name) or "unknown",
			"Type"
		)
		if class_name ~= "" then
			push_labeled_line(lines, highlights, "scope:", class_name, "Type")
		end
		local resolved_type = item.resolved_type
		if type(resolved_type) == "table" and resolved_type ~= vim.NIL then
			local resolved_name = tostring(resolved_type.name or resolved_type.symbol_name or "")
			if resolved_name ~= "" then
				push_labeled_line(lines, highlights, "resolved:", resolved_name, "Type")
			end
		end
	else
		if kind:lower():find("function", 1, true) then
			table.insert(lines, signature_label(item, -1, false))
		elseif class_name ~= "" and name ~= "" then
			table.insert(lines, class_name .. "::" .. name)
		else
			table.insert(lines, name)
		end

		if params ~= "" and params ~= "()" and not kind:lower():find("function", 1, true) then
			push_labeled_line(lines, highlights, "params:", params, "Normal")
		end
		if item.base_class and item.base_class ~= vim.NIL and tostring(item.base_class) ~= "" then
			push_labeled_line(lines, highlights, "base:", tostring(item.base_class), "Type")
		end
		if item.module_name and item.module_name ~= vim.NIL and tostring(item.module_name) ~= "" then
			push_labeled_line(lines, highlights, "module:", tostring(item.module_name), "Directory")
		end
	end

	if file_path ~= "" then
		local location = display_path(file_path)
		if line_number > 0 then
			location = location .. ":" .. line_number
		end
		push_labeled_line(lines, highlights, "path:", location, "Directory")
	end

	return lines, highlights
end

local function signature_lines(result)
	local lines = {}
	local signatures = type(result) == "table" and list_value(result.signatures) or {}
	local active_param = tonumber(result.active_parameter or 0) or 0

	for index, entry in ipairs(signatures or {}) do
		table.insert(lines, signature_label(entry, active_param, index == 1))
		local location = display_path(tostring(entry.file_path or ""))
		local line_number = tonumber(entry.line_number or 0) or 0
		if location ~= "" then
			if line_number > 0 then
				location = location .. ":" .. line_number
			end
			table.insert(lines, "  " .. location)
		end
		if index < #signatures then
			table.insert(lines, "")
		end
	end

	return lines
end

local function open_float(lines, opts)
	opts = opts or {}
	lines = vim.tbl_map(function(line)
		return tostring(line or "")
	end, lines or {})

	if vim.tbl_isempty(lines) then
		return
	end

	close_float()

	local buf = vim.api.nvim_create_buf(false, true)
	float_state.buf = buf

	vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
	vim.bo[buf].buftype = "nofile"
	vim.bo[buf].bufhidden = "wipe"
	vim.bo[buf].swapfile = false
	vim.bo[buf].modifiable = false
	vim.bo[buf].filetype = opts.filetype or "text"

	if opts.syntax_filetype then
		pcall(vim.treesitter.start, buf, opts.syntax_filetype)
	end

	for _, item in ipairs(opts.highlights or {}) do
		pcall(vim.api.nvim_buf_add_highlight, buf, -1, item.group, item.line, item.start_col, item.end_col)
	end

	local width = float_width(lines, opts.min_width, opts.max_width)
	local height = math.min(#lines, math.max(vim.o.lines - 4, 1))

	local cursor = vim.api.nvim_win_get_cursor(0)
	local row = cursor[1] > math.floor(vim.o.lines * 0.6) and -(height + 1) or 1

	local win = vim.api.nvim_open_win(buf, false, {
		relative = "cursor",
		row = row,
		col = 1,
		width = width,
		height = height,
		style = "minimal",
		border = "rounded",
		focusable = false,
		noautocmd = true,
	})

	float_state.win = win
	float_state.kind = opts.kind
	float_state.auto = opts.auto == true

	local group = vim.api.nvim_create_augroup("UCoreAssistFloat", { clear = true })
	float_state.group = group
	local close_events = opts.close_events or {
		"CursorMoved",
		"CursorMovedI",
		"BufLeave",
		"WinLeave",
		"InsertEnter",
	}
	vim.api.nvim_create_autocmd(close_events, {
		group = group,
		callback = close_float,
	})
end

local function current_symbol_name()
	local symbol = vim.fn.expand("<cword>")
	symbol = vim.trim(tostring(symbol or ""))
	if symbol == "" then
		return nil
	end
	return symbol
end

local function ensure_project_context(opts)
	opts = opts or {}
	local ctx, err = current_context()
	if not ctx then
		if not opts.silent then
			vim.notify(err, vim.log.levels.WARN)
		end
		return nil
	end
	return ctx
end

function M.hover()
	return M.hover_auto({ auto = false })
end

function M.hover_auto(opts)
	opts = opts or {}
	local ctx = ensure_project_context({
		silent = opts.auto == true,
	})
	if not ctx then
		return
	end

	local sequence = opts.sequence
	local bufnr = ctx.bufnr
	local changedtick = vim.api.nvim_buf_get_changedtick(bufnr)

	remote.get_hover(ctx.root, {
		content = ctx.content,
		line = ctx.line,
		character = ctx.character,
		file_path = ctx.file_path,
	}, function(result, err)
		if err then
			if opts.auto then
				return
			end
			return vim.schedule(function()
				vim.notify("UCore hover failed:\n" .. tostring(err), vim.log.levels.ERROR)
			end)
		end

		if opts.auto then
			if auto_state.hover_seq ~= sequence then
				return
			end
			if not vim.api.nvim_buf_is_valid(bufnr) or vim.api.nvim_buf_get_changedtick(bufnr) ~= changedtick then
				return
			end
		end

		if is_json_null(result) or (type(result) == "table" and vim.tbl_isempty(result)) then
			if opts.auto and float_state.auto and float_state.kind == "hover" then
				vim.schedule(close_float)
			end
			return
		end

		vim.schedule(function()
			local lines, highlights = hover_content(result)
			open_float(lines, {
				filetype = "text",
				min_width = 28,
				kind = "hover",
				auto = opts.auto == true,
				highlights = highlights,
				syntax_filetype = "cpp",
			})
		end)
	end)
end

function M.signature_help()
	return M.signature_help_auto({ auto = false })
end

function M.signature_help_auto(opts)
	opts = opts or {}
	local ctx = ensure_project_context({
		silent = opts.auto == true,
	})
	if not ctx then
		return
	end

	local sequence = opts.sequence
	local bufnr = ctx.bufnr
	local changedtick = vim.api.nvim_buf_get_changedtick(bufnr)

	remote.get_signature_help(ctx.root, {
		content = ctx.content,
		line = ctx.line,
		character = ctx.character,
		file_path = ctx.file_path,
	}, function(result, err)
		if err then
			if opts.auto then
				return
			end
			return vim.schedule(function()
				vim.notify("UCore signature help failed:\n" .. tostring(err), vim.log.levels.ERROR)
			end)
		end

		if opts.auto then
			if auto_state.signature_seq ~= sequence then
				return
			end
			if not vim.api.nvim_buf_is_valid(bufnr) or vim.api.nvim_buf_get_changedtick(bufnr) ~= changedtick then
				return
			end
		end

		if is_json_null(result) or vim.tbl_isempty(list_value(type(result) == "table" and result.signatures or nil)) then
			if opts.auto and float_state.auto and float_state.kind == "signature" then
				vim.schedule(close_float)
			end
			return
		end

		vim.schedule(function()
			open_float(signature_lines(result), {
				filetype = "text",
				min_width = 36,
				kind = "signature",
				auto = opts.auto == true,
				close_events = {
					"CursorMoved",
					"CursorMovedI",
					"BufLeave",
					"WinLeave",
					"InsertLeave",
				},
			})
		end)
	end)
end

local function ensure_buffer_for_path(path)
	local bufnr = vim.fn.bufadd(path)
	if vim.fn.bufloaded(bufnr) ~= 1 then
		pcall(vim.fn.bufload, bufnr)
	end
	return bufnr
end

local function collect_rename_items(project_root, items)
	local results = {}
	project_root = normalize_path(project_root)

	for _, item in ipairs(items or {}) do
		local source = text_value(item.source)
		local path = normalize_path(item.path or item.file_path)
		local line = tonumber(item.line)
		local col = tonumber(item.col)
		local in_project = project_root ~= "" and path:sub(1, #project_root):lower() == project_root:lower()
		if source ~= "engine" and in_project and path ~= "" and line and col then
			table.insert(results, item)
		end
	end

	return results
end

local function apply_rename_edits(project_root, old_name, new_name, items)
	local grouped = {}

	for _, item in ipairs(collect_rename_items(project_root, items)) do
		local path = normalize_path(item.path or item.file_path)
		grouped[path] = grouped[path] or {}
		table.insert(grouped[path], {
			line = tonumber(item.line),
			col = tonumber(item.col),
		})
	end

	local touched = {}
	local changed_files = 0
	local changed_items = 0
	local paths = {}

	for path, _ in pairs(grouped) do
		table.insert(paths, path)
	end

	table.sort(paths)

	local ok_writable, writable_err = write_access.ensure_writable_many(paths, {
		action = "renaming symbol",
	})
	if not ok_writable then
		vim.notify("UCore rename cancelled:\n" .. tostring(writable_err), vim.log.levels.WARN)
		return
	end

	for _, path in ipairs(paths) do
		local edits = grouped[path]
		table.sort(edits, function(left, right)
			if left.line ~= right.line then
				return left.line > right.line
			end
			return left.col > right.col
		end)

		local bufnr = ensure_buffer_for_path(path)
		if vim.api.nvim_buf_is_valid(bufnr) then
			changed_files = changed_files + 1
		end

		for _, edit in ipairs(edits) do
			local row = edit.line - 1
			local line_text = vim.api.nvim_buf_get_lines(bufnr, row, row + 1, false)[1] or ""
			local current = line_text:sub(edit.col + 1, edit.col + #old_name)
			if current == old_name then
				vim.api.nvim_buf_set_text(bufnr, row, edit.col, row, edit.col + #old_name, { new_name })
				touched[bufnr] = true
				changed_items = changed_items + 1
			end
		end
	end

	for bufnr, _ in pairs(touched) do
		pcall(require("ucore.semantic").refresh, bufnr)
		pcall(require("ucore.diagnostics").refresh, bufnr, { force = true, silent = true })
	end

	vim.notify(
		string.format("UCore rename applied: %d changes in %d files", changed_items, changed_files),
		vim.log.levels.INFO
	)
end

local function rename_file_count(items)
	local seen = {}

	for _, item in ipairs(items or {}) do
		local path = normalize_path(item.path or item.file_path)
		if path ~= "" then
			seen[path] = true
		end
	end

	local count = 0
	for _, _ in pairs(seen) do
		count = count + 1
	end

	return count
end

local function prompt_rename_name(old_name, callback)
	callback(vim.fn.input("Rename to: ", old_name))
end

local function valid_identifier(text)
	return text:match("^[_%a][_%w]*$") ~= nil
end

local function apply_rename_with_confirmation(ctx, old_name, new_name, rename_items)
	new_name = vim.trim(tostring(new_name or ""))
	if new_name == "" or new_name == old_name then
		return
	end

	if not valid_identifier(new_name) then
		return vim.notify("UCore rename only accepts C/C++ identifier names", vim.log.levels.WARN)
	end

	local occurrence_count = #rename_items
	local file_count = rename_file_count(rename_items)
	local choice = vim.fn.confirm(
		string.format(
			"UCore rename\n\n%s -> %s\n\nApply to %d occurrences in %d files?",
			old_name,
			new_name,
			occurrence_count,
			file_count
		),
		"&Apply rename\n&Cancel",
		1,
		"Question"
	)

	if choice == 1 then
		apply_rename_edits(ctx.root, old_name, new_name, rename_items)
	end
end

local function open_rename_preview(ctx, old_name, items, preset_new_name)
	local rename_items = collect_rename_items(ctx.root, items)
	if vim.tbl_isempty(rename_items) then
		vim.notify("UCore rename found no project files to update", vim.log.levels.WARN)
		return
	end

	local title = preset_new_name
		and string.format("%s -> %s", old_name, preset_new_name)
		or string.format("%s", old_name)

	select_ui.rename_preview(rename_items, {
		title = title,
		occurrence_count = #rename_items,
		file_count = rename_file_count(rename_items),
		on_choice = function()
			if preset_new_name and vim.trim(tostring(preset_new_name)) ~= "" then
				apply_rename_with_confirmation(ctx, old_name, preset_new_name, rename_items)
				return
			end

			prompt_rename_name(old_name, function(value)
				apply_rename_with_confirmation(ctx, old_name, value, rename_items)
			end)
		end,
	})
end

local function capture_rename_target()
	local ctx = ensure_project_context()
	if not ctx then
		return nil
	end

	local old_name = current_symbol_name()
	if not old_name then
		vim.notify("No symbol under cursor", vim.log.levels.WARN)
		return nil
	end

	return ctx, old_name
end

local function run_rename(ctx, old_name, new_name)
	if not ctx or not old_name then
		return
	end

	remote.find_references(ctx.root, {
		symbol_name = old_name,
		file_path = ctx.file_path,
		content = ctx.content,
		line = ctx.line,
		character = ctx.character,
	}, function(result, err)
		if err then
			return vim.schedule(function()
				vim.notify("UCore rename failed:\n" .. tostring(err), vim.log.levels.ERROR)
			end)
		end

		local items = type(result) == "table" and (not is_json_null(result.results) and result.results or result) or {}
		items = list_value(items)
		if vim.tbl_isempty(items) then
			return vim.schedule(function()
				vim.notify("UCore rename found no matching occurrences", vim.log.levels.WARN)
			end)
		end

		vim.schedule(function()
			open_rename_preview(ctx, old_name, items, new_name)
		end)
	end)
end

function M.rename(new_name)
	local ctx, old_name = capture_rename_target()
	if not ctx or not old_name then
		return
	end

	if new_name and vim.trim(tostring(new_name)) ~= "" then
		return run_rename(ctx, old_name, new_name)
	end

	run_rename(ctx, old_name, nil)
end

function M.close_float()
	close_float()
end

local function auto_hover_enabled()
	local mode = vim.api.nvim_get_mode().mode
	return mode == "n"
end

local function auto_signature_enabled()
	local mode = vim.api.nvim_get_mode().mode
	return mode == "i" or mode == "ic" or mode == "ix"
end

local function schedule_auto_hover()
	if not auto_hover_enabled() then
		return
	end

	auto_state.hover_seq = auto_state.hover_seq + 1
	local sequence = auto_state.hover_seq

	vim.defer_fn(function()
		if auto_state.hover_seq ~= sequence or not auto_hover_enabled() then
			return
		end
		M.hover_auto({
			auto = true,
			sequence = sequence,
		})
	end, 80)
end

local function schedule_auto_signature()
	if not auto_signature_enabled() then
		return
	end

	auto_state.signature_seq = auto_state.signature_seq + 1
	local sequence = auto_state.signature_seq

	vim.defer_fn(function()
		if auto_state.signature_seq ~= sequence or not auto_signature_enabled() then
			return
		end
		M.signature_help_auto({
			auto = true,
			sequence = sequence,
		})
	end, 60)
end

function M.setup()
	M.reset()

	local group = vim.api.nvim_create_augroup("UCoreAssistAuto", { clear = true })
	auto_state.group = group

	vim.api.nvim_create_autocmd("CursorHold", {
		group = group,
		callback = schedule_auto_hover,
	})

	vim.api.nvim_create_autocmd({ "TextChangedI", "CursorMovedI", "InsertEnter" }, {
		group = group,
		callback = schedule_auto_signature,
	})

	vim.api.nvim_create_autocmd({ "InsertLeave", "BufLeave", "WinLeave" }, {
		group = group,
		callback = function()
			auto_state.signature_seq = auto_state.signature_seq + 1
			if float_state.auto and float_state.kind == "signature" then
				close_float()
			end
		end,
	})
end

function M.reset()
	auto_state.hover_seq = auto_state.hover_seq + 1
	auto_state.signature_seq = auto_state.signature_seq + 1
	if auto_state.group then
		pcall(vim.api.nvim_del_augroup_by_id, auto_state.group)
		auto_state.group = nil
	end
	close_float()
end

return M
