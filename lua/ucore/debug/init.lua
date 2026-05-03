local config = require("ucore.config")
local dirty = require("ucore.editing.dirty")
local project = require("ucore.project")
local remote = require("ucore.remote")
local ui = require("ucore.ui.select")
local unreal = require("ucore.unreal")

local M = {}

local redirect_group = "ucore_debug_redirect"
local track_ns = vim.api.nvim_create_namespace("ucore_debug_track")

local state = {
	adapter_registered = false,
	loaded_roots = {},
	redirected = {},
}

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function is_windows()
	return package.config:sub(1, 1) == "\\"
end

local function path_join(...)
	local parts = {}
	for _, part in ipairs({ ... }) do
		part = tostring(part or "")
		if part ~= "" then
			table.insert(parts, part)
		end
	end
	return normalize(table.concat(parts, "/"))
end

local function path_exists(path)
	return path and (vim.fn.filereadable(path) == 1 or vim.fn.isdirectory(path) == 1)
end

local function file_readable(path)
	return path and vim.fn.filereadable(path) == 1
end

local function lower(text)
	return tostring(text or ""):lower()
end

local function has_module(name)
	return pcall(require, name)
end

local function dap_available()
	return has_module("dap")
end

local function notify_missing_dap()
	vim.notify("UCore debug requires nvim-dap", vim.log.levels.WARN)
end

local function auto_open_ui_enabled()
	local ui_config = ((config.values.debug or {}).ui or {})
	return ui_config.auto_open ~= false
end

local function auto_close_ui_enabled()
	local ui_config = ((config.values.debug or {}).ui or {})
	return ui_config.auto_close ~= false
end

local function current_content(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	local lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
	return table.concat(lines, "\n") .. "\n"
end

local function is_header_file(path)
	local ext = tostring(normalize(path) or ""):match("%.([^.]*)$")
	ext = ext and ext:lower() or ""
	return ext == "h" or ext == "hpp" or ext == "hh" or ext == "hxx" or ext == "inl"
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

local function find_buffer_for_path(path)
	path = normalize(path)
	if not path or path == "" then
		return nil
	end

	for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
		if vim.api.nvim_buf_is_valid(bufnr) and normalize(vim.api.nvim_buf_get_name(bufnr)) == path then
			return bufnr
		end
	end

	return nil
end

local function ensure_buffer(path)
	path = normalize(path)
	if not path or path == "" then
		return nil
	end

	local bufnr = find_buffer_for_path(path)
	if not bufnr then
		bufnr = vim.fn.bufadd(path)
	end

	if not bufnr or bufnr <= 0 or not vim.api.nvim_buf_is_valid(bufnr) then
		return nil
	end

	if not vim.api.nvim_buf_is_loaded(bufnr) then
		pcall(vim.fn.bufload, bufnr)
	end

	return bufnr
end

local function lines_for_path(path)
	local bufnr = ensure_buffer(path)
	if bufnr and vim.api.nvim_buf_is_loaded(bufnr) then
		return vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
	end

	if file_readable(path) then
		local ok, lines = pcall(vim.fn.readfile, path)
		if ok then
			return lines
		end
	end

	return {}
end

local function normalize_space(text)
	return tostring(text or ""):gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")
end

local function find_function_signature(lines, opts)
	local signature = normalize_space(opts.signature)
	local locator = opts.locator or opts.signature
	local max_span = opts.max_span or 6

	if signature == "" or vim.tbl_isempty(lines or {}) then
		return nil
	end

	for start_line = 1, #lines do
		local combined = ""
		local limit = math.min(#lines, start_line + max_span - 1)
		for finish_line = start_line, limit do
			local current = tostring(lines[finish_line] or "")
			combined = combined == "" and current or (combined .. "\n" .. current)
			if normalize_space(combined):find(signature, 1, true) then
				for target_line = start_line, finish_line do
					local line_text = tostring(lines[target_line] or "")
					local col = line_text:find(locator, 1, true)
					if col then
						return {
							line = target_line,
							col = col - 1,
						}
					end
				end

				return {
					line = start_line,
					col = 0,
				}
			end
		end
	end

	return nil
end

local function implementation_target_names(cursor_info)
	local names = {}
	local seen = {}

	for _, item in ipairs(cursor_info.generated_definitions or {}) do
		local name = tostring(type(item) == "table" and item.name or "")
		if name ~= "" and not seen[name] then
			seen[name] = true
			table.insert(names, name)
		end
	end

	local fallback_name = tostring(cursor_info.name or "")
	if fallback_name ~= "" and not seen[fallback_name] then
		table.insert(names, fallback_name)
	end

	return names
end

local function query_parse_buffer(root, bufnr, file_path, line, character, callback)
	remote.query(root, {
		kind = "ParseBuffer",
		content = current_content(bufnr),
		file_path = normalize(file_path),
		line = line,
		character = character,
	}, callback)
end

local function resolve_header_breakpoint_target(root, bufnr, file_path, line, character, callback)
	if not is_header_file(file_path) then
		return callback(nil)
	end

	query_parse_buffer(root, bufnr, file_path, line, character, function(result, err)
		if err then
			return callback(nil, err)
		end

		local cursor_info = type(result) == "table" and result.cursor_info or {}
		local class_name = tostring(cursor_info.class_name or "")
		local params = tostring(cursor_info.parameters or "")
		if class_name == "" or params == "" then
			return callback(nil)
		end

		local source_path
		for _, candidate in ipairs(header_to_source_candidates(file_path)) do
			if file_readable(candidate) then
				source_path = candidate
				break
			end
		end
		if not source_path then
			return callback(nil)
		end

		local source_lines = lines_for_path(source_path)
		for _, name in ipairs(implementation_target_names(cursor_info)) do
			local match = find_function_signature(source_lines, {
				signature = string.format("%s::%s%s", class_name, name, params),
				locator = string.format("%s::%s", class_name, name),
			})
			if match then
				return callback({
					class_name = class_name,
					display_name = tostring(cursor_info.name or name),
					actual_name = name,
					display_path = normalize(file_path),
					display_line = line + 1,
					actual_path = source_path,
					actual_line = match.line,
				})
			end
		end

		callback(nil)
	end)
end

local function breakpoint_store_path(root)
	if not root then
		return nil
	end

	local paths = project.build_paths(root)
	return path_join(paths.cache_dir, "breakpoints.json")
end

local function write_json(path, value)
	if not path then
		return false
	end

	local parent = vim.fn.fnamemodify(path, ":p:h")
	if parent and parent ~= "" then
		vim.fn.mkdir(parent, "p")
	end

	local ok = pcall(vim.fn.writefile, vim.split(vim.json.encode(value), "\n"), path)
	return ok
end

local function read_json(path)
	if not path or vim.fn.filereadable(path) ~= 1 then
		return nil
	end

	local ok, lines = pcall(vim.fn.readfile, path)
	if not ok then
		return nil
	end

	local ok_decode, value = pcall(vim.json.decode, table.concat(lines, "\n"))
	if not ok_decode or type(value) ~= "table" then
		return nil
	end

	return value
end

local function default_adapter_path_candidates()
	local candidates = {}
	local data_dir = normalize(vim.fn.stdpath("data"))
	local home = normalize(vim.loop.os_homedir())

	local function add(path)
		if path and path ~= "" then
			table.insert(candidates, normalize(path))
		end
	end

	add(path_join(data_dir, "mason/packages/cpptools/extension/debugAdapters/bin/OpenDebugAD7.exe"))
	add(path_join(data_dir, "mason/packages/cpptools/debugAdapters/bin/OpenDebugAD7.exe"))

	if home and home ~= "" then
		for _, base in ipairs({
			".vscode/extensions",
			".cursor/extensions",
			".vscode-insiders/extensions",
			".vscodium/extensions",
		}) do
			for _, match in ipairs(vim.fn.glob(path_join(home, base, "ms-vscode.cpptools-*", "debugAdapters/bin/OpenDebugAD7.exe"), false, true)) do
				add(match)
			end
		end
	end

	return candidates
end

local function adapter_command()
	local debug_config = config.values.debug or {}
	local adapter = debug_config.adapter or {}

	if adapter.command and file_readable(adapter.command) then
		return normalize(adapter.command)
	end

	for _, candidate in ipairs(default_adapter_path_candidates()) do
		if file_readable(candidate) then
			return candidate
		end
	end

	return nil
end

local function adapter_args()
	local adapter = (config.values.debug or {}).adapter or {}
	return type(adapter.args) == "table" and adapter.args or {}
end

local function ensure_dap_adapter()
	if state.adapter_registered then
		return true, nil
	end

	if not dap_available() then
		return false, "nvim-dap is not available"
	end

	if not is_windows() then
		return false, "UCore debug currently supports Windows Unreal workflows only"
	end

	local command = adapter_command()
	if not command then
		return false, "OpenDebugAD7.exe was not found"
	end

	local dap = require("dap")
	dap.adapters.cppvsdbg = {
		id = "cppvsdbg",
		type = "executable",
		command = command,
		args = adapter_args(),
	}

	state.adapter_registered = true
	return true, command
end

local function project_context(root)
	root = root or project.find_project_root_from_context()
	if not root then
		return nil, "Could not find .uproject"
	end

	local metadata = unreal.current_context()
	if not metadata then
		return nil, "Could not resolve Unreal project context"
	end

	local editor = unreal.editor_executable(metadata.engine_root)
	return {
		root = normalize(metadata.root),
		uproject = normalize(metadata.uproject),
		project_name = metadata.project_name,
		engine_root = normalize(metadata.engine_root),
		editor_exe = normalize(editor),
	}, nil
end

local function belongs_to_context(path, ctx)
	path = lower(normalize(path))
	if path == "" then
		return false
	end

	local roots = {
		lower(ctx.root),
		lower(ctx.engine_root),
	}

	for _, root in ipairs(roots) do
		if root and root ~= "" then
			if not root:match("/$") then
				root = root .. "/"
			end
			if path:sub(1, #root) == root then
				return true
			end
		end
	end

	return false
end

local function display_sign_name()
	if vim.fn.sign_getdefined("UCoreDebugBreakpoint") ~= nil and #vim.fn.sign_getdefined("UCoreDebugBreakpoint") > 0 then
		return "UCoreDebugBreakpoint"
	end

	vim.fn.sign_define("UCoreDebugBreakpoint", {
		text = "B",
		texthl = "DiagnosticSignError",
		linehl = "",
		numhl = "",
	})
	return "UCoreDebugBreakpoint"
end

local function place_display_sign(path, line)
	local bufnr = ensure_buffer(path)
	if not bufnr then
		return nil, nil
	end

	local sign_id = vim.fn.sign_place(0, redirect_group, display_sign_name(), bufnr, {
		lnum = line,
		priority = 19,
	})

	return bufnr, sign_id
end

local function sign_line(bufnr, sign_id)
	if not bufnr or not sign_id or not vim.api.nvim_buf_is_valid(bufnr) then
		return nil
	end

	local placed = vim.fn.sign_getplaced(bufnr, { group = redirect_group }) or {}
	local signs = placed[1] and placed[1].signs or {}
	for _, sign in ipairs(signs) do
		if sign.id == sign_id then
			return tonumber(sign.lnum)
		end
	end

	return nil
end

local function unplace_display_sign(entry)
	if entry.display_bufnr and entry.display_sign_id then
		pcall(vim.fn.sign_unplace, redirect_group, {
			buffer = entry.display_bufnr,
			id = entry.display_sign_id,
		})
	end
end

local function actual_line(entry)
	if not entry.actual_bufnr or not entry.actual_mark_id or not vim.api.nvim_buf_is_valid(entry.actual_bufnr) then
		return entry.actual_line
	end

	local pos = vim.api.nvim_buf_get_extmark_by_id(entry.actual_bufnr, track_ns, entry.actual_mark_id, {})
	if type(pos) == "table" and #pos >= 1 then
		return pos[1] + 1
	end

	return entry.actual_line
end

local function set_actual_mark(path, line)
	local bufnr = ensure_buffer(path)
	if not bufnr then
		return nil, nil
	end

	local mark_id = vim.api.nvim_buf_set_extmark(bufnr, track_ns, math.max(line - 1, 0), 0, {})
	return bufnr, mark_id
end

local function remove_actual_mark(entry)
	if entry.actual_bufnr and entry.actual_mark_id and vim.api.nvim_buf_is_valid(entry.actual_bufnr) then
		pcall(vim.api.nvim_buf_del_extmark, entry.actual_bufnr, track_ns, entry.actual_mark_id)
	end
end

local function redirect_key(path, line)
	return string.format("%s:%d", normalize(path), tonumber(line) or 0)
end

local function entry_at_display(path, line)
	local wanted_path = normalize(path)
	for key, entry in pairs(state.redirected) do
		if entry.display_path == wanted_path then
			local current_line = sign_line(entry.display_bufnr, entry.display_sign_id) or entry.display_line
			if current_line == line then
				return key, entry
			end
		end
	end
	return nil, nil
end

local function active_root()
	return project.find_project_root_from_context()
end

local function save_project_breakpoints(root)
	root = root or active_root()
	if not root or not dap_available() then
		return
	end

	local breakpoints = require("dap.breakpoints").get()
	local ctx, err = project_context(root)
	if not ctx then
		if err then
			return
		end
	end

	local actuals = {}
	for bufnr, buf_breakpoints in pairs(breakpoints) do
		if vim.api.nvim_buf_is_valid(bufnr) then
			local path = normalize(vim.api.nvim_buf_get_name(bufnr))
			if path and belongs_to_context(path, ctx) then
				actuals[path] = actuals[path] or {}
				for _, bp in ipairs(buf_breakpoints) do
					table.insert(actuals[path], bp)
				end
			end
		end
	end

	local consumed = {}
	local items = {}

	for _, entry in pairs(state.redirected) do
		if entry.project_root == root then
			local current_actual_line = actual_line(entry) or entry.actual_line
			local current_display_line = sign_line(entry.display_bufnr, entry.display_sign_id) or entry.display_line
			local path = normalize(entry.actual_path)
			consumed[path] = consumed[path] or {}
			consumed[path][current_actual_line] = true

			table.insert(items, {
				redirected = true,
				display_path = normalize(entry.display_path),
				display_line = current_display_line,
				actual_path = path,
				actual_line = current_actual_line,
				condition = entry.condition,
				hit_condition = entry.hit_condition,
				log_message = entry.log_message,
			})
		end
	end

	for path, buf_breakpoints in pairs(actuals) do
		for _, bp in ipairs(buf_breakpoints) do
			local line = tonumber(bp.line) or 0
			if not (consumed[path] and consumed[path][line]) then
				table.insert(items, {
					redirected = false,
					display_path = path,
					display_line = line,
					actual_path = path,
					actual_line = line,
					condition = bp.condition,
					hit_condition = bp.hitCondition,
					log_message = bp.logMessage,
				})
			end
		end
	end

	table.sort(items, function(a, b)
		if a.display_path == b.display_path then
			return (a.display_line or 0) < (b.display_line or 0)
		end
		return tostring(a.display_path) < tostring(b.display_path)
	end)

	write_json(breakpoint_store_path(root), {
		version = 1,
		items = items,
	})
end

local function set_breakpoint_record(root, item)
	local dap_breakpoints = require("dap.breakpoints")
	local actual_bufnr = ensure_buffer(item.actual_path)
	if not actual_bufnr then
		return
	end

	dap_breakpoints.set({
		condition = item.condition,
		hit_condition = item.hit_condition,
		log_message = item.log_message,
	}, actual_bufnr, item.actual_line)

	if item.redirected then
		local display_bufnr, display_sign_id = place_display_sign(item.display_path, item.display_line)
		local tracked_bufnr, tracked_mark_id = set_actual_mark(item.actual_path, item.actual_line)
		if display_bufnr and display_sign_id and tracked_bufnr and tracked_mark_id then
			state.redirected[redirect_key(item.display_path, item.display_line)] = {
				project_root = root,
				display_path = normalize(item.display_path),
				display_line = item.display_line,
				display_bufnr = display_bufnr,
				display_sign_id = display_sign_id,
				actual_path = normalize(item.actual_path),
				actual_line = item.actual_line,
				actual_bufnr = tracked_bufnr,
				actual_mark_id = tracked_mark_id,
				condition = item.condition,
				hit_condition = item.hit_condition,
				log_message = item.log_message,
			}
		end
	end
end

local function restore_project_breakpoints(root)
	root = root or active_root()
	if not root or state.loaded_roots[root] or not dap_available() then
		return
	end

	local payload = read_json(breakpoint_store_path(root))
	state.loaded_roots[root] = true

	if not payload or type(payload.items) ~= "table" then
		return
	end

	for _, item in ipairs(payload.items) do
		if type(item) == "table" and item.actual_path and item.actual_line then
			set_breakpoint_record(root, item)
		end
	end
end

local function remove_redirected_breakpoint(key, entry)
	if not dap_available() then
		return
	end

	local dap_breakpoints = require("dap.breakpoints")
	local line = actual_line(entry) or entry.actual_line
	if entry.actual_bufnr and line then
		dap_breakpoints.remove(entry.actual_bufnr, line)
	end

	remove_actual_mark(entry)
	unplace_display_sign(entry)
	state.redirected[key] = nil
	save_project_breakpoints(entry.project_root)
end

local function create_redirected_breakpoint(root, target)
	local dap_breakpoints = require("dap.breakpoints")
	local actual_bufnr = ensure_buffer(target.actual_path)
	if not actual_bufnr then
		return vim.notify("UCore debug: could not open target source file for breakpoint", vim.log.levels.ERROR)
	end

	local existing = entry_at_display(target.display_path, target.display_line)
	if existing then
		return
	end

	dap_breakpoints.set({
		condition = target.condition,
		hit_condition = target.hit_condition,
		log_message = target.log_message,
	}, actual_bufnr, target.actual_line)

	local display_bufnr, display_sign_id = place_display_sign(target.display_path, target.display_line)
	local tracked_bufnr, tracked_mark_id = set_actual_mark(target.actual_path, target.actual_line)
	if not display_bufnr or not display_sign_id or not tracked_bufnr or not tracked_mark_id then
		return vim.notify("UCore debug: failed to place redirected breakpoint marker", vim.log.levels.ERROR)
	end

	state.redirected[redirect_key(target.display_path, target.display_line)] = {
		project_root = root,
		display_path = normalize(target.display_path),
		display_line = target.display_line,
		display_bufnr = display_bufnr,
		display_sign_id = display_sign_id,
		actual_path = normalize(target.actual_path),
		actual_line = target.actual_line,
		actual_bufnr = tracked_bufnr,
		actual_mark_id = tracked_mark_id,
		condition = target.condition,
		hit_condition = target.hit_condition,
		log_message = target.log_message,
	}

	save_project_breakpoints(root)
	vim.notify(
		string.format(
			"UCore debug: breakpoint redirected to %s:%d",
			vim.fn.fnamemodify(target.actual_path, ":t"),
			target.actual_line
		),
		vim.log.levels.INFO
	)
end

local function fallback_toggle_current_breakpoint(root)
	require("dap").toggle_breakpoint()
	save_project_breakpoints(root)
end

local function ensure_launch_ready(root, callback)
	local debug_config = config.values.debug or {}
	if debug_config.autosave_before_launch == false then
		return callback(true)
	end

	dirty.confirm_save(root, { action = "debug launch" }, function(ok, err)
		callback(ok, err)
	end)
end

local function dap_status(root)
	root = root or active_root()
	local ok = false
	local command = adapter_command()
	if dap_available() and is_windows() then
		ok = command ~= nil
	end
	return {
		enabled = (config.values.debug or {}).enable ~= false,
		dap_available = dap_available(),
		windows = is_windows(),
		adapter_ready = ok,
		adapter_command = command,
		breakpoint_store = root and breakpoint_store_path(root) or nil,
	}
end

local function enumerate_processes(ctx, callback)
	if not is_windows() then
		return callback({}, "UCore debug currently supports Windows only")
	end

	local names = {
		"UnrealEditor.exe",
		"UE4Editor.exe",
		ctx.project_name .. ".exe",
		ctx.project_name .. "Server.exe",
		ctx.project_name .. "Client.exe",
	}

	local quoted = {}
	for _, name in ipairs(names) do
		table.insert(quoted, "'" .. name:gsub("'", "''") .. "'")
	end

	local script = table.concat({
		"$names = @(" .. table.concat(quoted, ",") .. ")",
		"$items = Get-CimInstance Win32_Process | Where-Object { $names -contains $_.Name } | Select-Object @{n='pid';e={$_.ProcessId}}, @{n='name';e={$_.Name}}, @{n='exe';e={$_.ExecutablePath}}, @{n='command_line';e={$_.CommandLine}}",
		"$items | ConvertTo-Json -Compress -Depth 3",
	}, "; ")

	vim.system({
		vim.fn.executable("pwsh") == 1 and "pwsh" or "powershell",
		"-NoProfile",
		"-ExecutionPolicy",
		"Bypass",
		"-Command",
		script,
	}, { text = true }, function(result)
		vim.schedule(function()
			if result.code ~= 0 then
				return callback({}, result.stderr ~= "" and result.stderr or result.stdout)
			end

			local text = vim.trim(result.stdout or "")
			if text == "" then
				return callback({}, nil)
			end

			local ok, decoded = pcall(vim.json.decode, text)
			if not ok then
				return callback({}, "failed to parse process list")
			end

			local items = vim.islist(decoded) and decoded or { decoded }
			for _, item in ipairs(items) do
				local name = tostring(item.name or "")
				local command_line = normalize(item.command_line or "")
				local exe = normalize(item.exe or "")
				local score = 0
				if command_line:find(lower(ctx.uproject), 1, true) then
					score = score + 200
				end
				if command_line:find(lower(ctx.root), 1, true) then
					score = score + 120
				end
				if lower(name) == "unrealeditor.exe" or lower(name) == "ue4editor.exe" then
					score = score + 80
					item.kind = "editor"
				elseif lower(name):find("server", 1, true) then
					item.kind = "server"
				elseif lower(name):find("client", 1, true) then
					item.kind = "client"
				else
					item.kind = "game"
				end
				item.score = score
				item.command_line = command_line
				item.exe = exe
			end

			table.sort(items, function(a, b)
				if (a.score or 0) == (b.score or 0) then
					return tostring(a.pid or 0) < tostring(b.pid or 0)
				end
				return (a.score or 0) > (b.score or 0)
			end)

			callback(items, nil)
		end)
	end)
end

local function attach_with_process(process)
	local ok, err = ensure_dap_adapter()
	if not ok then
		return vim.notify("UCore debug: " .. tostring(err), vim.log.levels.ERROR)
	end

	local dap = require("dap")
	dap.run({
		type = "cppvsdbg",
		request = "attach",
		name = "UCore Attach " .. tostring(process.name or process.pid),
		processId = tostring(process.pid),
	})
end

function M.attach()
	if not dap_available() then
		return notify_missing_dap()
	end

	local ctx, err = project_context()
	if not ctx then
		return vim.notify("UCore debug: " .. tostring(err), vim.log.levels.ERROR)
	end

	restore_project_breakpoints(ctx.root)

	enumerate_processes(ctx, function(items, process_err)
		if process_err then
			return vim.notify("UCore debug: " .. tostring(process_err), vim.log.levels.ERROR)
		end

		if vim.tbl_isempty(items) then
			return vim.notify("UCore debug: no Unreal process found for current project", vim.log.levels.WARN)
		end

		local best = items[1]
		attach_with_process(best)
	end)
end

function M.pick_process()
	if not dap_available() then
		return notify_missing_dap()
	end

	local ctx, err = project_context()
	if not ctx then
		return vim.notify("UCore debug: " .. tostring(err), vim.log.levels.ERROR)
	end

	restore_project_breakpoints(ctx.root)

	enumerate_processes(ctx, function(items, process_err)
		if process_err then
			return vim.notify("UCore debug: " .. tostring(process_err), vim.log.levels.ERROR)
		end

		if vim.tbl_isempty(items) then
			return vim.notify("UCore debug: no Unreal process found", vim.log.levels.WARN)
		end

		ui.items("UCore debug processes", items, {
			format_item = function(item)
				local suffix = item.command_line and item.command_line ~= "" and (" - " .. item.command_line) or ""
				return string.format("[%s] %s (%s)%s", tostring(item.kind or "proc"), tostring(item.name or "?"), tostring(item.pid or "?"), suffix)
			end,
			on_choice = function(choice)
				attach_with_process(choice)
			end,
		})
	end)
end

function M.launch_editor()
	if not dap_available() then
		return notify_missing_dap()
	end

	local ctx, err = project_context()
	if not ctx then
		return vim.notify("UCore debug: " .. tostring(err), vim.log.levels.ERROR)
	end

	if not ctx.editor_exe or vim.fn.filereadable(ctx.editor_exe) ~= 1 then
		return vim.notify("UCore debug: UnrealEditor.exe was not found", vim.log.levels.ERROR)
	end

	local ok, adapter_err = ensure_dap_adapter()
	if not ok then
		return vim.notify("UCore debug: " .. tostring(adapter_err), vim.log.levels.ERROR)
	end

	restore_project_breakpoints(ctx.root)

	ensure_launch_ready(ctx.root, function(ready)
		if not ready then
			return
		end

		require("dap").run({
			type = "cppvsdbg",
			request = "launch",
			name = "UCore Launch Unreal Editor",
			program = ctx.editor_exe,
			args = { ctx.uproject },
			cwd = ctx.root,
			stopAtEntry = false,
			console = "integratedTerminal",
		})
	end)
end

function M.continue()
	if not dap_available() then
		return notify_missing_dap()
	end

	local dap = require("dap")
	if dap.session() then
		return dap.continue()
	end

	local ctx, err = project_context()
	if not ctx then
		return vim.notify("UCore debug: " .. tostring(err), vim.log.levels.ERROR)
	end

	restore_project_breakpoints(ctx.root)

	enumerate_processes(ctx, function(items)
		if items and not vim.tbl_isempty(items) then
			return attach_with_process(items[1])
		end

		M.launch_editor()
	end)
end

function M.restart()
	if not dap_available() then
		return notify_missing_dap()
	end

	local dap = require("dap")
	if dap.session() then
		return dap.restart()
	end

	M.continue()
end

function M.stop()
	if not dap_available() then
		return notify_missing_dap()
	end

	local dap = require("dap")
	if dap.session() then
		return dap.terminate()
	end

	vim.notify("UCore debug: no active debug session", vim.log.levels.INFO)
end

function M.step_over()
	if not dap_available() then
		return notify_missing_dap()
	end
	require("dap").step_over()
end

function M.step_into()
	if not dap_available() then
		return notify_missing_dap()
	end
	require("dap").step_into()
end

function M.step_out()
	if not dap_available() then
		return notify_missing_dap()
	end
	require("dap").step_out()
end

function M.hover()
	if not dap_available() then
		return notify_missing_dap()
	end

	require("dap.ui.widgets").hover()
end

function M.toggle_ui()
	local debug_ui = require("ucore.debug.ui")
	if debug_ui.is_open and debug_ui.is_open() then
		return debug_ui.close()
	end

	if dap_available() then
		return debug_ui.refresh(require("dap").session())
	end

	debug_ui.open()
end

function M.toggle_breakpoint()
	return M.toggle_breakpoint_with_opts({})
end

function M.toggle_breakpoint_with_opts(opts)
	if not dap_available() then
		return notify_missing_dap()
	end

	local root = active_root()
	if not root then
		return vim.notify("UCore debug: could not find .uproject", vim.log.levels.ERROR)
	end

	restore_project_breakpoints(root)

	local file_path = normalize(vim.api.nvim_buf_get_name(0))
	local bufnr = vim.api.nvim_get_current_buf()
	local line = vim.api.nvim_win_get_cursor(0)[1]
	local key, redirected = entry_at_display(file_path, line)
	if key and redirected then
		return remove_redirected_breakpoint(key, redirected)
	end

	local debug_config = config.values.debug or {}
	if debug_config.redirect_header_breakpoints ~= false and is_header_file(file_path) then
		local cursor = vim.api.nvim_win_get_cursor(0)
		return resolve_header_breakpoint_target(root, bufnr, file_path, cursor[1] - 1, cursor[2], function(target, err)
			if err then
				vim.notify("UCore debug: failed to resolve header breakpoint\n" .. tostring(err), vim.log.levels.ERROR)
				return fallback_toggle_current_breakpoint(root)
			end

			if target then
				target.condition = opts.condition
				target.hit_condition = opts.hit_condition
				target.log_message = opts.log_message
				return create_redirected_breakpoint(root, target)
			end

			if opts and (opts.condition or opts.hit_condition or opts.log_message) then
				require("dap.breakpoints").toggle({
					condition = opts.condition,
					hit_condition = opts.hit_condition,
					log_message = opts.log_message,
					replace = true,
				}, bufnr, line)
				save_project_breakpoints(root)
				return
			end

			fallback_toggle_current_breakpoint(root)
		end)
	end

	if opts and (opts.condition or opts.hit_condition or opts.log_message) then
		require("dap.breakpoints").toggle({
			condition = opts.condition,
			hit_condition = opts.hit_condition,
			log_message = opts.log_message,
			replace = true,
		}, bufnr, line)
		save_project_breakpoints(root)
		return
	end

	fallback_toggle_current_breakpoint(root)
end

function M.conditional_breakpoint()
	if not dap_available() then
		return notify_missing_dap()
	end

	vim.ui.input({ prompt = "UCore breakpoint condition: " }, function(condition)
		if condition == nil or vim.trim(condition) == "" then
			return
		end

		M.toggle_breakpoint_with_opts({
			condition = condition,
		})
	end)
end

function M.logpoint()
	if not dap_available() then
		return notify_missing_dap()
	end

	vim.ui.input({ prompt = "UCore logpoint message: " }, function(message)
		if message == nil or vim.trim(message) == "" then
			return
		end

		M.toggle_breakpoint_with_opts({
			log_message = message,
		})
	end)
end

function M.clear_breakpoints()
	if not dap_available() then
		return notify_missing_dap()
	end

	require("dap").clear_breakpoints()
	for key, entry in pairs(state.redirected) do
		remove_actual_mark(entry)
		unplace_display_sign(entry)
		state.redirected[key] = nil
	end

	local root = active_root()
	if root then
		save_project_breakpoints(root)
	end

	vim.notify("UCore debug: cleared breakpoints", vim.log.levels.INFO)
end

function M.list_breakpoints()
	if not dap_available() then
		return notify_missing_dap()
	end

	local items = {}
	local breakpoints = require("dap.breakpoints").get()
	for bufnr, buf_breakpoints in pairs(breakpoints) do
		local path = normalize(vim.api.nvim_buf_get_name(bufnr))
		for _, bp in ipairs(buf_breakpoints) do
			table.insert(items, {
				label = string.format("%s:%d", vim.fn.fnamemodify(path, ":."), bp.line),
				path = path,
				line = bp.line,
			})
		end
	end

	if vim.tbl_isempty(items) then
		return vim.notify("UCore debug: no breakpoints", vim.log.levels.INFO)
	end

	table.sort(items, function(a, b)
		if a.path == b.path then
			return a.line < b.line
		end
		return a.path < b.path
	end)

	ui.items("UCore breakpoints", items, {
		format_item = function(item)
			return item.label
		end,
		on_choice = function(item)
			vim.cmd.edit(vim.fn.fnameescape(item.path))
			pcall(vim.api.nvim_win_set_cursor, 0, { item.line, 0 })
			vim.cmd("normal! zz")
		end,
	})
end

function M.dispatch(tail)
	local sub = (tail or ""):match("^%s*(%S+)")
	sub = sub and sub:lower() or ""

	local handlers = {
		attach = M.attach,
		breakpoint = M.toggle_breakpoint,
		editor = M.launch_editor,
		["continue"] = M.continue,
		condition = M.conditional_breakpoint,
		clear = M.clear_breakpoints,
		logpoint = M.logpoint,
		stop = M.stop,
		breakpoints = M.list_breakpoints,
		processes = M.pick_process,
		ui = M.toggle_ui,
	}

	local handler = handlers[sub]
	if handler then
		return handler()
	end

	print([[
UCore debug subcommands:
  :UCore debug attach        Attach to the current Unreal process
  :UCore debug breakpoint    Toggle a breakpoint at the cursor
  :UCore debug editor        Launch Unreal Editor under debugger
  :UCore debug continue      Continue the active session, or attach if none
  :UCore debug condition     Set a conditional breakpoint at the cursor
  :UCore debug logpoint      Set a logpoint at the cursor
  :UCore debug clear         Clear all current breakpoints
  :UCore debug stop          Stop the active debug session
  :UCore debug breakpoints   List current breakpoints
  :UCore debug processes     Pick a process to attach
  :UCore debug ui            Toggle the minimal UCore debug UI
]])
end

function M.status(root)
	return dap_status(root)
end

function M.setup()
	local debug_config = config.values.debug or {}
	if debug_config.enable == false then
		return
	end

	local group = vim.api.nvim_create_augroup("UCoreDebug", { clear = true })
	vim.api.nvim_create_autocmd({ "BufReadPost", "BufNewFile", "BufEnter" }, {
		group = group,
		callback = function(args)
			local path = vim.api.nvim_buf_get_name(args.buf)
			local root = path ~= "" and project.find_project_root(path) or nil
			if root then
				restore_project_breakpoints(root)
			end
		end,
	})

	vim.api.nvim_create_autocmd("VimLeavePre", {
		group = group,
		callback = function()
			for root, _ in pairs(state.loaded_roots) do
				save_project_breakpoints(root)
			end
		end,
	})

	if dap_available() then
		local ok, dap = pcall(require, "dap")
		if ok and dap and dap.listeners then
			dap.listeners.after.event_initialized.ucore_debug = function()
				local root = active_root()
				if root then
					restore_project_breakpoints(root)
				end
				local debug_ui = require("ucore.debug.ui")
				if auto_open_ui_enabled() or debug_ui.is_open() then
					debug_ui.refresh(dap.session())
				end
			end
			dap.listeners.after.event_stopped.ucore_debug = function()
				local debug_ui = require("ucore.debug.ui")
				if auto_open_ui_enabled() or debug_ui.is_open() then
					debug_ui.refresh(dap.session())
				end
			end
			dap.listeners.after.event_continued.ucore_debug = function()
				local debug_ui = require("ucore.debug.ui")
				if auto_open_ui_enabled() or debug_ui.is_open() then
					debug_ui.mark_running(dap.session())
				end
			end
			dap.listeners.before.event_terminated.ucore_debug = function()
				if auto_close_ui_enabled() then
					require("ucore.debug.ui").close()
				end
			end
			dap.listeners.before.event_exited.ucore_debug = function()
				if auto_close_ui_enabled() then
					require("ucore.debug.ui").close()
				end
			end
		end
	end
end

return M
