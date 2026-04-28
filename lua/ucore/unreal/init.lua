local config = require("ucore.config")
local project = require("ucore.project")

local M = {}

local build_job = nil
local build_buf = nil
local build_cancelled = false

-- Accumulated diagnostics for the current build.
-- 当前构建累积的诊断信息。
local build_diagnostics = {}
local build_error_count = 0
local build_warning_count = 0

-- Extmark namespace for build log coloring.
-- 构建日志着色的 extmark namespace。
local build_ns = vim.api.nvim_create_namespace("ucore_build_log")
local highlights_setup = false

local function setup_highlights()
	if highlights_setup then
		return
	end
	highlights_setup = true
	vim.api.nvim_set_hl(0, "UCoreBuildError", { fg = "#F44747", bold = true })
	vim.api.nvim_set_hl(0, "UCoreBuildWarning", { fg = "#FFCC66" })
	vim.api.nvim_set_hl(0, "UCoreBuildSuccess", { fg = "#89D185", bold = true })
	vim.api.nvim_set_hl(0, "UCoreBuildInfo", { fg = "#6A9955" })
	vim.api.nvim_set_hl(0, "UCoreBuildCommand", { fg = "#4FC1FF" })
end

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function readable(path)
	return path and vim.fn.filereadable(path) == 1
end

local function current_context()
	local root = project.find_project_root()
	if not root then
		return nil, "Could not find .uproject"
	end

	local uproject = project.find_project_file_in_root(root)
	if not uproject then
		return nil, "Could not find .uproject under project root: " .. root
	end

	local engine, engine_err = project.engine_metadata(root)
	if not engine then
		return nil, engine_err
	end

	return {
		root = root,
		uproject = uproject,
		project_name = vim.fn.fnamemodify(uproject, ":t:r"),
		engine_root = engine.engine_root,
		engine_association = engine.engine_association,
	}
end

local function executable(path)
	return path and (vim.fn.executable(path) == 1 or readable(path))
end

local function build_bat(engine_root)
	return normalize(engine_root .. "/Engine/Build/BatchFiles/Build.bat")
end

local function editor_exe(engine_root)
	local candidates = {
		normalize(engine_root .. "/Engine/Binaries/Win64/UnrealEditor.exe"),
		normalize(engine_root .. "/Engine/Binaries/Win64/UE4Editor.exe"),
	}

	for _, path in ipairs(candidates) do
		if executable(path) then
			return path
		end
	end

	return nil
end

local function powershell()
	return vim.fn.executable("pwsh") == 1 and "pwsh" or "powershell"
end

local function ps_quote(text)
	return "'" .. tostring(text):gsub("'", "''") .. "'"
end

local function build_command(ctx, opts)
	local bat = build_bat(ctx.engine_root)
	if not readable(bat) then
		return nil, "Build.bat not found: " .. tostring(bat)
	end

	local target = opts.target or (ctx.project_name .. "Editor")
	local platform = opts.platform or "Win64"
	local configuration = opts.configuration or "Development"
	local script = table.concat({
		"&",
		ps_quote(bat),
		ps_quote(target),
		ps_quote(platform),
		ps_quote(configuration),
		ps_quote("-Project=" .. ctx.uproject),
		ps_quote("-WaitMutex"),
	}, " ")

	return { powershell(), "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script }, nil
end

local function parse_build_args(args, ctx)
	args = vim.trim(args or "")
	local tokens = {}
	for token in args:gmatch("%S+") do
		table.insert(tokens, token)
	end

	return {
		configuration = tokens[1] or "Development",
		platform = tokens[2] or "Win64",
		target = tokens[3] or (ctx.project_name .. "Editor"),
	}
end

local function create_log_buffer(title)
	local previous_win = vim.api.nvim_get_current_win()
	vim.cmd("botright 15new")
	local buf = vim.api.nvim_get_current_buf()
	vim.bo[buf].buftype = "nofile"
	vim.bo[buf].bufhidden = "hide"
	vim.bo[buf].swapfile = false
	vim.bo[buf].buflisted = false
	vim.bo[buf].filetype = "ucore-build"
	local name = title:gsub("^UCore build:%s*", "UCore build - ") .. " #" .. tostring(buf)
	pcall(vim.api.nvim_buf_set_name, buf, name)
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, {
		title,
		string.rep("=", vim.fn.strdisplaywidth(title)),
		"",
	})
	vim.bo[buf].modified = false

	if vim.api.nvim_win_is_valid(previous_win) then
		vim.api.nvim_set_current_win(previous_win)
	end

	return buf
end

local function scroll_to_bottom(buf)
	for _, win in ipairs(vim.fn.win_findbuf(buf)) do
		local line_count = vim.api.nvim_buf_line_count(buf)
		vim.api.nvim_win_set_cursor(win, { line_count, 0 })
	end
end

-- Append lines to a buffer, optionally calling on_line(buf, line_num, text)
-- for each appended line. Used to feed lines into coloring and parsing.
-- 将行追加到 buffer，可选为每行调用 on_line(buf, line_num, text) 回调。
-- 用于着色和诊断解析。
local function append_lines(buf, data, on_line)
	if not data or data == "" then
		return
	end

	vim.schedule(function()
		if not vim.api.nvim_buf_is_valid(buf) then
			return
		end

		data = data:gsub("\r\n", "\n"):gsub("\r", "\n")
		local lines = vim.split(data, "\n", { plain = true })
		if lines[#lines] == "" then
			table.remove(lines, #lines)
		end
		if vim.tbl_isempty(lines) then
			return
		end

		local start_line = vim.api.nvim_buf_line_count(buf)
		vim.api.nvim_buf_set_lines(buf, -1, -1, false, lines)
		vim.bo[buf].modified = false
		scroll_to_bottom(buf)

		if on_line then
			for i, line_text in ipairs(lines) do
				on_line(buf, start_line + i - 1, line_text)
			end
		end
	end)
end

-- ---------------------------------------------------------------------------
-- Build diagnostic parsing and coloring
-- ---------------------------------------------------------------------------

-- Parse a single build output line into a quickfix item, or return nil.
-- 解析一行构建输出为 quickfix item，无法解析时返回 nil。
local function parse_diagnostic_line(line, project_root)
	-- MSVC: path(line[,col]) : type CXXXX: message
	local path, lnum, col, kind, msg = line:match(
		"^(.-)%((%d+)(?:,(%d+))?%)%s*:%s*(error|warning)%s+(.+)$"
	)
	if path then
		lnum = tonumber(lnum)
		col = tonumber(col or 0)
		kind = (kind == "error") and "E" or "W"
		if not readable(path) and project_root then
			local abs = normalize(project_root .. "/" .. path)
			if readable(abs) then
				path = abs
			end
		end
		return { filename = path, lnum = lnum, col = col, type = kind, text = msg }
	end

	-- Clang: path:line:col: type: message
	local path2, lnum2, col2, kind2, msg2 = line:match(
		"^([A-Za-z]:[^:]+):(%d+):(%d+):%s*(error|warning):%s*(.+)$"
	)
	if path2 then
		return {
			filename = path2,
			lnum = tonumber(lnum2),
			col = tonumber(col2),
			type = (kind2 == "error") and "E" or "W",
			text = msg2,
		}
	end

	-- LINK fatal error
	if line:find("fatal error LNK", 1, true) then
		local msg3 = line:match("fatal error LNK%d+%s*:.*$") or line
		return { type = "E", text = msg3 }
	end

	-- UBT/UHT known error prefixes
	if line:find("Error:", 1, true) and (line:find("^LogCompile", 1, true) or line:find("^LogLinker", 1, true)) then
		return { type = "E", text = line }
	end

	return nil
end

-- Apply extmark highlight to a single build log line.
-- 给一行构建日志添加 extmark 高亮。
local function color_build_line(buf, line_num, text)
	if not config.values.build.color_log then
		return
	end

	local group
	if text:find("error C%d+", 1, true)
		or text:find("fatal error", 1, true)
		or text:find(" LNK%d+", 1, true)
		or text:find("UBT ERROR", 1, true)
		or text:find("Error:", 1, true)
	then
		group = "UCoreBuildError"
	elseif text:find("warning C%d+", 1, true) or text:find(": warning ", 1, true) or text:find("WARNING:", 1, true) then
		group = "UCoreBuildWarning"
	elseif text:find("Succeeded", 1, true) or text:find("finished with exit code 0", 1, true) then
		group = "UCoreBuildSuccess"
	elseif text:find("^Project:", 1, true) or text:find("^Engine:", 1, true) or text:find("^Command:", 1, true) then
		group = "UCoreBuildCommand"
	end

	if group then
		vim.api.nvim_buf_set_extmark(buf, build_ns, line_num, 0, {
			hl_group = group,
			end_row = line_num,
			end_col = -1,
		})
	end
end

-- Fill quickfix list from accumulated diagnostics and optionally open it.
-- 用累积的诊断填充 quickfix 列表，并可选打开 quickfix 窗口。
local function fill_quickfix()
	if vim.tbl_isempty(build_diagnostics) then
		return
	end

	vim.fn.setqflist(build_diagnostics, "r")

	if config.values.build.open_quickfix_on_error and build_error_count > 0 then
		vim.cmd("botright copen")
		vim.cmd("wincmd p")
	end
end

-- Build a human-readable summary string.
-- 构造人类可读的摘要字符串。
local function build_summary(ok, exit_code)
	local parts = {}
	if ok then
		table.insert(parts, "Build succeeded")
	else
		table.insert(parts, "Build failed")
	end

	if build_error_count > 0 then
		table.insert(parts, build_error_count .. " error" .. (build_error_count > 1 and "s" or ""))
	end

	if build_warning_count > 0 then
		table.insert(parts, build_warning_count .. " warning" .. (build_warning_count > 1 and "s" or ""))
	end

	if exit_code ~= nil then
		table.insert(parts, "exit " .. exit_code)
	end

	return table.concat(parts, ", ")
end

-- Handler for each incoming line of build output.
-- 每行构建输出的处理函数。
local function on_build_line(project_root, _buf, _line_num, text, no_parse)
	if not no_parse then
		local item = parse_diagnostic_line(text, project_root)
		if item then
			table.insert(build_diagnostics, item)
			if item.type == "E" then
				build_error_count = build_error_count + 1
			elseif item.type == "W" then
				build_warning_count = build_warning_count + 1
			end
		end
	end
	color_build_line(_buf, _line_num, text)
end

-- Reset diagnostics state for a new build.
-- 重置新构建的诊断状态。
local function reset_diagnostics()
	build_diagnostics = {}
	build_error_count = 0
	build_warning_count = 0
	build_cancelled = false
end

-- ---------------------------------------------------------------------------
-- Build lifecycle
-- ---------------------------------------------------------------------------

local function start_build(args, callback)
	callback = callback or function() end

	if build_job then
		vim.notify("UCore build is already running", vim.log.levels.WARN)
		return callback(false, "build already running")
	end

	local ctx, err = current_context()
	if not ctx then
		vim.notify(tostring(err), vim.log.levels.ERROR)
		return callback(false, err)
	end

	local opts = parse_build_args(args, ctx)
	local cmd, cmd_err = build_command(ctx, opts)
	if not cmd then
		vim.notify(tostring(cmd_err), vim.log.levels.ERROR)
		return callback(false, cmd_err)
	end

	reset_diagnostics()
	setup_highlights()

	local title = string.format("UCore build: %s %s %s", opts.target, opts.platform, opts.configuration)
	local buf = create_log_buffer(title)
	build_buf = buf

	local function header_on_line(b, ln, t)
		color_build_line(b, ln, t)
	end

	append_lines(buf, "Project: " .. ctx.uproject, header_on_line)
	append_lines(buf, "Engine:  " .. ctx.engine_root, header_on_line)
	append_lines(buf, "Command: " .. table.concat(cmd, " "), header_on_line)
	append_lines(buf, "")

	local project_root = ctx.root
	build_job = vim.system(cmd, {
		cwd = ctx.root,
		text = true,
		stdout = function(_, data)
			append_lines(buf, data, function(b, ln, t)
				on_build_line(project_root, b, ln, t)
			end)
		end,
		stderr = function(_, data)
			append_lines(buf, data, function(b, ln, t)
				on_build_line(project_root, b, ln, t, true)
			end)
		end,
	}, function(result)
		build_job = nil
		local this_buf = build_buf
		build_buf = nil
		local was_cancelled = build_cancelled

		vim.schedule(function()
			if not was_cancelled then
				local ok = result.code == 0
				local summary = build_summary(ok, result.code)
				local level = ok and vim.log.levels.INFO or vim.log.levels.ERROR

				if this_buf and vim.api.nvim_buf_is_valid(this_buf) then
					append_lines(this_buf, "")
					append_lines(this_buf, summary)
				end

				vim.notify(summary, level)
				fill_quickfix()
				callback(ok, result, ctx)
			else
				vim.notify("UCore build cancelled", vim.log.levels.WARN)
				callback(false, "cancelled")
			end
		end)
	end)
end

local function launch_editor(ctx)
	local exe = editor_exe(ctx.engine_root)
	if not exe then
		return vim.notify("UnrealEditor.exe not found under: " .. tostring(ctx.engine_root), vim.log.levels.ERROR)
	end

	vim.system({ powershell(), "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "Start-Process -FilePath " .. ps_quote(exe) .. " -ArgumentList " .. ps_quote(ctx.uproject) }, {
		cwd = ctx.root,
	}, function() end)

	vim.notify("Opening Unreal Editor: " .. ctx.project_name, vim.log.levels.INFO)
end

function M.build(args)
	start_build(args)
end

function M.cancel_build()
	if not build_job then
		return vim.notify("No UCore build is running", vim.log.levels.INFO)
	end

	build_cancelled = true
	local buf = build_buf
	pcall(function()
		build_job:kill(15)
	end)
	build_job = nil
	build_buf = nil

	if buf and vim.api.nvim_buf_is_valid(buf) then
		append_lines(buf, "")
		append_lines(buf, "UCore build cancelled")
	end
end

function M.open_editor(args)
	start_build(args, function(ok, result, ctx)
		if not ok then
			return vim.notify("UCore editor: build failed, not opening Unreal Editor", vim.log.levels.ERROR)
		end

		launch_editor(ctx)
	end)
end

return M
