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

function M.fix()
	local diagnostic, bufnr = current_ucore_diagnostic()
	if not diagnostic then
		return vim.notify("No UCore diagnostic on the current line", vim.log.levels.INFO)
	end

	local code = diagnostic.code or (diagnostic.user_data and diagnostic.user_data.code)
	if code == "UHT002" then
		insert_generated_body(bufnr, diagnostic)
	elseif code == "UEBP001" then
		add_category(bufnr, diagnostic)
	elseif code == "UEBP002" then
		add_allow_private_access(bufnr, diagnostic)
	else
		return vim.notify("No quick fix available for " .. tostring(code), vim.log.levels.INFO)
	end

	M.refresh(bufnr, { force = true, silent = true })
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

	vim.diagnostic.config({
		underline = diagnostics_config.underline ~= false,
		virtual_text = diagnostics_config.virtual_text == true,
		signs = diagnostics_config.signs ~= false,
		update_in_insert = diagnostics_config.update_in_insert == true,
		severity_sort = true,
		float = {
			border = "rounded",
			source = true,
		},
	}, ns)

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
