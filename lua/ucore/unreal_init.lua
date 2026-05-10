local install = require("ucore.install")
local status = require("ucore.status")

local M = {}

local running = {}
local completed = {}

local function finish_step(key, message)
	status.unreal_step_finish(key, message)
end

local function finish_panel()
	finish_step("task:asset_bridge", "Asset Jump Bridge Planned")
	status.unreal_finish("UCore Unreal Init Complete")
end

local function run_plugin_step(project_root, callback)
	local plugin = install.plugin_status(project_root)
	if plugin.ready then
		finish_step("task:plugin", "NvimSourceCodeAccess Ready")
		return callback()
	end

	status.unreal_step("task:plugin", "Installing NvimSourceCodeAccess...")
	local ok, result = install.install_plugin("project")
	if ok then
		finish_step("task:plugin", "NvimSourceCodeAccess Installed")
	else
		finish_step("task:plugin", "NvimSourceCodeAccess Install Failed")
		vim.notify("UCore Unreal Init: plugin install failed\n" .. tostring(result), vim.log.levels.WARN)
	end

	callback()
end

local function run_nvr_step(callback)
	if install.has_nvr() then
		finish_step("task:nvr", "nvr Ready")
		return callback()
	end

	status.unreal_step("task:nvr", "Installing nvr...")
	install.install_nvr_async(function(ok, result)
		vim.schedule(function()
			if ok then
				finish_step("task:nvr", "nvr Installed")
			else
				finish_step("task:nvr", "nvr Install Failed")
				vim.notify("UCore Unreal Init: nvr install failed\n" .. tostring(result), vim.log.levels.WARN)
			end
			callback()
		end)
	end)
end

function M.run(project_root, callback)
	callback = callback or function() end
	project_root = tostring(project_root or "")
	if project_root == "" then
		return callback()
	end

	if completed[project_root] then
		return callback()
	end

	if running[project_root] then
		return callback()
	end

	local plugin = install.plugin_status(project_root)
	local need_nvr = not install.has_nvr()
	local need_plugin = not plugin.ready

	if not need_nvr and not need_plugin then
		completed[project_root] = true
		return callback()
	end

	running[project_root] = true
	status.unreal_start("Preparing Unreal editor integration...")
	if need_nvr then
		status.unreal_step("task:nvr", "Installing nvr...")
	else
		finish_step("task:nvr", "nvr Ready")
	end

	if need_plugin then
		status.unreal_step("task:plugin", "Installing NvimSourceCodeAccess...")
	else
		finish_step("task:plugin", "NvimSourceCodeAccess Ready")
	end

	status.unreal_step("task:asset_bridge", "Asset Jump Bridge Planned")

	run_nvr_step(function()
		run_plugin_step(project_root, function()
			running[project_root] = nil
			completed[project_root] = true
			finish_panel()
			callback()
		end)
	end)
end

function M.reset()
	running = {}
	completed = {}
end

return M
