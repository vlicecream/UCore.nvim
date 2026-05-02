local config = require("ucore.config")
local project = require("ucore.project")
local remote = require("ucore.remote")

local M = {}

local ns = vim.api.nvim_create_namespace("ucore_diagnostics")
local group_name = "UCoreDiagnostics"
local enabled = true
local refresh_sequence = 0

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

local function diagnostic_from_item(item, fallback_bufnr)
	local file_path = normalize_path(item.file_path)
	local bufnr = fallback_bufnr

	if file_path and file_path ~= "" then
		bufnr = vim.fn.bufadd(file_path)
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

function M.toggle()
	enabled = not enabled
	if not enabled then
		M.clear()
	end
	vim.notify("UCore diagnostics " .. (enabled and "enabled" or "disabled"), vim.log.levels.INFO)
end

function M.to_qflist()
	vim.diagnostic.setqflist({ namespace = ns, open = true })
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

function M.fix()
	local diagnostic, bufnr = current_ucore_diagnostic()
	if not diagnostic then
		return vim.notify("No UCore diagnostic on the current line", vim.log.levels.INFO)
	end

	local ok, code = apply_ucore_fix(bufnr, diagnostic)
	if not ok then
		return vim.notify("No quick fix available for " .. tostring(code), vim.log.levels.INFO)
	end
end

local function lsp_clients_supporting_code_action(bufnr)
	return vim.tbl_filter(function(client)
		return client.supports_method and client:supports_method("textDocument/codeAction")
	end, vim.lsp.get_clients({ bufnr = bufnr }))
end

local function make_action_title(action)
	local kind = type(action.kind) == "string" and action.kind or ""
	if kind ~= "" then
		return string.format("%s [%s]", action.title or "Code Action", kind)
	end
	return action.title or "Code Action"
end

local function apply_lsp_action(bufnr, action, client_id, callback)
	callback = callback or function() end

	local function finish(ok, err)
		vim.schedule(function()
			callback(ok, err)
		end)
	end

	if action.edit then
		vim.lsp.util.apply_workspace_edit(action.edit, "utf-8")
	end

	if action.command then
		local command = action.command
		if type(command) == "table" then
			local client = vim.lsp.get_client_by_id(client_id)
			if client then
				client:request("workspace/executeCommand", command, function(err)
					finish(err == nil, err)
				end, bufnr)
				return
			end
		elseif type(command) == "string" then
			local client = vim.lsp.get_client_by_id(client_id)
			local payload = {
				command = command,
				arguments = action.arguments,
			}
			if client then
				client:request("workspace/executeCommand", payload, function(err)
					finish(err == nil, err)
				end, bufnr)
				return
			end
			vim.lsp.buf.execute_command(payload)
		end
	end

	finish(true, nil)
end

local function try_lsp_code_action(bufnr, callback)
	callback = callback or function() end
	local diagnostics, row = current_line_diagnostics(bufnr)
	local clients = lsp_clients_supporting_code_action(bufnr)

	if vim.tbl_isempty(clients) then
		return callback(false, "no_clients")
	end

	local params = vim.lsp.util.make_range_params(0, "utf-8")
	params.context = {
		diagnostics = diagnostics,
		only = { "quickfix", "source.fixAll" },
		triggerKind = vim.lsp.protocol.CodeActionTriggerKind.Invoked,
	}
	params.range.start.line = row
	params.range["end"].line = row

	vim.lsp.buf_request_all(bufnr, "textDocument/codeAction", params, function(results)
		local actions = {}

		for client_id, payload in pairs(results or {}) do
			for _, action in ipairs(payload.result or {}) do
				table.insert(actions, {
					client_id = client_id,
					action = action,
					is_preferred = action.isPreferred == true,
				})
			end
		end

		if vim.tbl_isempty(actions) then
			return callback(false, "empty")
		end

		table.sort(actions, function(left, right)
			if left.is_preferred ~= right.is_preferred then
				return left.is_preferred
			end
			return make_action_title(left.action) < make_action_title(right.action)
		end)

		if #actions == 1 or actions[1].is_preferred then
			return apply_lsp_action(bufnr, actions[1].action, actions[1].client_id, function(ok)
				callback(ok, ok and nil or "apply_failed")
			end)
		end

		vim.schedule(function()
			vim.ui.select(actions, {
				prompt = "Code Actions",
				format_item = function(item)
					return make_action_title(item.action)
				end,
			}, function(choice)
				if not choice then
					return callback(false, "cancelled")
				end
				apply_lsp_action(bufnr, choice.action, choice.client_id, function(ok)
					callback(ok, ok and nil or "apply_failed")
				end)
			end)
		end)
	end)
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

local function try_include_symbol(bufnr)
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

	try_lsp_code_action(bufnr, function(applied, reason)
		if applied then
			return
		end
		if reason == "cancelled" then
			return
		end

		local diagnostic = current_ucore_diagnostic()
		if diagnostic then
			local ok = apply_ucore_fix(bufnr, diagnostic)
			if ok then
				return
			end
		end

		try_include_symbol(bufnr)
	end)
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

function M.dispatch(args)
	local sub = (args or ""):match("^(%S+)") or "refresh"

	if sub == "refresh" then
		return M.refresh(0, { force = true })
	elseif sub == "clear" then
		return M.clear()
	elseif sub == "qflist" or sub == "quickfix" then
		return M.to_qflist()
	elseif sub == "action" then
		return M.smart_action()
	elseif sub == "fix" then
		return M.fix()
	elseif sub == "toggle" then
		return M.toggle()
	end

	vim.notify("Unknown UCore diagnostics command: " .. sub, vim.log.levels.ERROR)
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

	-- Apply the visual presentation globally so LSP diagnostics (for example
	-- clangd) and UCore diagnostics render consistently.
	-- 全局应用显示配置，让 clangd 这类 LSP 诊断和 UCore 诊断表现一致。
	vim.diagnostic.config(display_config)
	vim.diagnostic.config(display_config, ns)

	local group = vim.api.nvim_create_augroup(group_name, { clear = true })
	vim.api.nvim_create_autocmd({ "BufWritePost", "TextChanged" }, {
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
end

return M
