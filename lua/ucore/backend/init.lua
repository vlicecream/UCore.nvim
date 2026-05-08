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

local function bundled_source_dir()
	local backend = config.values.backend or {}
	return normalize(backend.source_dir or backend.bundled_source_dir or (plugin_root .. "/UScanner"))
end

local function bundled_manifest_path()
	local source_dir = bundled_source_dir()
	if not source_dir then
		return nil
	end

	local manifest = normalize(source_dir .. "/Cargo.toml")
	if manifest and vim.fn.filereadable(manifest) == 1 then
		return manifest
	end

	return nil
end

function M.can_update_managed_backend()
	local script = build_script_path()
	return script and vim.fn.filereadable(script) == 1 and bundled_manifest_path() ~= nil
end

function M.update_managed_backend(callback)
	callback = callback or function() end

	if not M.can_update_managed_backend() then
		return callback(false, "Bundled backend build is unavailable for the current UCore backend config")
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
					output = string.format("bundled backend build exited with code %d", result.code)
				end
				return callback(false, output)
			end

			config.refresh_backend_commands()
			callback(true, output ~= "" and output or "Bundled backend built")
		end)
	end)
end

return M
