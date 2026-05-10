local install = require("ucore.install")
local project = require("ucore.project")
local status = require("ucore.status")

local M = {}

local running = {}
local completed = {}
local plugin_ids = { "nvimsourcecodeaccess", "neovimlink" }

local function finish_step(key, message)
	status.unreal_step_finish(key, message)
end

local function plugin_status(id, project_root)
	if id == "nvimsourcecodeaccess" then
		return install.plugin_status(project_root)
	end
	return install.asset_link_status(project_root)
end

local function plugin_task_key(id)
	local spec = install.resolve_plugin(id)
	return spec and spec.task_key or id
end

local function plugin_display_name(id)
	local spec = install.resolve_plugin(id)
	return spec and spec.display_name or id
end

local function missing_ids(project_root)
	local missing = {}
	for _, id in ipairs(plugin_ids) do
		local item = plugin_status(id, project_root)
		if item.ready then
			finish_step(plugin_task_key(id), plugin_display_name(id) .. " Ready")
		else
			finish_step(plugin_task_key(id), plugin_display_name(id) .. " Missing")
			table.insert(missing, id)
		end
	end
	return missing
end

local function finish_panel(project_root)
	missing_ids(project_root)
	status.unreal_finish("UCore Unreal Init Complete")
end

local function registry_prompt_state(project_root)
	local registry = project.read_registry()
	local item = registry.projects and registry.projects[project_root]
	local prompted = type(item) == "table" and type(item.ucore_unreal_init_prompted) == "table"
		and item.ucore_unreal_init_prompted or {}
	return registry, item or {}, prompted
end

local function was_prompted(project_root, id)
	local _, _, prompted = registry_prompt_state(project_root)
	return prompted[id] == true
end

local function mark_prompted(project_root, ids)
	local registry, item, prompted = registry_prompt_state(project_root)
	for _, id in ipairs(ids) do
		prompted[id] = true
	end
	item.ucore_unreal_init_prompted = prompted
	registry.projects = registry.projects or {}
	registry.projects[project_root] = vim.tbl_deep_extend("force", registry.projects[project_root] or {}, item)
	project.write_registry(registry)
end

local function unprompted_missing(project_root)
	local items = {}
	for _, id in ipairs(missing_ids(project_root)) do
		if not was_prompted(project_root, id) then
			table.insert(items, id)
		end
	end
	return items
end

local function prompt_scope(callback)
	vim.ui.select({
		{ id = "project", label = "Project" },
		{ id = "engine", label = "Engine" },
	}, {
		prompt = "UCore Unreal Init install target",
		format_item = function(item)
			return item.label
		end,
	}, function(choice)
		callback(choice and choice.id or nil)
	end)
end

local function prompt_install(project_root, missing, callback)
	local items = {}
	for _, id in ipairs(missing) do
		local spec = install.resolve_plugin(id)
		if spec then
			table.insert(items, spec.display_name .. " - " .. (spec.description or ""))
		end
	end

	local prompt = "Install missing Unreal integration plugins?\n" .. table.concat(items, "\n")
	vim.ui.select({
		{ id = "install", label = "Install" },
		{ id = "skip", label = "Skip" },
	}, {
		prompt = prompt,
		format_item = function(item)
			return item.label
		end,
	}, function(choice)
		if not choice or choice.id ~= "install" then
			mark_prompted(project_root, missing)
			return callback(false)
		end

		prompt_scope(function(scope)
			if not scope then
				mark_prompted(project_root, missing)
				return callback(false)
			end
			callback(true, scope)
		end)
	end)
end

local function install_missing(project_root, missing, scope, callback)
	local index = 0

	local function step()
		index = index + 1
		if index > #missing then
			mark_prompted(project_root, missing)
			return callback(true)
		end

		local id = missing[index]
		local spec = install.resolve_plugin(id)
		local task_key = plugin_task_key(id)
		status.unreal_step(task_key, "Preparing " .. spec.display_name .. "...")

		local ok, result = install.install_named(id, scope, function(progress)
			if progress.message then
				status.unreal_step(task_key, progress.message)
				return
			end

			status.unreal_step(task_key, string.format(
				"%s %.1f MB / %.1f MB",
				spec.display_name,
				(tonumber(progress.current_bytes or 0) or 0) / 1024 / 1024,
				(tonumber(progress.total_bytes or 0) or 0) / 1024 / 1024
			))
		end)

		if ok then
			finish_step(task_key, spec.display_name .. " Installed")
		else
			finish_step(task_key, spec.display_name .. " Install Failed")
			vim.notify("UCore Unreal Init: plugin install failed\n" .. tostring(result), vim.log.levels.WARN)
		end

		step()
	end

	step()
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

	running[project_root] = true
	status.unreal_start("Preparing Unreal editor integration...")

	local missing = unprompted_missing(project_root)
	if #missing == 0 then
		running[project_root] = nil
		completed[project_root] = true
		finish_panel(project_root)
		return callback()
	end

	prompt_install(project_root, missing, function(accepted, scope)
		if not accepted then
			running[project_root] = nil
			completed[project_root] = true
			finish_panel(project_root)
			return callback()
		end

		install_missing(project_root, missing, scope, function()
			running[project_root] = nil
			completed[project_root] = true
			finish_panel(project_root)
			callback()
		end)
	end)
end

function M.reset()
	running = {}
	completed = {}
end

return M
