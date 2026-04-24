local project = require("ucore.project")

local M = {}

local build_job = nil
local build_buf = nil

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
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

local function readable(path)
	return path and vim.fn.filereadable(path) == 1
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
	vim.cmd("botright 15new")
	local buf = vim.api.nvim_get_current_buf()
	vim.bo[buf].buftype = "nofile"
	vim.bo[buf].bufhidden = "hide"
	vim.bo[buf].swapfile = false
	vim.bo[buf].filetype = "ucore-build"
	pcall(vim.api.nvim_buf_set_name, buf, "ucore://build/" .. tostring(buf))
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, {
		title,
		string.rep("=", vim.fn.strdisplaywidth(title)),
		"",
	})
	vim.bo[buf].modified = false
	return buf
end

local function scroll_to_bottom(buf)
	for _, win in ipairs(vim.fn.win_findbuf(buf)) do
		local line_count = vim.api.nvim_buf_line_count(buf)
		vim.api.nvim_win_set_cursor(win, { line_count, 0 })
	end
end

local function append_lines(buf, data)
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

		vim.api.nvim_buf_set_lines(buf, -1, -1, false, lines)
		vim.bo[buf].modified = false
		scroll_to_bottom(buf)
	end)
end

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

	local title = string.format("UCore build: %s %s %s", opts.target, opts.platform, opts.configuration)
	local buf = create_log_buffer(title)
	build_buf = buf
	append_lines(buf, "Project: " .. ctx.uproject)
	append_lines(buf, "Engine:  " .. ctx.engine_root)
	append_lines(buf, "Command: " .. table.concat(cmd, " "))
	append_lines(buf, "")

	build_job = vim.system(cmd, {
		cwd = ctx.root,
		text = true,
		stdout = function(_, data)
			append_lines(buf, data)
		end,
		stderr = function(_, data)
			append_lines(buf, data)
		end,
	}, function(result)
		build_job = nil
		build_buf = nil
		vim.schedule(function()
			append_lines(buf, "")
			append_lines(buf, string.format("UCore build finished with exit code %d", result.code))
			local level = result.code == 0 and vim.log.levels.INFO or vim.log.levels.ERROR
			vim.notify(string.format("UCore build finished: %d", result.code), level)
			callback(result.code == 0, result, ctx)
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

	vim.notify("UCore build cancelled", vim.log.levels.WARN)
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
