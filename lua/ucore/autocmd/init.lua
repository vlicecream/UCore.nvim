local bootstrap = require("ucore.bootstrap")
local config = require("ucore.config")
local project = require("ucore.project")

local M = {}

local group_name = "UCoreAutoBoot"
local booted_projects = {}
local pending_projects = {}

-- Schedule auto boot once per Unreal project root.
-- 对每个 Unreal 工程根目录只调度一次自动启动。
local function schedule_boot(project_root)
	if booted_projects[project_root] or pending_projects[project_root] then
		return
	end

	pending_projects[project_root] = true

	vim.defer_fn(function()
		pending_projects[project_root] = nil

		if booted_projects[project_root] then
			return
		end

		bootstrap.boot(function(ok, err)
			if ok then
				booted_projects[project_root] = true
				return
			end

			vim.notify("UCore auto boot failed:\n" .. tostring(err), vim.log.levels.WARN)
		end, {
			project_root = project_root,
		})
	end, config.values.auto_boot_delay_ms)
end

-- Try to auto boot when the current buffer belongs to an Unreal project.
-- 当当前 buffer 属于 Unreal 工程时尝试自动启动。
local function try_auto_boot()
	if not config.values.auto_boot then
		return
	end

	local buffer_path = vim.api.nvim_buf_get_name(0)
	if buffer_path == "" then
		return
	end

	local project_root = project.find_project_root(buffer_path)
	if not project_root then
		return
	end

	schedule_boot(project_root)
end

-- Register auto boot autocmds.
-- 注册自动启动 autocmd。
function M.setup()
	local group = vim.api.nvim_create_augroup(group_name, { clear = true })

	vim.api.nvim_create_autocmd(config.values.auto_boot_events, {
		group = group,
		callback = try_auto_boot,
	})

	-- Lazy-loaded plugins can miss the initial BufReadPost event, so check the
	-- current buffer once after setup too.
	-- lazy 加载时可能已经错过初始 BufReadPost，所以 setup 后也检查一次当前 buffer。
	vim.defer_fn(try_auto_boot, config.values.auto_boot_delay_ms)
end

-- Clear auto boot state, mostly useful for debugging.
-- 清理自动启动状态，主要用于调试。
function M.reset()
	booted_projects = {}
	pending_projects = {}
end

return M
