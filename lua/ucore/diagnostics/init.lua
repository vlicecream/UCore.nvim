local config = require("ucore.config")
local project = require("ucore.project")
local remote = require("ucore.remote")
local write_access = require("ucore.write_access")

local M = {}

local ns = vim.api.nvim_create_namespace("ucore_diagnostics")
local group_name = "UCoreDiagnostics"
local enabled = true
local refresh_sequence = 0
local try_include_symbol
local float_sequence = 0
local float_winid = nil
local last_float_key = nil

local severity_map = {
	error = vim.diagnostic.severity.ERROR,
	warning = vim.diagnostic.severity.WARN,
	information = vim.diagnostic.severity.INFO,
	info = vim.diagnostic.severity.INFO,
	hint = vim.diagnostic.severity.HINT,
}

local function current_content(bufnr)
	local lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
	return table.concat(lines, "\n") .. "\n"
end

local function normalize_path(path)
	return path and path:gsub("\\", "/") or nil
end

local function resolve_bufnr_for_path(file_path, fallback_bufnr)
	local normalized = normalize_path(file_path)
	if not normalized or normalized == "" then
		return fallback_bufnr
	end

	if fallback_bufnr and vim.api.nvim_buf_is_valid(fallback_bufnr) then
		local fallback_name = normalize_path(vim.api.nvim_buf_get_name(fallback_bufnr))
		if fallback_name == normalized then
			return fallback_bufnr
		end
	end

	for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
		if vim.api.nvim_buf_is_valid(bufnr) then
			local name = normalize_path(vim.api.nvim_buf_get_name(bufnr))
			if name == normalized then
				return bufnr
			end
		end
	end

	return vim.fn.bufadd(normalized)
end

local function diagnostic_from_item(item, fallback_bufnr)
	local file_path = normalize_path(item.file_path)
	local bufnr = fallback_bufnr

	if file_path and file_path ~= "" then
		bufnr = resolve_bufnr_for_path(file_path, fallback_bufnr)
	end

	return bufnr, {
		lnum = tonumber(item.line) or 0,
		col = tonumber(item.character) or 0,
		end_lnum = tonumber(item.end_line) or tonumber(item.line) or 0,
		end_col = tonumber(item.end_character) or ((tonumber(item.character) or 0) + 1),
		severity = severity_map[tostring(item.severity or "warning"):lower()] or vim.diagnostic.severity.WARN,
		source = item.source or "UCore",
		code = item.code,
		message = item.message or "",
		user_data = item,
	}
end

local function apply_items(items, fallback_bufnr)
	local by_buf = {}

	for _, item in ipairs(items or {}) do
		local bufnr, diagnostic = diagnostic_from_item(item, fallback_bufnr)
		by_buf[bufnr] = by_buf[bufnr] or {}
		table.insert(by_buf[bufnr], diagnostic)
	end

	if fallback_bufnr and not by_buf[fallback_bufnr] then
		vim.diagnostic.set(ns, fallback_bufnr, {})
	end

	for bufnr, diagnostics in pairs(by_buf) do
		vim.diagnostic.set(ns, bufnr, diagnostics)
	end
end

function M.refresh(bufnr, opts)
	opts = opts or {}
	bufnr = bufnr or vim.api.nvim_get_current_buf()

	if not enabled and not opts.force then
		return
	end

	local diagnostics_config = config.values.diagnostics or {}
	if diagnostics_config.enable == false and not opts.force then
		return
	end

	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	local file_path = vim.api.nvim_buf_get_name(bufnr)
	if file_path == "" then
		return
	end

	local root = project.find_project_root(file_path)
	if not root then
		return
	end

	refresh_sequence = refresh_sequence + 1
	local sequence = refresh_sequence
	local changedtick = vim.api.nvim_buf_get_changedtick(bufnr)

	remote.get_diagnostics(root, {
		content = current_content(bufnr),
		file_path = normalize_path(file_path),
	}, function(result, err)
		if sequence ~= refresh_sequence
			or not vim.api.nvim_buf_is_valid(bufnr)
			or vim.api.nvim_buf_get_changedtick(bufnr) ~= changedtick
		then
			return
		end

		if err then
			if not opts.silent then
				vim.notify("UCore diagnostics failed:\n" .. tostring(err), vim.log.levels.ERROR)
			end
			return
		end

		local items = type(result) == "table" and (result.items or result) or {}
		vim.schedule(function()
			apply_items(items, bufnr)
		end)
	end)
end

function M.clear(bufnr)
	if bufnr then
		vim.diagnostic.reset(ns, bufnr)
	else
		vim.diagnostic.reset(ns)
	end
end

local function close_cursor_float()
	if float_winid and vim.api.nvim_win_is_valid(float_winid) then
		pcall(vim.api.nvim_win_close, float_winid, true)
	end

	float_winid = nil
	last_float_key = nil
end

local function in_insert_mode()
	local mode = vim.api.nvim_get_mode().mode
	return mode == "i" or mode == "ic" or mode == "ix" or mode:sub(1, 2) == "ni"
end

local function show_cursor_float(bufnr)
	local diagnostics_config = config.values.diagnostics or {}
	if diagnostics_config.enable == false or diagnostics_config.float_on_cursor == false then
		return
	end

	local assist_ok, assist = pcall(require, "ucore.assist")
	if assist_ok and assist and type(assist.has_active_float) == "function" and assist.has_active_float() then
		local active_kind = type(assist.active_float_kind) == "function" and assist.active_float_kind() or nil
		if active_kind == "hover" and type(assist.close_float) == "function" then
			assist.close_float()
		else
			close_cursor_float()
			return
		end
	end

	if not vim.api.nvim_buf_is_valid(bufnr) or vim.api.nvim_get_current_buf() ~= bufnr then
		return
	end

	if in_insert_mode() and diagnostics_config.float_in_insert ~= true then
		return
	end

	local cursor = vim.api.nvim_win_get_cursor(0)
	local row = cursor[1] - 1
	local diagnostics = vim.diagnostic.get(bufnr, { lnum = row })
	if vim.tbl_isempty(diagnostics) then
		close_cursor_float()
		return
	end

	local key = table.concat({ bufnr, row, cursor[2] }, ":")
	if key == last_float_key and float_winid and vim.api.nvim_win_is_valid(float_winid) then
		return
	end

	close_cursor_float()

	local _, winid = vim.diagnostic.open_float(bufnr, {
		scope = "cursor",
		focusable = false,
		border = "rounded",
		source = true,
		close_events = {
			"CursorMoved",
			"CursorMovedI",
			"BufLeave",
			"WinLeave",
			"InsertEnter",
		},
	})

	float_winid = winid
	last_float_key = key
end

local function schedule_cursor_float(bufnr)
	local diagnostics_config = config.values.diagnostics or {}
	if diagnostics_config.enable == false or diagnostics_config.float_on_cursor == false then
		return
	end

	if in_insert_mode() and diagnostics_config.float_in_insert ~= true then
		return
	end

	float_sequence = float_sequence + 1
	local sequence = float_sequence
	local delay = tonumber(diagnostics_config.float_delay_ms) or 200

	vim.defer_fn(function()
		if sequence ~= float_sequence then
			return
		end

		vim.schedule(function()
			if sequence ~= float_sequence then
				return
			end
			show_cursor_float(bufnr)
		end)
	end, math.max(delay, 0))
end

local function current_ucore_diagnostic()
	local bufnr = vim.api.nvim_get_current_buf()
	local cursor = vim.api.nvim_win_get_cursor(0)
	local row = cursor[1] - 1
	local diagnostics = vim.diagnostic.get(bufnr, {
		namespace = ns,
		lnum = row,
	})

	table.sort(diagnostics, function(left, right)
		return (left.severity or 99) < (right.severity or 99)
	end)

	return diagnostics[1], bufnr
end

local function current_line_diagnostics(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	local cursor = vim.api.nvim_win_get_cursor(0)
	local row = cursor[1] - 1
	local diagnostics = vim.diagnostic.get(bufnr, {
		lnum = row,
	})

	table.sort(diagnostics, function(left, right)
		return (left.severity or 99) < (right.severity or 99)
	end)

	return diagnostics, row
end

local function line_text(bufnr, row)
	return vim.api.nvim_buf_get_lines(bufnr, row, row + 1, false)[1] or ""
end

local function set_line(bufnr, row, text)
	vim.api.nvim_buf_set_lines(bufnr, row, row + 1, false, { text })
end

local function insert_generated_body(bufnr, diagnostic)
	local row = (diagnostic and diagnostic.lnum or vim.api.nvim_win_get_cursor(0)[1] - 1) + 1
	local line = line_text(bufnr, row)
	local indent = line:match("^(%s*)") or ""
	vim.api.nvim_buf_set_lines(bufnr, row + 1, row + 1, false, { indent .. "\tGENERATED_BODY()", "" })
end

local function add_category(bufnr, diagnostic)
	local row = diagnostic.lnum
	local line = line_text(bufnr, row)
	if line:find("Category", 1, true) then
		return
	end

	local close = line:find("%)")
	if close then
		local sep = line:find("%(%s*%)") and "" or ", "
		set_line(bufnr, row, line:sub(1, close - 1) .. sep .. 'Category="Default"' .. line:sub(close))
	end
end

local function add_allow_private_access(bufnr, diagnostic)
	local row = diagnostic.lnum
	local line = line_text(bufnr, row)
	if line:find("AllowPrivateAccess", 1, true) then
		return
	end

	if line:find("meta%s*=%s*%(", 1) then
		set_line(bufnr, row, line:gsub("meta%s*=%s*%(", "meta=(AllowPrivateAccess=\"true\", ", 1))
		return
	end

	local close = line:find("%)")
	if close then
		local sep = line:find("%(%s*%)") and "" or ", "
		set_line(bufnr, row, line:sub(1, close - 1) .. sep .. 'meta=(AllowPrivateAccess="true")' .. line:sub(close))
	end
end

local function apply_ucore_fix(bufnr, diagnostic)
	local code = diagnostic.code or (diagnostic.user_data and diagnostic.user_data.code)
	if code == "UHT002" then
		insert_generated_body(bufnr, diagnostic)
	elseif code == "UEBP001" then
		add_category(bufnr, diagnostic)
	elseif code == "UEBP002" then
		add_allow_private_access(bufnr, diagnostic)
	else
		return false, code
	end

	M.refresh(bufnr, { force = true, silent = true })
	return true
end

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function include_path_from_file(file_path)
	file_path = normalize(file_path)
	if not file_path or file_path == "" then
		return nil
	end

	for _, marker in ipairs({ "/Public/", "/Classes/", "/Private/" }) do
		local start_at = file_path:find(marker, 1, true)
		if start_at then
			return file_path:sub(start_at + #marker)
		end
	end

	return file_path:match("([^/]+)$")
end

local function is_header_file(path)
	path = normalize(path or "")
	local ext = path:match("%.([^.]*)$")
	if not ext then
		return false
	end

	ext = ext:lower()
	return ext == "h" or ext == "hpp" or ext == "hh" or ext == "hxx"
end

local function header_to_source_candidates(path)
	path = normalize(path or "")
	if path == "" then
		return {}
	end

	local ext = path:match("%.([^.]*)$")
	if not ext then
		return {}
	end

	local base = path:sub(1, -(#ext + 2))
	local candidates = {
		base .. ".cpp",
		base .. ".cc",
		base .. ".cxx",
	}

	local mapped = path:gsub("/Classes/", "/Private/"):gsub("/Public/", "/Private/")
	if mapped ~= path then
		local mapped_base = mapped:sub(1, -(#ext + 2))
		table.insert(candidates, 1, mapped_base .. ".cpp")
		table.insert(candidates, 2, mapped_base .. ".cc")
		table.insert(candidates, 3, mapped_base .. ".cxx")
	end

	local seen = {}
	local result = {}
	for _, candidate in ipairs(candidates) do
		if candidate ~= "" and not seen[candidate] then
			seen[candidate] = true
			table.insert(result, candidate)
		end
	end

	return result
end

local function resolve_source_path(header_path)
	local candidates = header_to_source_candidates(header_path)
	for _, candidate in ipairs(candidates) do
		if vim.fn.filereadable(candidate) == 1 then
			return candidate, false
		end
	end

	return candidates[1], true
end

local function file_lines(path)
	if vim.fn.filereadable(path) ~= 1 then
		return {}
	end

	local ok, lines = pcall(vim.fn.readfile, path)
	return ok and lines or {}
end

local function find_buffer_for_path(path)
	path = normalize(path)
	if not path or path == "" then
		return nil
	end

	for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
		if normalize(vim.api.nvim_buf_get_name(bufnr)) == path then
			return bufnr
		end
	end

	return nil
end

local function lines_for_path(path)
	local bufnr = find_buffer_for_path(path)
	if bufnr and vim.api.nvim_buf_is_valid(bufnr) then
		if not vim.api.nvim_buf_is_loaded(bufnr) then
			pcall(vim.fn.bufload, bufnr)
		end
		if vim.api.nvim_buf_is_loaded(bufnr) then
			return vim.api.nvim_buf_get_lines(bufnr, 0, -1, false), bufnr
		end
	end

	return file_lines(path), nil
end

local function ensure_target_writable(path)
	return write_access.ensure_writable(path, {
		action = "generating definition",
	})
end

local function ensure_parent_dir(path)
	local parent = vim.fn.fnamemodify(path, ":p:h")
	if parent and parent ~= "" then
		vim.fn.mkdir(parent, "p")
	end
end

local function normalize_space(text)
	return tostring(text or ""):gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")
end

local function has_definition_text(lines, signature)
	local file_text = normalize_space(table.concat(lines or {}, "\n"))
	local normalized_signature = normalize_space(signature)
	return normalized_signature ~= "" and file_text:find(normalized_signature, 1, true) ~= nil
end

local function persist_lines_to_path(path, lines)
	ensure_parent_dir(path)
	local ok_writable, writable_err = ensure_target_writable(path)
	if not ok_writable then
		return false, writable_err
	end

	local bufnr = find_buffer_for_path(path)
	local temp_buf = false
	if not bufnr then
		bufnr = vim.fn.bufadd(path)
		temp_buf = true
	end

	if not bufnr or bufnr <= 0 or not vim.api.nvim_buf_is_valid(bufnr) then
		return false, "failed to allocate target buffer: " .. tostring(path)
	end

	if not vim.api.nvim_buf_is_loaded(bufnr) then
		local ok_load, load_err = pcall(vim.fn.bufload, bufnr)
		if not ok_load then
			return false, tostring(load_err)
		end
	end

	vim.bo[bufnr].modifiable = true
	vim.bo[bufnr].readonly = false
	vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)

	local ok_write, write_err = pcall(vim.api.nvim_buf_call, bufnr, function()
		vim.cmd("silent keepalt write")
	end)

	if not ok_write then
		return false, tostring(write_err)
	end

	if temp_buf and vim.api.nvim_buf_is_valid(bufnr) and vim.fn.bufwinnr(bufnr) == -1 then
		pcall(vim.api.nvim_buf_delete, bufnr, { force = false })
	end

	return true, nil
end

local function append_definition(path, header_path, definition_lines, created)
	local lines = lines_for_path(path)
	local header_include = include_path_from_file(header_path)

	if created or vim.tbl_isempty(lines) then
		if header_include and header_include ~= "" then
			lines = {
				string.format('#include "%s"', header_include),
				"",
			}
		end
	end

	if not vim.tbl_isempty(lines) and lines[#lines] ~= "" then
		table.insert(lines, "")
	end

	local start_line = #lines + 1

	for _, line in ipairs(definition_lines) do
		table.insert(lines, line)
	end

	table.insert(lines, "")
	local ok_write, write_err = persist_lines_to_path(path, lines)
	if not ok_write then
		return nil, write_err
	end

	return start_line, nil
end

local function definition_suffix(cursor_info)
	local suffixes = {}
	local full_text = tostring(cursor_info.full_text or "")
	local params = tostring(cursor_info.parameters or "")
	local params_end
	if params ~= "" then
		_, params_end = full_text:find(params, 1, true)
	end
	local trailing = params_end and full_text:sub(params_end + #params) or ""

	if cursor_info.is_const == true then
		table.insert(suffixes, "const")
	end

	local noexcept_text = trailing:match("(noexcept%s*%b())") or trailing:match("%f[%w](noexcept)%f[%W]")
	if noexcept_text then
		table.insert(suffixes, noexcept_text)
	end

	if trailing:find("&&", 1, true) then
		table.insert(suffixes, "&&")
	elseif trailing:find("&", 1, true) then
		table.insert(suffixes, "&")
	end

	if vim.tbl_isempty(suffixes) then
		return ""
	end

	return " " .. table.concat(suffixes, " ")
end

local function definition_targets(cursor_info)
	local targets = {}
	for _, item in ipairs(cursor_info.generated_definitions or {}) do
		if type(item) == "table" and tostring(item.name or "") ~= "" then
			table.insert(targets, {
				name = tostring(item.name),
				return_type = vim.trim(tostring(item.return_type or "")),
				kind = tostring(item.kind or "definition"),
			})
		end
	end

	return targets
end

local function build_definition_specs(cursor_info)
	local kind = tostring(cursor_info.kind or "")
	local class_name = tostring(cursor_info.class_name or "")
	local name = tostring(cursor_info.name or "")
	local params = tostring(cursor_info.parameters or "()")
	local return_type = vim.trim(tostring(cursor_info.return_type or ""))

	if kind == "function_definition" then
		return nil, "Current declaration already has a function body"
	end

	if class_name == "" or name == "" or params == "" then
		return nil, "Current declaration is not a supported member function"
	end

	local trimmed = vim.trim(tostring(cursor_info.full_text or ""))
	if trimmed:find("=%s*0%s*;") or trimmed:find("=%s*delete%s*;") or trimmed:find("=%s*default%s*;") then
		return nil, "Current declaration should not generate an out-of-line definition"
	end

	local targets = definition_targets(cursor_info)
	if vim.tbl_isempty(targets) then
		targets = {
			{
				name = name,
				return_type = return_type,
				kind = "definition",
			},
		}
	end

	local suffix = definition_suffix(cursor_info)
	local specs = {}
	for _, target in ipairs(targets) do
		local target_return_type = target.return_type
		if target.kind == "validation" and target_return_type == "" then
			target_return_type = "bool"
		end

		local signature
		if target_return_type ~= "" then
			signature = string.format("%s %s::%s%s", target_return_type, class_name, target.name, params)
		else
			signature = string.format("%s::%s%s", class_name, target.name, params)
		end

		signature = signature .. suffix
		table.insert(specs, {
			name = target.name,
			kind = target.kind,
			signature = signature,
			lines = {
				signature,
				"{",
				target.kind == "validation" and "\treturn true;" or "\t",
				"}",
			},
		})
	end

	return specs, nil
end

local function flatten_definition_lines(definition_specs)
	local lines = {}
	for index, spec in ipairs(definition_specs) do
		if index > 1 then
			table.insert(lines, "")
		end
		vim.list_extend(lines, spec.lines)
	end
	return lines
end

local function try_generate_definition(bufnr)
	local header_path = normalize(vim.api.nvim_buf_get_name(bufnr))
	if not is_header_file(header_path) then
		return false, "not_header"
	end

	local root = project.find_project_root(header_path)
	if not root then
		return false, "no_project"
	end

	local target_path, should_create = resolve_source_path(header_path)
	if not target_path or target_path == "" then
		vim.notify("No matching source path could be resolved for this header", vim.log.levels.INFO)
		return true, "no_source_path"
	end

	local cursor = vim.api.nvim_win_get_cursor(0)
	remote.query(root, {
		kind = "ParseBuffer",
		content = current_content(bufnr),
		file_path = header_path,
		line = cursor[1] - 1,
		character = cursor[2],
	}, function(result, err)
		if err then
			return vim.notify("UCore parse buffer failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end

		local cursor_info = type(result) == "table" and result.cursor_info or {}
		local definition_specs, reason = build_definition_specs(cursor_info or {})
		if not definition_specs then
			if reason == "Current declaration is not a supported member function" then
				return try_include_symbol(bufnr)
			end

			return vim.notify(reason or "Current declaration cannot generate a definition", vim.log.levels.INFO)
		end

		local source_lines = lines_for_path(target_path)
		local missing_specs = {}
		for _, spec in ipairs(definition_specs) do
			if not has_definition_text(source_lines, spec.signature) then
				table.insert(missing_specs, spec)
			end
		end

		if vim.tbl_isempty(missing_specs) then
			local ok = pcall(vim.cmd, "edit " .. vim.fn.fnameescape(target_path))
			if ok then
				vim.fn.search(cursor_info.class_name .. "::" .. definition_specs[1].name, "W")
			end
			return vim.notify("Definition already exists in source file: " .. target_path, vim.log.levels.INFO)
		end

		local definition_lines = flatten_definition_lines(missing_specs)
		local start_line, append_err = append_definition(target_path, header_path, definition_lines, should_create)
		if not start_line then
			if append_err == "definition generation cancelled" then
				return
			end
			return vim.notify("Failed to write definition:\n" .. tostring(append_err), vim.log.levels.ERROR)
		end
		local ok = pcall(vim.cmd, "edit " .. vim.fn.fnameescape(target_path))
		if ok then
			pcall(vim.api.nvim_win_set_cursor, 0, { start_line + 2, 1 })
		else
			vim.notify("Definition written to " .. target_path, vim.log.levels.INFO)
		end
	end)

	return true, nil
end

local function line_contains_include(lines, include_path)
	local quoted = string.format('"%s"', include_path)
	local angled = string.format("<%s>", include_path)
	for _, line in ipairs(lines or {}) do
		if type(line) == "table" then
			line = line.path or ""
		end
		line = tostring(line or "")
		if line:find(quoted, 1, true) or line:find(angled, 1, true) then
			return true
		end
		if line == include_path then
			return true
		end
	end
	return false
end

local function current_symbol(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	local cursor = vim.api.nvim_win_get_cursor(0)
	local row = cursor[1] - 1
	local col = cursor[2]
	local line = line_text(bufnr, row)
	if line == "" then
		return nil
	end

	local left = col + 1
	local right = col + 1
	local len = #line

	while left > 1 and line:sub(left - 1, left - 1):match("[%w_]") do
		left = left - 1
	end

	while right <= len and line:sub(right, right):match("[%w_]") do
		right = right + 1
	end

	if right <= left then
		return nil
	end

	local symbol = line:sub(left, right - 1)
	if symbol == "" or not symbol:match("^[%w_]+$") then
		return nil
	end

	return {
		symbol = symbol,
		row = row,
		col = col,
		start_col = left - 1,
		end_col = right - 2,
		line = line,
		before = line:sub(1, left - 1),
		after = line:sub(right),
	}
end

local function looks_like_assignment_target(symbol_info)
	if not symbol_info then
		return false
	end

	return symbol_info.after:match("^%s*[%+%-%*%%%&%|%^/]?=") ~= nil
end

local function should_try_include_symbol(bufnr, symbol_info)
	if not symbol_info then
		return false, "missing_symbol"
	end

	if looks_like_assignment_target(symbol_info) then
		local diagnostics = vim.diagnostic.get(bufnr, {
			lnum = symbol_info.row,
		})
		if not vim.tbl_isempty(diagnostics) then
			return false, "assignment_target"
		end
	end

	return true, nil
end

local function rank_include_candidate(item, symbol, current_path)
	local name = tostring(item.name or "")
	local path = normalize(item.path)
	if not path or path == current_path then
		return nil
	end

	local include_path = include_path_from_file(path)
	if not include_path or include_path == "" then
		return nil
	end

	local score = 0
	if name == symbol then
		score = score + 100
	end
	if path:match("%.h$") or path:match("%.hpp$") or path:match("%.hh$") then
		score = score + 25
	end
	if path:find("/Public/", 1, true) or path:find("/Classes/", 1, true) then
		score = score + 20
	end
	if path:find("/Private/", 1, true) then
		score = score + 5
	end

	return {
		item = item,
		path = path,
		include_path = include_path,
		score = score,
	}
end

local function insert_include_line(bufnr, include_path, target_line)
	local lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
	if line_contains_include(lines, include_path) then
		return false, "already_included"
	end

	local row = math.max((tonumber(target_line) or 1) - 1, 0)
	local insert = string.format('#include "%s"', include_path)
	vim.api.nvim_buf_set_lines(bufnr, row, row, false, { insert })
	return true
end

local function choose_and_insert_include(bufnr, metadata, candidates)
	if vim.tbl_isempty(candidates) then
		return vim.notify("No indexed header found for the current symbol", vim.log.levels.INFO)
	end

	table.sort(candidates, function(left, right)
		if left.score ~= right.score then
			return left.score > right.score
		end
		return left.include_path < right.include_path
	end)

	local function apply(candidate)
		local ok, reason = insert_include_line(
			bufnr,
			candidate.include_path,
			metadata.suggested_insert_line or 1
		)
		if ok then
			M.refresh(bufnr, { force = true, silent = true })
			return
		end
		if reason == "already_included" then
			vim.notify("Include already exists: " .. candidate.include_path, vim.log.levels.INFO)
			return
		end
		vim.notify("Failed to insert include", vim.log.levels.ERROR)
	end

	if #candidates == 1 or candidates[1].score > candidates[2].score then
		return apply(candidates[1])
	end

	vim.ui.select(candidates, {
		prompt = "Select include",
		format_item = function(entry)
			return entry.include_path
		end,
	}, function(choice)
		if choice then
			apply(choice)
		end
	end)
end

try_include_symbol = function(bufnr)
	local root = project.find_project_root(vim.api.nvim_buf_get_name(bufnr))
	if not root then
		return
	end

	local file_path = normalize(vim.api.nvim_buf_get_name(bufnr))
	local symbol_info = current_symbol(bufnr)
	if not symbol_info then
		return vim.notify("No symbol under cursor", vim.log.levels.INFO)
	end
	local allow_include, skip_reason = should_try_include_symbol(bufnr, symbol_info)
	if not allow_include then
		if skip_reason == "assignment_target" then
			return vim.notify("No quick fix available for the current symbol", vim.log.levels.INFO)
		end
		return vim.notify("No symbol under cursor", vim.log.levels.INFO)
	end
	local symbol = symbol_info.symbol

	remote.query(root, {
		kind = "ParseBuffer",
		content = current_content(bufnr),
		file_path = file_path,
	}, function(buffer_result, buffer_err)
		if buffer_err then
			return vim.notify("UCore buffer parse failed:\n" .. tostring(buffer_err), vim.log.levels.ERROR)
		end

		local metadata = type(buffer_result) == "table" and buffer_result.metadata or {}
		local include_lines = type(metadata.includes) == "table" and metadata.includes or {}

		remote.search_symbols(root, symbol, function(search_result, search_err)
			if search_err then
				return vim.notify("UCore include search failed:\n" .. tostring(search_err), vim.log.levels.ERROR)
			end

			local items = type(search_result) == "table" and search_result or {}
			local candidates = {}
			for _, item in ipairs(items) do
				local candidate = rank_include_candidate(item, symbol, file_path)
				if candidate and not line_contains_include(include_lines, candidate.include_path) then
					table.insert(candidates, candidate)
				end
			end

			choose_and_insert_include(bufnr, metadata, candidates)
		end, 24)
	end)
end

function M.smart_action()
	local bufnr = vim.api.nvim_get_current_buf()
	local diagnostic = current_ucore_diagnostic()
	if diagnostic then
		local ok = apply_ucore_fix(bufnr, diagnostic)
		if ok then
			return
		end
	end

	local handled = try_generate_definition(bufnr)
	if handled then
		return
	end

	try_include_symbol(bufnr)
end

function M.from_build_output(output, project_root)
	project_root = project_root or project.find_project_root_from_context()
	if not project_root then
		return
	end

	remote.parse_build_diagnostics(project_root, output, function(result, err)
		if err then
			vim.notify("UCore build diagnostics failed:\n" .. tostring(err), vim.log.levels.ERROR)
			return
		end

		local items = type(result) == "table" and (result.items or result) or {}
		vim.schedule(function()
			apply_items(items, vim.api.nvim_get_current_buf())
		end)
	end)
end

function M.from_quickfix(items)
	local diagnostics = {}

	for _, item in ipairs(items or {}) do
		local filename = normalize_path(item.filename)
		if filename and filename ~= "" then
			table.insert(diagnostics, {
				file_path = filename,
				line = math.max((tonumber(item.lnum) or 1) - 1, 0),
				character = math.max((tonumber(item.col) or 1) - 1, 0),
				end_line = math.max((tonumber(item.lnum) or 1) - 1, 0),
				end_character = math.max((tonumber(item.col) or 1), 1),
				severity = item.type == "E" and "error" or "warning",
				source = "Build",
				code = "BUILD",
				message = item.text or "",
			})
		end
	end

	apply_items(diagnostics, vim.api.nvim_get_current_buf())
end

local function schedule_refresh(args)
	local diagnostics_config = config.values.diagnostics or {}
	local delay = diagnostics_config.debounce_ms or 300
	local bufnr = args.buf

	refresh_sequence = refresh_sequence + 1
	local sequence = refresh_sequence

	vim.defer_fn(function()
		if sequence == refresh_sequence then
			M.refresh(bufnr, { silent = true })
		end
	end, delay)
end

function M.setup()
	local diagnostics_config = config.values.diagnostics or {}
	if diagnostics_config.enable == false then
		enabled = false
		return
	end

	local display_config = {
		underline = diagnostics_config.underline ~= false,
		virtual_text = diagnostics_config.virtual_text == true,
		signs = diagnostics_config.signs ~= false,
		update_in_insert = diagnostics_config.update_in_insert == true,
		severity_sort = true,
		float = {
			border = "rounded",
			source = true,
		},
	}

	-- Apply the visual presentation globally so editor diagnostics and UCore
	-- build/index diagnostics render consistently.
	-- 全局应用显示配置，让编辑器诊断与 UCore 的构建/索引诊断表现一致。
	vim.diagnostic.config(display_config)
	vim.diagnostic.config(display_config, ns)

	local group = vim.api.nvim_create_augroup(group_name, { clear = true })
	local refresh_events = { "BufWritePost", "TextChanged" }
	if diagnostics_config.update_in_insert == true then
		table.insert(refresh_events, "TextChangedI")
	end

	vim.api.nvim_create_autocmd(refresh_events, {
		group = group,
		pattern = { "*.h", "*.hpp", "*.cpp", "*.cc", "*.cxx" },
		callback = schedule_refresh,
	})

	vim.api.nvim_create_autocmd("BufReadPost", {
		group = group,
		pattern = { "*.h", "*.hpp", "*.cpp", "*.cc", "*.cxx" },
		callback = function(args)
			M.refresh(args.buf, { silent = true })
		end,
	})

	vim.api.nvim_create_autocmd({ "CursorMoved", "BufEnter" }, {
		group = group,
		pattern = { "*.h", "*.hpp", "*.cpp", "*.cc", "*.cxx" },
		callback = function(args)
			close_cursor_float()
			schedule_cursor_float(args.buf)
		end,
	})

	vim.api.nvim_create_autocmd("CursorMovedI", {
		group = group,
		pattern = { "*.h", "*.hpp", "*.cpp", "*.cc", "*.cxx" },
		callback = function(args)
			close_cursor_float()
			if diagnostics_config.float_in_insert == true then
				schedule_cursor_float(args.buf)
			end
		end,
	})

	vim.api.nvim_create_autocmd({ "InsertEnter", "BufLeave", "WinLeave" }, {
		group = group,
		pattern = { "*.h", "*.hpp", "*.cpp", "*.cc", "*.cxx" },
		callback = function()
			float_sequence = float_sequence + 1
			close_cursor_float()
		end,
	})
end

function M.reset()
	enabled = true
	refresh_sequence = refresh_sequence + 1
	float_sequence = float_sequence + 1
	close_cursor_float()
	pcall(vim.api.nvim_del_augroup_by_name, group_name)
	vim.diagnostic.reset(ns)
end

function M.close_cursor_float()
	float_sequence = float_sequence + 1
	close_cursor_float()
end

function M.has_active_float()
	return float_winid ~= nil and vim.api.nvim_win_is_valid(float_winid)
end

function M.has_cursor_diagnostic(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	if not vim.api.nvim_buf_is_valid(bufnr) then
		return false
	end

	local cursor = vim.api.nvim_win_get_cursor(0)
	local row = cursor[1] - 1
	local diagnostics = vim.diagnostic.get(bufnr, {
		lnum = row,
	})

	return not vim.tbl_isempty(diagnostics)
end

function M.resume_cursor_float(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	local assist_ok, assist = pcall(require, "ucore.assist")
	if assist_ok and assist and type(assist.has_active_float) == "function" and assist.has_active_float() then
		return
	end

	schedule_cursor_float(bufnr)
end

return M
