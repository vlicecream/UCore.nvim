local config = require("ucore.config")

local M = {}

local source = debug.getinfo(1, "S").source:sub(2)
local plugin_root = vim.fn.fnamemodify(source, ":p:h:h:h:h"):gsub("\\", "/")

local function normalize(path)
	if not path or path == "" then
		return nil
	end

	local absolute = vim.fn.fnamemodify(path, ":p")
	return absolute:gsub("\\", "/"):gsub("/+$", "")
end

local function build_script_path()
	return normalize(plugin_root .. "/scripts/build.ps1")
end

function M.can_update_managed_backend()
	local backend = config.values.backend or {}
	if backend.source_dir ~= nil or backend.bin_dir ~= nil then
		return false
	end

	local script = build_script_path()
	return script and vim.fn.filereadable(script) == 1
end

function M.update_managed_backend(callback)
	callback = callback or function() end

	if not M.can_update_managed_backend() then
		return callback(false, "Managed backend auto-update is unavailable for the current UCore backend config")
	end

	local shell = vim.fn.executable("pwsh") == 1 and "pwsh" or "powershell"
	local cmd = {
		shell,
		"-NoProfile",
		"-ExecutionPolicy",
		"Bypass",
		"-File",
		build_script_path(),
	}

	vim.system(cmd, {
		cwd = plugin_root,
		text = true,
	}, function(result)
		vim.schedule(function()
			local output = vim.trim(table.concat({
				tostring(result.stdout or ""),
				tostring(result.stderr or ""),
			}, "\n"))

			if result.code ~= 0 then
				if output == "" then
					output = string.format("backend build exited with code %d", result.code)
				end
				return callback(false, output)
			end

			config.refresh_backend_commands()
			callback(true, output ~= "" and output or "Managed backend updated")
		end)
	end)
end

return M
